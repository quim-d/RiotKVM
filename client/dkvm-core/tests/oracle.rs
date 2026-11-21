//! Paridad con la implementación Python de referencia: mismas entradas -> mismas tramas/códigos.
//! Vectores generados por `client/python/tools/gen_oracle.py` -> `client/testdata/oracle.json`.

use dkvm_core::input::{abs_from_pixels, Kvm};
use dkvm_core::keys::parse_combo;
use dkvm_core::layouts::get_layout;
use dkvm_core::protocol as P;
use dkvm_core::script::{run_script, shlex_split, Context};
use dkvm_core::{Error, MockHandle, MockTransport, Result};
use serde_json::Value;
use std::sync::OnceLock;

fn oracle() -> &'static Value {
    static O: OnceLock<Value> = OnceLock::new();
    O.get_or_init(|| {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../testdata/oracle.json");
        let s = std::fs::read_to_string(p).expect("falta testdata/oracle.json (cd client/python && python tools/gen_oracle.py)");
        serde_json::from_str(&s).unwrap()
    })
}

fn mk(layout: &str) -> (Kvm, MockHandle) {
    let (t, h) = MockTransport::with_handle();
    let mut k = Kvm::new(Box::new(t), layout).unwrap();
    k.set_sleep(|_| {});
    (k, h)
}

fn sent(h: &MockHandle) -> Value {
    Value::from(h.lock().sent.clone())
}

fn strokes_json(s: &[(u16, u8)]) -> Value {
    Value::from(s.iter().map(|(u, m)| Value::from(vec![*u as u64, *m as u64])).collect::<Vec<_>>())
}

/// Compara un resultado Rust con el esperado de Python: `{"error": N}` o el valor.
fn check<T>(what: &str, got: Result<T>, want: &Value, ok: impl Fn(T) -> Value) {
    match (got, want.get("error").and_then(Value::as_i64)) {
        (Err(e), Some(code)) => assert_eq!(e.exit_code() as i64, code, "{what}: código de salida ({e})"),
        (Err(e), None) => panic!("{what}: Rust falló ({e}) pero Python no"),
        (Ok(_), Some(code)) => panic!("{what}: Rust no falló pero Python dio error {code}"),
        (Ok(v), None) => assert_eq!(&ok(v), want, "{what}"),
    }
}

#[test]
fn strokes_match() {
    let mut n = 0;
    for (name, chars) in oracle()["strokes"].as_object().unwrap() {
        let lay = get_layout(name).unwrap();
        for (ch, want) in chars.as_object().unwrap() {
            let c = ch.chars().next().unwrap();
            let want = if want.get("error").is_some() { want.clone() } else { want["s"].clone() };
            check(&format!("{name} {ch:?}"), lay.strokes(c), &want, |s| strokes_json(&s));
            n += 1;
        }
    }
    assert!(n > 700, "{n}");
}

#[test]
fn combos_match() {
    for c in oracle()["combos"].as_array().unwrap() {
        let lay = get_layout(c["layout"].as_str().unwrap()).unwrap();
        let combo = c["combo"].as_str().unwrap();
        let r = &c["r"];
        let want = if r.get("error").is_some() { r.clone() } else { r["s"].clone() };
        check(&format!("combo {combo:?}"), parse_combo(combo, lay), &want, |s| strokes_json(&s));
    }
}

#[test]
fn type_text_match() {
    let arr = oracle()["type"].as_array().unwrap();
    assert!(arr.len() > 600);
    for c in arr {
        let (mut k, h) = mk(c["layout"].as_str().unwrap());
        let text = c["text"].as_str().unwrap();
        let r = &c["r"];
        let want = if r.get("error").is_some() { r.clone() } else { r["sent"].clone() };
        check(&format!("type {text:?}"), k.type_text(text, 0.0), &want, |_| sent(&h));
    }
}

#[test]
fn type_fw_match() {
    for c in oracle()["type_fw"].as_array().unwrap() {
        let (mut k, h) = mk("es");
        let text = c["text"].as_str().unwrap();
        let r = &c["r"];
        let want = if r.get("error").is_some() { r.clone() } else { r["sent"].clone() };
        check(&format!("type_fw {text:?}"), k.type_text_firmware(text), &want, |_| sent(&h));
    }
}

#[test]
fn press_match() {
    for c in oracle()["press"].as_array().unwrap() {
        let (mut k, h) = mk("es");
        let combo = c["combo"].as_str().unwrap();
        let r = &c["r"];
        let want = if r.get("error").is_some() { r.clone() } else { r["sent"].clone() };
        let got = match c["fn"].as_str().unwrap() {
            "press" => k.press(combo, 0.0),
            "key_down_name" => k.key_down_name(combo),
            _ => k.key_up_name(combo),
        };
        check(&format!("{} {combo:?}", c["fn"]), got, &want, |_| sent(&h));
    }
}

