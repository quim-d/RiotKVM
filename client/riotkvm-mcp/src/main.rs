//! `riotkvm-mcp`: servidor MCP (stdio, JSON-RPC 2.0) de RiotKVM al estilo "Code Mode" de Cloudflare.
//!
//! En vez de una herramienta por acción, expone dos:
//! - `search`: el modelo escribe JS que filtra el catálogo de documentación (qué es RiotKVM, API, recetas).
//! - `execute`: el modelo escribe JS contra la API tipada `riotkvm` (teclado, ratón, capturas), que corre en un
//!   sandbox QuickJS sin red ni ficheros; la única salida es el puente `__host` hacia la sesión.
//!
//! Un hilo lee stdin y pasa las líneas por un canal; así, mientras corre un `execute`, sigue atendiendo
//! `notifications/cancelled` (marca la petición en curso como cancelada) y, sin peticiones durante un rato, el
//! bucle principal suelta el dongle para la GUI. stdout es SOLO protocolo; los diagnósticos van a stderr.

mod catalog;
mod sandbox;
mod session;

use serde_json::{json, Value};
use session::Session;
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT: f64 = 60.0;
const MAX_TIMEOUT: f64 = 600.0;
/// Tope del texto devuelto al modelo (valor de `execute` o de `search`).
const RESULT_MAX: usize = 100 * 1024;
/// Versiones del protocolo MCP que hablamos (la primera es la preferida).
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// Sin peticiones durante este tiempo se suelta el dongle (BLE) para que la GUI u otra instancia lo usen.
const IDLE_RELEASE: u64 = 60;

fn tools() -> Value {
    json!([
        {
            "name": "search",
            "title": "Buscar en la documentación de RiotKVM",
            "description": catalog::SEARCH_DESC,
            "inputSchema": {
                "type": "object",
                "properties": { "code": { "type": "string", "description": "JavaScript que usa `catalog` y `find()` y devuelve el resultado con return." } },
                "required": ["code"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "execute",
            "title": "Ejecutar código contra RiotKVM (teclado, ratón y pantalla del servidor)",
            "description": catalog::EXECUTE_DESC,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "JavaScript con la API `riotkvm`. Devuelve con return; las capturas se adjuntan como imágenes." },
                    "timeoutSeconds": { "type": "number", "description": "Límite de tiempo de la ejecución (60 por defecto, máx. 600).", "minimum": 1, "maximum": 600 }
                },
                "required": ["code"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// Recorta el texto para el modelo (por límite UTF-8) avisando de cuánto había.
fn clip(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let total = text.len();
    let mut t = session::cut_utf8(&text, max).to_string();
    t.push_str(&format!("\n... (resultado truncado: {} KB de {} KB; devuelve menos datos o fíltralos en el propio código)", max / 1024, total.div_ceil(1024)));
    t
}

fn pretty(json: &str) -> String {
    serde_json::from_str::<Value>(json).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| json.to_string())
}

fn do_search(code: &str, cancel: Arc<AtomicBool>) -> Value {
    let prelude = format!("globalThis.catalog = Object.freeze({});\n{}", catalog::CATALOG, catalog::SEARCH_PRELUDE);
    let out = sandbox::run(code, &prelude, Instant::now() + Duration::from_secs(10), cancel, |op, _| Err(format!("{op}: en `search` no hay acceso al dongle; usa `execute`")));
    match out.error {
        None => text_result(clip(pretty(&out.result), RESULT_MAX), false),
        Some(e) => text_result(clip(format!("Error: {e}"), RESULT_MAX), true),
    }
}

fn do_execute(sess: &Rc<RefCell<Session>>, code: &str, timeout: f64, cancel: Arc<AtomicBool>) -> Value {
    let timeout = if timeout.is_finite() { timeout.clamp(1.0, MAX_TIMEOUT) } else { DEFAULT_TIMEOUT };
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    sess.borrow_mut().begin_run(deadline, cancel.clone());
    let host_sess = sess.clone();
    let out = sandbox::run(code, catalog::EXEC_PRELUDE, deadline, cancel, move |op, args| {
        let a: Value = serde_json::from_str(args).map_err(|e| format!("argumentos JSON inválidos: {e}"))?;
        let v = host_sess.borrow_mut().call(op, &a)?;
        Ok(v.to_string())
    });
    let mut s = sess.borrow_mut();
    s.release_after_run();
    let mut text = if out.ok { format!("Resultado:\n{}", pretty(&out.result)) } else { format!("Error: {}", out.error.clone().unwrap_or_default()) };
    text = clip(text, RESULT_MAX);
    if !s.logs.is_empty() {
        text.push_str("\n\nconsole:\n");
        text.push_str(&s.logs.join("\n"));
    }
    let mut content = vec![json!({ "type": "text", "text": text })];
    for shot in s.shots.drain(..) {
        content.push(json!({ "type": "image", "data": shot.data_b64, "mimeType": shot.mime }));
    }
    json!({ "content": content, "isError": !out.ok })
}

fn negotiate(requested: Option<&str>) -> &'static str {
    requested.and_then(|r| PROTOCOL_VERSIONS.iter().find(|v| **v == r).copied()).unwrap_or(PROTOCOL_VERSIONS[0])
}

fn handle(sess: &Rc<RefCell<Session>>, method: &str, params: &Value, cancel: Arc<AtomicBool>) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiate(params.get("protocolVersion").and_then(Value::as_str)),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "riotkvm", "title": "RiotKVM (KVM de bolsillo)", "version": env!("CARGO_PKG_VERSION") },
            "instructions": catalog::INSTRUCTIONS
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let code = args.get("code").and_then(Value::as_str).unwrap_or("");
            if !matches!(name, "search" | "execute") {
                return Err((-32602, format!("herramienta desconocida: {name}")));
            }
            if code.trim().is_empty() {
                return Ok(text_result("Error: falta `code`".into(), true));
            }
            match name {
                "search" => Ok(do_search(code, cancel)),
                _ => {
                    let t = args.get("timeoutSeconds").and_then(Value::as_f64).unwrap_or(DEFAULT_TIMEOUT);
                    Ok(do_execute(sess, code, t, cancel))
                }
            }
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        other => Err((-32601, format!("método no soportado: {other}"))),
    }
}

