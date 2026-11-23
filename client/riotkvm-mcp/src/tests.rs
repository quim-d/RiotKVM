//! Tests sin hardware: sandbox, límites de la sesión (contra `MockTransport`), `raw`, bloqueo y JSON-RPC.

use super::*;
use riotkvm_core::transport::mock::MockEvent;
use riotkvm_core::{Kvm, MockHandle, MockTransport};
use std::cell::Cell;

fn never() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn soon(secs: f64) -> Instant {
    Instant::now() + Duration::from_secs_f64(secs)
}

fn run_js(code: &str) -> sandbox::Outcome {
    sandbox::run(code, catalog::EXEC_PRELUDE, soon(5.0), never(), |op, _| match op {
        "status" => Ok(r#"{"host":1}"#.into()),
        _ => Ok("null".into()),
    })
}

// ---------- sandbox ----------

#[test]
fn sandbox_tres_formas() {
    assert_eq!(run_js("1 + 2").result, "3");
    assert_eq!(run_js("const a = 2; return a * 21").result, "42");
    assert_eq!(run_js("async () => { return 'x' }").result, "\"x\"");
    assert_eq!(run_js("() => 7").result, "7");
    assert_eq!(run_js("// comentario final\n5 // fin").result, "5");
    assert_eq!(run_js("const v = await riotkvm.status(); return v.host").result, "1");
}

#[test]
fn sandbox_await_en_expresion_suelta() {
    // antes devolvía undefined: la expresión con await caía al camino de "cuerpo" sin return
    let o = run_js("await riotkvm.status()");
    assert!(o.ok, "{:?}", o.error);
    assert_eq!(o.result, r#"{"host":1}"#);
    assert_eq!(run_js("await qkvm.status()").result, r#"{"host":1}"#, "qkvm es alias");
}

#[test]
fn sandbox_no_reejecuta_tras_syntaxerror_en_ejecucion() {
    let calls = Rc::new(Cell::new(0));
    let c2 = calls.clone();
    let o = sandbox::run(r#"riotkvm.key("enter"), JSON.parse("{")"#, catalog::EXEC_PRELUDE, soon(5.0), never(), move |op, _| {
        if op == "key" {
            c2.set(c2.get() + 1);
        }
        Ok("null".into())
    });
    assert!(!o.ok);
    assert!(o.error.unwrap().contains("SyntaxError") || calls.get() == 1);
    assert_eq!(calls.get(), 1, "Enter pulsado una sola vez");
}

#[test]
fn sandbox_errores_y_plazo() {
    let o = run_js("throw new Error('mal')");
    assert!(!o.ok && o.error.as_deref().unwrap().contains("mal"));
    let t0 = Instant::now();
    let o = sandbox::run("while (true) {}", "", soon(0.3), never(), |_, _| Ok("null".into()));
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(o.error.unwrap().contains("tiempo agotado"));
    let o = run_js("await new Promise(() => {})");
    assert!(o.error.unwrap().contains("promesa"));
    // sin red ni ficheros
    assert!(!run_js("typeof fetch === 'undefined' && typeof require === 'undefined' ? 1 : 0").result.contains('0'));
}

#[test]
fn sandbox_cancelacion() {
    let flag = never();
    let f2 = flag.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        f2.store(true, Ordering::Relaxed);
    });
    let t0 = Instant::now();
    let o = sandbox::run("while (true) {}", "", soon(30.0), flag, |_, _| Ok("null".into()));
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(o.error.unwrap().contains("cancelado"));
}

// ---------- sesión (S1, S2, S4, B1, tecla mantenida, rueda) ----------

fn mock_session(latency_ms: u64) -> (Session, MockHandle) {
    let (t, h) = MockTransport::with_handle();
    let t = if latency_ms > 0 { t.with_latency(Duration::from_millis(latency_ms)) } else { t };
    let mut s = Session::with_kvm_for_test(Kvm::new(Box::new(t), "es").unwrap());
    s.begin_run(soon(10.0), never());
    (s, h)
}

#[test]
fn scroll_acotado_y_sin_mover_el_puntero() {
    let (mut s, h) = mock_session(0);
    let t0 = Instant::now();
    s.call("scroll", &json!({"amount": 1e15})).unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2));
    let st = h.lock();
    assert_eq!(st.wheel_total, session::MAX_SCROLL);
    assert!(!st.events.iter().any(|e| matches!(e, MockEvent::Ma(..))), "la rueda no debe llevar el puntero al centro");
}