#[test]
fn mouse_match() {
    for c in oracle()["mouse"].as_array().unwrap() {
        let (mut k, h) = mk("es");
        let a = c["args"].as_array().unwrap();
        let r = &c["r"];
        let want = if r.get("error").is_some() { r.clone() } else { r["sent"].clone() };
        let got = match c["op"].as_str().unwrap() {
            "move_rel" => k.move_rel(a[0].as_i64().unwrap(), a[1].as_i64().unwrap(), a[2].as_i64().unwrap()),
            "scroll" => {
                k.move_abs(100, 200).unwrap();
                k.scroll(a[0].as_i64().unwrap(), a[1].as_bool().unwrap())
            }
            "click" => k.click(a[0].as_str().unwrap(), a[1].as_i64().unwrap(), a[2].as_bool().unwrap(), 0.0),
            op => panic!("{op}"),
        };
        check(&format!("mouse {c}"), got, &want, |_| sent(&h));
    }
    let (mut k, h) = mk("es");
    k.button_down("left", false).unwrap();
    k.button_down("middle", false).unwrap();
    k.button_up("left", false).unwrap();
    assert_eq!(sent(&h), oracle()["buttons_seq"]["sent"]);
    assert_eq!(k.buttons as i64, oracle()["buttons_seq"]["buttons"].as_i64().unwrap());
}

#[test]
fn abs_px_match() {
    for c in oracle()["abs_px"].as_array().unwrap() {
        let a = c["a"].as_array().unwrap();
        let got = abs_from_pixels(a[0].as_f64().unwrap(), a[1].as_f64().unwrap(), a[2].as_i64().unwrap(), a[3].as_i64().unwrap());
        let want = if c["r"].get("error").is_some() { c["r"].clone() } else { c["r"]["xy"].clone() };
        check(&format!("abs_px {a:?}"), got, &want, |(x, y)| Value::from(vec![x, y]));
    }
}

#[test]
fn protocol_match() {
    for c in oracle()["protocol"].as_array().unwrap() {
        let a = c["args"].as_array().unwrap();
        let i = |n: usize| a[n].as_i64().unwrap();
        let got: Result<String> = match c["fn"].as_str().unwrap() {
            "key_down" => P::key_down(i(0)),
            "key_up" => P::key_up(i(0)),
            "mouse_rel" => P::mouse_rel(i(0), i(1), i(2)),
            "mouse_abs" => P::mouse_abs(i(0), i(1), i(2)),
            "buttons_rel" => P::buttons_rel(i(0)),
            "buttons_abs" => P::buttons_abs(i(0)),
            "type_text" => P::type_text(a[0].as_str().unwrap()),
            f => panic!("{f}"),
        };
        let r = &c["r"];
        let want = if r.get("error").is_some() { r.clone() } else { r["s"].clone() };
        check(&format!("protocol {c}"), got, &want, Value::from);
    }
}

#[test]
fn status_match() {
    for c in oracle()["status"].as_array().unwrap() {
        let got = P::parse_status(c["in"].as_str().unwrap());
        match (&c["out"], got) {
            (Value::Null, r) => assert!(r.is_err(), "{c}"),
            (want, Ok(m)) => assert_eq!(&Value::Object(m), want, "{c}"),
            (_, Err(e)) => panic!("{c}: {e}"),
        }
    }
}

#[test]
fn shlex_match() {
    for c in oracle()["shlex"].as_array().unwrap() {
        let got = shlex_split(c["in"].as_str().unwrap());
        match (&c["out"], got) {
            (Value::Null, r) => assert_eq!(r.unwrap_err(), c["err"].as_str().unwrap(), "{c}"),
            (want, Ok(v)) => assert_eq!(&Value::from(v), want, "{c}"),
            (_, Err(e)) => panic!("{c}: {e}"),
        }
    }
}

#[test]
fn scripts_match() {
    for c in oracle()["scripts"].as_array().unwrap() {
        let (mut k, h) = mk("es");
        let text = c["text"].as_str().unwrap();
        let r = &c["r"];
        let mut ctx = Context::new(&mut k);
        let res = run_script(&mut ctx, text, None);
        let results: Vec<Value> = ctx.results.iter().map(|(c, v)| Value::from(vec![Value::from(c.clone()), v.clone()])).collect();
        match (&res, r.get("error").and_then(Value::as_i64)) {
            (Err(e), Some(code)) => assert_eq!(e.exit_code() as i64, code, "script {text:?}: {e}"),
            (Err(e), None) => panic!("script {text:?}: Rust falló ({e}), Python no"),
            (Ok(_), Some(_)) => panic!("script {text:?}: Rust no falló, Python sí: {r}"),
            (Ok(_), None) => assert_eq!(Value::from(results), r["results"], "script {text:?}"),
        }
        assert_eq!(sent(&h), r["sent"], "script {text:?} (tramas enviadas)");
    }
}

#[test]
fn error_type_is_clone_eq() {
    // El mismo error se puede reenviar a varios flushers (IoTransport)
    let e = Error::Usage("x".into());
    assert_eq!(e.clone(), e);
}