fn err(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// Petición en curso (para `notifications/cancelled`) y cancelaciones que llegaron antes de empezarla.
#[derive(Default)]
struct Inflight {
    current: Option<(Value, Arc<AtomicBool>)>,
    early: HashSet<String>,
}

/// Lo que el hilo lector pasa al bucle: una línea (o un error de lectura, p. ej. bytes no UTF-8).
type Line = Result<String, String>;

/// Hilo lector: atiende `notifications/cancelled` al momento y reenvía todo lo demás.
fn spawn_reader(inflight: Arc<Mutex<Inflight>>) -> mpsc::Receiver<Line> {
    let (tx, rx) = mpsc::channel::<Line>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match stdin.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = match String::from_utf8(std::mem::take(&mut buf)) {
                Ok(l) => l,
                Err(e) => {
                    if tx.send(Err(format!("la línea no es UTF-8 válido: {e}"))).is_err() {
                        break;
                    }
                    continue;
                }
            };
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
                    if let Some(rid) = v.get("params").and_then(|p| p.get("requestId")) {
                        let mut g = inflight.lock().unwrap_or_else(|e| e.into_inner());
                        match &g.current {
                            Some((id, flag)) if id == rid => flag.store(true, Ordering::Relaxed),
                            _ => {
                                if g.early.len() > 1000 {
                                    g.early.clear();
                                }
                                g.early.insert(rid.to_string());
                            }
                        }
                    }
                    continue;
                }
            }
            if tx.send(Ok(line)).is_err() {
                break;
            }
        }
    });
    rx
}