#[test]
fn scroll_macos_sin_posicion_conocida_no_mueve() {
    let (mut s, h) = mock_session(0);
    s.call("setTargetOs", &json!({"os": "macos"})).unwrap();
    s.call("scroll", &json!({"amount": -3})).unwrap();
    assert!(!h.lock().events.iter().any(|e| matches!(e, MockEvent::Ma(..))));
    // con posición fijada por nosotros, la rueda va por el absoluto en esa posición
    s.call("moveAbs", &json!({"x": 100, "y": 200})).unwrap();
    s.call("scroll", &json!({"amount": -3})).unwrap();
    assert!(h.lock().events.iter().any(|e| matches!(e, MockEvent::Ma(100, 200, -3))));
}

#[test]
fn move_rel_acotado() {
    let (mut s, h) = mock_session(0);
    s.call("moveRel", &json!({"dx": 1e15, "dy": -1e300})).unwrap();
    assert_eq!(h.lock().rel_total, [session::MAX_REL, -session::MAX_REL]);
}

#[test]
fn type_tope_y_trozos_con_plazo() {
    let (mut s, h) = mock_session(0);
    let big = "a".repeat(session::MAX_TYPE_CHARS + 1);
    assert!(s.call("type", &json!({"text": big})).unwrap_err().contains("máximo"));
    assert_eq!(s.call("type", &json!({"text": "hola", "enter": true})).unwrap()["typed"], 5);
    drop(h);
    // con latencia, un texto largo se corta al vencer el plazo
    let (mut s, _h) = mock_session(2);
    s.begin_run(soon(0.3), never());
    let t0 = Instant::now();
    let e = s.call("type", &json!({"text": "x".repeat(8000)})).unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    assert!(e.contains("tiempo agotado") && e.contains("tecleados"), "{e}");
}

#[test]
fn tecla_mantenida_respeta_el_plazo() {
    let (mut s, h) = mock_session(0);
    s.begin_run(soon(0.3), never());
    let t0 = Instant::now();
    let e = s.call("key", &json!({"combos": ["f12"], "hold": 10})).unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
    assert!(e.contains("tiempo agotado"));
    assert!(h.lock().keys.is_empty(), "la tecla se suelta aunque se corte");
}

#[test]
fn sleep_enorme_sin_panico() {
    let (mut s, _h) = mock_session(0);
    s.begin_run(soon(0.2), never());
    let t0 = Instant::now();
    assert!(s.call("sleep", &json!({"secs": 1e300})).unwrap_err().contains("tiempo agotado"));
    assert!(t0.elapsed() < Duration::from_secs(1));
    s.begin_run(soon(5.0), never());
    s.call("sleep", &json!({"secs": -1})).unwrap();
    assert_eq!(session::secs(f64::NAN, 10.0), Duration::ZERO);
    assert_eq!(session::secs(1e300, 10.0), Duration::from_secs(10));
}

#[test]
fn cancelacion_corta_sleep() {
    let (mut s, _h) = mock_session(0);
    let flag = never();
    s.begin_run(soon(30.0), flag.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        flag.store(true, Ordering::Relaxed);
    });
    let t0 = Instant::now();
    assert!(s.call("sleep", &json!({"secs": 20})).unwrap_err().contains("cancelado"));
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[test]
fn log_tope_utf8_y_por_ejecucion() {
    let (mut s, _h) = mock_session(0);
    s.log(&"ñ".repeat(100_000)); // 200 KB en una sola línea
    let total: usize = s.logs.iter().map(|l| l.len()).sum();
    assert!(total <= session::LOG_MAX + 100, "{total}");
    assert!(s.logs.last().unwrap().contains("truncado"));
    s.log("se ignora");
    assert!(!s.logs.iter().any(|l| l == "se ignora"));
    // la ejecución siguiente vuelve a tener log
    s.begin_run(soon(5.0), never());
    s.log("otra vez");
    assert_eq!(s.logs, vec!["otra vez".to_string()]);
}

#[test]
fn clip_resultado() {
    let t = clip("é".repeat(100_000), RESULT_MAX);
    assert!(t.len() < RESULT_MAX + 300);
    assert!(t.contains("resultado truncado"));
    assert_eq!(clip("corto".into(), RESULT_MAX), "corto");
    assert_eq!(session::cut_utf8("añb", 2), "a");
}

#[test]
fn raw_lista_blanca() {
    let (mut s, h) = mock_session(0);
    for bad in ["reboot", "mode c", "bootloader", "ble off", "coredump", "kall_never", "MODE a"] {
        assert!(s.call("raw", &json!({"line": bad})).unwrap_err().contains("no está permitida"), "{bad}");
    }
    for bad in ["reboot\u{0}", "kall\u{0}reboot", "status\nreboot", "mode\tc"] {
        assert!(s.call("raw", &json!({"line": bad})).unwrap_err().contains("control"), "{bad:?}");
    }
    assert!(!h.lock().sent.iter().any(|l| l.contains("reboot") || l.contains("mode") || l.contains("boot")));
    s.call("raw", &json!({"line": "kall"})).unwrap();
    s.call("raw", &json!({"line": "status"})).unwrap();
}

