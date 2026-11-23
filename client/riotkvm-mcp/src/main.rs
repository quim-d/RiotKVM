//! `riotkvm-mcp`: servidor MCP (stdio, JSON-RPC 2.0) de RiotKVM al estilo "Code Mode" de Cloudflare.
//!
//! En vez de una herramienta por acción, expone dos:
//! - `search`: el modelo escribe JS que filtra el catálogo de documentación (qué es el QKVM, API, recetas).
//! - `execute`: el modelo escribe JS contra la API tipada `qkvm` (teclado, ratón, capturas), que corre en un
//!   sandbox QuickJS sin red ni ficheros; la única salida es el puente `__host` hacia la sesión.
//!
//! stdout es SOLO protocolo; los diagnósticos van a stderr.

mod catalog;
mod sandbox;
mod session;

use serde_json::{json, Value};
use session::Session;
use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT: f64 = 60.0;
const MAX_TIMEOUT: f64 = 600.0;

fn tools() -> Value {
    json!([
        {
            "name": "search",
            "title": "Buscar en la documentación del QKVM",
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
            "title": "Ejecutar código contra el QKVM (teclado, ratón y pantalla del servidor)",
            "description": catalog::EXECUTE_DESC,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "JavaScript con la API `qkvm`. Devuelve con return; las capturas se adjuntan como imágenes." },
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

fn do_search(code: &str) -> Value {
    let prelude = format!("globalThis.catalog = Object.freeze({});\n{}", catalog::CATALOG, catalog::SEARCH_PRELUDE);
    let out = sandbox::run(code, &prelude, Instant::now() + Duration::from_secs(10), |op, _| Err(format!("{op}: en `search` no hay acceso al dongle; usa `execute`")));
    match out.error {
        None => text_result(pretty(&out.result), false),
        Some(e) => text_result(format!("Error: {e}"), true),
    }
}

fn pretty(json: &str) -> String {
    serde_json::from_str::<Value>(json).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| json.to_string())
}

fn do_execute(sess: &Rc<RefCell<Session>>, code: &str, timeout: f64) -> Value {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.clamp(1.0, MAX_TIMEOUT));
    {
        let mut s = sess.borrow_mut();
        s.deadline = deadline;
        s.shots.clear();
        s.logs.clear();
    }
    let host_sess = sess.clone();
    let out = sandbox::run(code, catalog::EXEC_PRELUDE, deadline, move |op, args| {
        let a: Value = serde_json::from_str(args).map_err(|e| format!("argumentos JSON inválidos: {e}"))?;
        let v = host_sess.borrow_mut().call(op, &a)?;
        Ok(v.to_string())
    });
    let mut s = sess.borrow_mut();
    s.release_after_run();
    let mut text = String::new();
    if out.ok {
        text.push_str(&format!("Resultado:\n{}", pretty(&out.result)));
    } else {
        text.push_str(&format!("Error: {}", out.error.clone().unwrap_or_default()));
    }
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

fn handle(sess: &Rc<RefCell<Session>>, method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let proto = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18");
            Ok(json!({
                "protocolVersion": proto,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "qkvm", "title": "QKVM (KVM de bolsillo)", "version": env!("CARGO_PKG_VERSION") },
                "instructions": catalog::INSTRUCTIONS
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let code = args.get("code").and_then(Value::as_str).unwrap_or("");
            if code.trim().is_empty() {
                return Ok(text_result("Error: falta `code`".into(), true));
            }
            match name {
                "search" => Ok(do_search(code)),
                "execute" => {
                    let t = args.get("timeoutSeconds").and_then(Value::as_f64).unwrap_or(DEFAULT_TIMEOUT);
                    Ok(do_execute(sess, code, t))
                }
                other => Err((-32602, format!("herramienta desconocida: {other}"))),
            }
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        other => Err((-32601, format!("método no soportado: {other}"))),
    }
}

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("riotkvm-mcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("riotkvm-mcp: servidor MCP (stdio) de RiotKVM en modo Code Mode (herramientas `search` y `execute`).\n\
                  Variables: DKVM_PORT, DKVM_LAYOUT, DKVM_BLE_TIMEOUT, DKVM_MCP_VIDEO, DKVM_MCP_RES, DKVM_MCP_VIDEO_IDLE.\n\
                  Alta en Claude Code: claude mcp add riotkvm -- /ruta/a/riotkvm-mcp");
        return;
    }
    dkvm_core::rt::timer_resolution_1ms();
    let sess = Rc::new(RefCell::new(Session::new()));
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let r = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("JSON inválido: {e}")}});
                let mut o = stdout.lock();
                let _ = writeln!(o, "{r}");
                let _ = o.flush();
                continue;
            }
        };
        let Some(id) = msg.get("id").cloned() else { continue }; // notificación (initialized, cancelled...)
        let Some(method) = msg.get("method").and_then(Value::as_str) else { continue }; // respuesta a algo nuestro: no pedimos nada
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let resp = match handle(&sess, method, &params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        };
        let mut o = stdout.lock();
        let _ = writeln!(o, "{resp}");
        let _ = o.flush();
    }
    // al cerrar: suelta todo y libera el BLE
    let mut s = sess.borrow_mut();
    s.release_after_run();
    let _ = s.call("disconnect", &json!({}));
}