/// Procesa una línea y devuelve la respuesta (None = notificación, respuesta o petición cancelada).
fn process(sess: &Rc<RefCell<Session>>, inflight: &Arc<Mutex<Inflight>>, line: Line) -> Option<Value> {
    let line = match line {
        Ok(l) => l,
        Err(e) => return Some(err(Value::Null, -32700, e)),
    };
    if line.trim().is_empty() {
        return None;
    }
    let msg: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(e) => return Some(err(Value::Null, -32700, format!("JSON inválido: {e}"))),
    };
    if msg.is_array() {
        return Some(err(Value::Null, -32600, "los lotes JSON-RPC (arrays) no están soportados: envía las peticiones de una en una"));
    }
    if !msg.is_object() {
        return Some(err(Value::Null, -32600, "petición inválida: se esperaba un objeto JSON-RPC"));
    }
    let method = msg.get("method").and_then(Value::as_str)?; // sin método: respuesta a algo nuestro (no pedimos nada)
    let id = msg.get("id")?.clone(); // sin id: notificación (initialized...)
    if !(id.is_string() || id.is_number()) {
        return Some(err(Value::Null, -32600, "petición inválida: `id` debe ser cadena o número (no null)"));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut g = inflight.lock().unwrap_or_else(|e| e.into_inner());
        if g.early.remove(&id.to_string()) {
            return None; // el cliente la canceló antes de empezar
        }
        g.current = Some((id.clone(), cancel.clone()));
    }
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(sess, method, &params, cancel.clone())));
    inflight.lock().unwrap_or_else(|e| e.into_inner()).current = None;
    let resp = match r {
        Ok(Ok(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Ok(Err((code, message))) => err(id, code, message),
        Err(p) => {
            let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "pánico".into());
            // estado incierto: suelta el dongle (se reconecta en la siguiente llamada)
            if let Ok(mut s) = sess.try_borrow_mut() {
                s.release_after_run();
                s.drop_kvm();
            }
            err(id, -32603, format!("error interno del servidor: {why}"))
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return None; // MCP: no se responde a una petición cancelada
    }
    Some(resp)
}

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("riotkvm-mcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!(
            "riotkvm-mcp: servidor MCP (stdio) de RiotKVM en modo Code Mode (herramientas `search` y `execute`).\n\
             Variables: DKVM_PORT (ble, serial, puerto, ble-mock, mock), DKVM_LAYOUT, DKVM_BLE_TIMEOUT, DKVM_ABS_BUTTONS,\n\
             RIOTKVM_MCP_VIDEO (capture, pattern, none o nombre), RIOTKVM_MCP_RES (1920x1080@30), RIOTKVM_MCP_VIDEO_IDLE (s, 20),\n\
             RIOTKVM_MCP_IDLE (s sin peticiones antes de soltar el dongle, 60). Las DKVM_MCP_* antiguas valen como alias.\n\
             Alta en Claude Code: claude mcp add riotkvm -- /ruta/a/riotkvm-mcp"
        );
        return;
    }
    dkvm_core::rt::timer_resolution_1ms();
    let idle = Duration::from_secs(session::env("IDLE").and_then(|v| v.parse().ok()).unwrap_or(IDLE_RELEASE).max(1));
    let sess = Rc::new(RefCell::new(Session::new()));
    let inflight = Arc::new(Mutex::new(Inflight::default()));
    let rx = spawn_reader(inflight.clone());
    let stdout = std::io::stdout();
    loop {
        let line = match rx.recv_timeout(idle) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) => {
                let mut s = sess.borrow_mut();
                if s.is_connected() {
                    eprintln!("riotkvm-mcp: {} s sin peticiones: suelto el dongle", idle.as_secs());
                    s.drop_kvm();
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(resp) = process(&sess, &inflight, line) {
            let mut o = stdout.lock();
            let _ = writeln!(o, "{resp}");
            let _ = o.flush();
        }
    }
    // al cerrar: suelta todo y libera el BLE (sin pasar por `call`, que comprobaría el plazo)
    let mut s = sess.borrow_mut();
    s.release_after_run();
    s.drop_kvm();
}

#[cfg(test)]
mod tests;
