//! Sandbox de JavaScript (QuickJS vía rquickjs): sin red, sin ficheros, sin procesos, sin temporizadores
//! reales. Lo único que el código puede tocar es la función `__host(op, json)` que el prelude envuelve
//! como la API `qkvm`. Límite de memoria y corte por tiempo (interrupt handler).

use rquickjs::{CaughtError, Context, Function, Promise, Runtime, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub struct Outcome {
    pub ok: bool,
    /// Valor devuelto, serializado como JSON (o `undefined`).
    pub result: String,
    pub error: Option<String>,
}

/// `host(op, args_json) -> Ok(json) | Err(mensaje)`. Se corta al pasar `deadline` o al activarse `cancel`.
pub fn run(code: &str, prelude: &str, deadline: Instant, cancel: Arc<AtomicBool>, host: impl Fn(&str, &str) -> Result<String, String> + 'static) -> Outcome {
    let fail = |e: String| Outcome { ok: false, result: "undefined".into(), error: Some(e) };
    let rt = match Runtime::new() {
        Ok(r) => r,
        Err(e) => return fail(format!("no se pudo crear el runtime JS: {e}")),
    };
    rt.set_memory_limit(64 * 1024 * 1024);
    rt.set_max_stack_size(1024 * 1024);
    let stop = cancel.clone();
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline || stop.load(Ordering::Relaxed))));
    let nice = |m: String| nice(m, deadline, &cancel);
    let ctx = match Context::full(&rt) {
        Ok(c) => c,
        Err(e) => return fail(format!("no se pudo crear el contexto JS: {e}")),
    };
    ctx.with(|ctx| {
        let hostfn = Function::new(ctx.clone(), move |op: String, args: String| -> String {
            match host(&op, &args) {
                Ok(v) => format!("{{\"ok\":true,\"v\":{v}}}"),
                Err(e) => serde_json::json!({ "ok": false, "e": e }).to_string(),
            }
        });
        let hostfn = match hostfn {
            Ok(f) => f,
            Err(e) => return fail(e.to_string()),
        };
        if let Err(e) = ctx.globals().set("__host", hostfn) {
            return fail(e.to_string());
        }
        if let Err(e) = ctx.eval::<(), _>(prelude) {
            return fail(format!("prelude: {}", CaughtError::from_error(&ctx, e)));
        }
        // Se admiten tres formas: una función (estilo Cloudflare, `async () => { ... }`), que se llama;
        // una expresión suelta (`riotkvm.status()`, también con `await`), cuyo valor se devuelve; o un cuerpo con
        // `await`/`return`. Se COMPILA primero (AsyncFunction solo analiza, no ejecuta) y se ejecuta una sola vez:
        // un SyntaxError en tiempo de ejecución (JSON.parse) no debe provocar un segundo intento.
        let quoted = serde_json::to_string(code.trim()).unwrap_or_default();
        let src = format!(
            r#"(async () => {{
  const __src = {quoted};
  const __AF = (async function () {{}}).constructor;
  let __f;
  try {{ __f = new __AF("return (" + __src + "\n);"); }}
  catch (e) {{ if (!(e instanceof SyntaxError)) throw e; __f = new __AF(__src); }}
  let __v = await __f();
  if (typeof __v === 'function') __v = await __v();
  return __v;
}})()"#
        );
        let promise: Promise = match ctx.eval(src) {
            Ok(p) => p,
            Err(e) => return fail(nice(CaughtError::from_error(&ctx, e).to_string())),
        };
        match promise.finish::<Value>() {
            Ok(v) => {
                let json = if v.is_undefined() {
                    "undefined".to_string()
                } else {
                    match ctx.json_stringify(v) {
                        Ok(Some(s)) => s.to_string().unwrap_or_else(|_| "\"(no serializable)\"".into()),
                        Ok(None) => "undefined".into(),
                        Err(e) => format!("\"(no serializable: {})\"", CaughtError::from_error(&ctx, e)),
                    }
                };
                Outcome { ok: true, result: json, error: None }
            }
            Err(rquickjs::Error::WouldBlock) => fail(
                "el código quedó esperando una promesa que nunca se resuelve (en el sandbox no hay red ni temporizadores reales; usa await qkvm.sleep(s))".into(),
            ),
            Err(e) => fail(nice(CaughtError::from_error(&ctx, e).to_string())),
        }
    })
}

/// Primera línea del error (sin el "Error: " de JS) + solo las líneas de la traza que apuntan al código del
/// modelo (`<input>`), no al prelude.
fn nice(msg: String, deadline: Instant, cancel: &AtomicBool) -> String {
    if cancel.load(Ordering::Relaxed) && (msg.contains("interrupted") || msg.contains("cancelado")) {
        return "cancelado por el cliente (notifications/cancelled)".into();
    }
    if Instant::now() >= deadline && (msg.contains("interrupted") || msg.contains("tiempo agotado")) {
        return "tiempo agotado (timeoutSeconds): se cortó la ejecución".into();
    }
    let mut lines = msg.lines();
    let first = lines.next().unwrap_or("").trim();
    let first = first.strip_prefix("Error: ").unwrap_or(first);
    // `<input>:L:C` cuenta las 2 líneas de cabecera que añade AsyncFunction: se dan en líneas del código del modelo
    let mut at: Vec<String> = lines.filter_map(|l| input_line(l).map(|n| format!("línea {n}"))).collect();
    at.dedup();
    if at.is_empty() {
        first.to_string()
    } else {
        format!("{first} ({})", at.join(", "))
    }
}

fn input_line(l: &str) -> Option<usize> {
    let rest = &l[l.find("<input>:")? + "<input>:".len()..];
    let n: usize = rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    Some(n.saturating_sub(2).max(1))
}