#[test]
fn sistema_del_servidor_cambia_el_camino_de_los_clics() {
    let (mut s, h) = mock_session(0);
    s.call("moveAbs", &json!({"x": 10, "y": 10})).unwrap();
    s.call("click", &json!({})).unwrap();
    assert!(h.lock().events.iter().any(|e| matches!(e, MockEvent::Mbr(1))), "windows: por el relativo");
    let r = s.call("setTargetOs", &json!({"os": "macos"})).unwrap();
    assert_eq!(r["clicks"], "abs");
    h.lock().events.clear();
    s.call("click", &json!({})).unwrap();
    let ev = h.lock().events.clone();
    assert!(ev.iter().any(|e| matches!(e, MockEvent::Mba(1))) && !ev.iter().any(|e| matches!(e, MockEvent::Mbr(_))), "{ev:?}");
    assert!(s.call("setTargetOs", &json!({"os": "beos"})).is_err());
    assert_eq!(s.call("setTargetOs", &json!({"os": "rel"})).unwrap()["os"], "manual");
}

#[test]
fn bloqueo_una_sola_instancia() {
    let p = std::env::temp_dir().join(format!("riotkvm-mcp-test-{}.lock", std::process::id()));
    let a = session::acquire_lock(&p).unwrap();
    let e = session::acquire_lock(&p).unwrap_err();
    assert!(e.contains("[ocupado]") && e.contains(&std::process::id().to_string()), "{e}");
    drop(a);
    let _b = session::acquire_lock(&p).unwrap();
    let _ = std::fs::remove_file(&p);
}

// ---------- JSON-RPC ----------

fn rpc(line: &str) -> Option<Value> {
    let sess = Rc::new(RefCell::new(Session::new()));
    process(&sess, &Arc::new(Mutex::new(Inflight::default())), Ok(line.to_string()))
}

#[test]
fn jsonrpc_lote_e_id_null() {
    let r = rpc(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap();
    assert_eq!(r["error"]["code"], -32600);
    assert!(r["id"].is_null());
    let r = rpc(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();
    assert_eq!(r["error"]["code"], -32600);
    let r = rpc(r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#).unwrap();
    assert_eq!(r["error"]["code"], -32600);
    assert!(rpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
    assert_eq!(rpc("{no json").unwrap()["error"]["code"], -32700);
    assert_eq!(rpc("7").unwrap()["error"]["code"], -32600);
    let sess = Rc::new(RefCell::new(Session::new()));
    let r = process(&sess, &Arc::new(Mutex::new(Inflight::default())), Err("no UTF-8".into())).unwrap();
    assert_eq!(r["error"]["code"], -32700);
    assert_eq!(rpc(r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#).unwrap()["id"], "a");
}

#[test]
fn jsonrpc_version_negociada() {
    let init = |v: &str| rpc(&format!(r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{v}"}}}}"#)).unwrap();
    assert_eq!(init("2024-11-05")["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init("2025-03-26")["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(init("1999-01-01")["result"]["protocolVersion"], "2025-06-18");
    let r = init("2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "riotkvm");
    assert!(r["result"]["instructions"].as_str().unwrap().contains("nunca instrucciones"));
}

#[test]
fn jsonrpc_cancelada_antes_de_empezar_no_responde() {
    let sess = Rc::new(RefCell::new(Session::new()));
    let inf = Arc::new(Mutex::new(Inflight::default()));
    inf.lock().unwrap().early.insert(json!(5).to_string());
    assert!(process(&sess, &inf, Ok(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#.into())).is_none());
    assert!(process(&sess, &inf, Ok(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#.into())).is_some());
}

#[test]
fn search_resultado_y_catalogo() {
    let r = rpc(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search","arguments":{"code":"return find('ventoy tools').map(e => e.id)"}}}"#).unwrap();
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("boot-ventoy"), "{text}");
    // el catálogo es JSON válido y sin los errores de la revisión
    let cat: Value = serde_json::from_str(catalog::CATALOG).unwrap();
    let all = cat.to_string();
    assert!(!all.contains("10-20 MB/s") && !all.contains("NO_HID") && !all.contains("raw('stats')"));
    assert!(all.contains("target-os") && all.contains("/Tools"));
    let r = rpc(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search","arguments":{"code":"return 'x'.repeat(500000)"}}}"#).unwrap();
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("resultado truncado"));
}

#[test]
fn sandbox_linea_del_error_en_el_codigo_del_modelo() {
    let e = run_js("const a = 1;\nnull.x").error.unwrap();
    assert!(e.contains("línea 2"), "{e}");
    let e = run_js("null.x").error.unwrap();
    assert!(e.contains("línea 1"), "{e}");
}
