//! Port de las pruebas Python de la CLI: en proceso, sin hardware ni procesos externos.

use riotkvm_cli::{run, Env};
use riotkvm_core::transport::PortInfo;
use riotkvm_core::{MockHandle, MockTransport};
use serde_json::Value;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);
impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Buf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn env_with(stdin: &str, h: Option<&MockHandle>, ports: Vec<PortInfo>) -> (Env, Buf, Buf) {
    let (o, e) = (Buf::default(), Buf::default());
    let factory: Option<riotkvm_cli::TransportFactory> = h.map(|h| {
        let h = h.clone();
        Box::new(move || Ok(Box::new(MockTransport::sharing(&h)) as Box<dyn riotkvm_core::Transport>)) as riotkvm_cli::TransportFactory
    });
    let env = Env {
        stdin: Box::new(std::io::Cursor::new(stdin.as_bytes().to_vec())),
        stdout: Arc::new(Mutex::new(Box::new(o.clone()))),
        stderr: Arc::new(Mutex::new(Box::new(e.clone()))),
        transport_factory: factory,
        list_ports: Box::new(move || ports.clone()),
    };
    (env, o, e)
}

fn invoke_full(args: &[&str], stdin: &str, h: Option<&MockHandle>, ports: Vec<PortInfo>) -> Out {
    let (mut env, o, e) = env_with(stdin, h, ports);
    let mut a = vec!["riotkvm"];
    a.extend_from_slice(args);
    let code = run(a, &mut env);
    Out { code, stdout: o.text(), stderr: e.text() }
}

/// Como el `invoke` de Python: `-p mock` pero con el mock inyectado para inspeccionarlo.
fn invoke(args: &[&str], stdin: &str) -> (Out, MockHandle) {
    let (_, h) = MockTransport::with_handle();
    let o = invoke_full(args, stdin, Some(&h), vec![]);
    (o, h)
}

fn json_last(s: &str) -> Value {
    serde_json::from_str(s.trim().lines().last().unwrap()).unwrap()
}

#[test]
fn status_json() {
    let (r, _) = invoke(&["-p", "mock", "--json", "status"], "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = json_last(&r.stdout);
    assert_eq!(d["ok"], true);
    assert_eq!(d["hidready"], true);
}

#[test]
fn status_text() {
    let (r, _) = invoke(&["-p", "mock", "status"], "");
    assert_eq!(r.stdout.trim(), "host=1 leds=0 hidready=1");
}

#[test]
fn type_stdin() {
    let (r, h) = invoke(&["-p", "mock", "-l", "us", "type"], "ab");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(h.lock().kd_usages(), [0x04, 0x05]);
}

#[test]
fn type_enter_and_fw() {
    let (r, h) = invoke(&["-p", "mock", "type", "--fw", "--enter", "hola"], "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    let st = h.lock();
    assert_eq!(st.typed, ["hola"]);
    assert_eq!(st.kd_usages(), [0x28]); // un solo Enter (el '\n' final termina la línea)
}

#[test]
fn key() {
    let (r, h) = invoke(&["-p", "mock", "key", "ctrl+alt+del", "enter"], "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(h.lock().kd_usages(), [0xE0, 0xE2, 0x4C, 0x28]);
}

#[test]
fn mouse_and_click() {
    let (r, h) = invoke(&["-p", "mock", "--screen", "1920x1080", "click", "--at", "1919", "1079", "-n", "2"], "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(h.lock().abs_pos, (32767, 32767));
    assert_eq!(h.lock().sent.iter().filter(|l| *l == "mba 1").count(), 2);
    let (r, h) = invoke(&["-p", "mock", "mouse", "--rel", "200", "-5"], "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(h.lock().rel_total, [200, -5]);
}

#[test]
fn exit_codes() {
    let (r, _) = invoke(&["-p", "mock", "--json", "key", "ctrl+nada"], "");
    assert_eq!(r.code, 2);
    let d = json_last(&r.stdout);
    assert_eq!((d["ok"].clone(), d["code"].clone()), (Value::Bool(false), Value::from(2)));
    assert_eq!(invoke(&["-p", "mock", "raw", "nope"], "").0.code, 4);
    assert_eq!(invoke(&["-p", "mock", "mouse", "99999", "0"], "").0.code, 2);
    assert_eq!(invoke(&["-p", "mock", "--layout", "xx", "type", "a"], "").0.code, 2);
    assert_eq!(invoke(&["-p", "mock", "--screen", "foo", "status"], "").0.code, 2);
    // uso incorrecto de clap -> 2, --help -> 0
    assert_eq!(invoke(&["-p", "mock", "frobnicate"], "").0.code, 2);
    assert_eq!(invoke(&["--help"], "").0.code, 0);
}

fn dongle(port: &str) -> PortInfo {
    PortInfo {
        port: port.into(),
        vid: Some(0x1209),
        pid: Some(1),
        description: "x".into(),
        serial_number: Some("1".into()),
        is_dongle: true,
    }
}

#[test]
fn no_dongle_exit_3() {
    // sin --port ni dongle: el transporte real falla con 3 (sin tocar el hardware si no hay dongle;
    // si hubiera uno enchufado esta prueba no se ejecuta con él: se fuerza un puerto inexistente)
    let r = invoke_full(&["-p", "NO_EXISTE_COM999", "status"], "", None, vec![]);
    assert_eq!(r.code, 3, "{}", r.stderr);
    let r = invoke_full(&["--json", "scan"], "", None, vec![]);
    assert_eq!(r.code, 3);
}

#[test]
fn scan_finds() {
    let r = invoke_full(&["--json", "scan"], "", None, vec![dongle("COM9")]);
    assert_eq!(r.code, 0);
    assert_eq!(json_last(&r.stdout)["ports"][0]["port"], "COM9");
    let r = invoke_full(&["scan"], "", None, vec![dongle("COM9")]);
    assert!(r.stdout.starts_with("* COM9"), "{}", r.stdout);
}

#[test]
fn run_script_file_and_dry_run() {
    let dir = std::env::temp_dir().join(format!("riotkvm-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("m.riotkvm");
    std::fs::write(&f, "layout us\ntype \"a\"\nstatus\n").unwrap();
    let r = invoke_full(&["--json", "run", "--dry-run", f.to_str().unwrap()], "", None, vec![]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(json_last(&r.stdout)["results"][0]["cmd"], "status");
    assert!(r.stderr.contains("> kd 0x04"), "dry-run vuelca las órdenes a stderr con --json: {}", r.stderr);
    let r = invoke_full(&["run", "--dry-run", f.to_str().unwrap()], "", None, vec![]);
    assert!(r.stdout.contains("> kd 0x04") && r.stdout.contains("status: host=1"), "{}", r.stdout);
    let r = invoke_full(&["run", "/no/existe.riotkvm"], "", None, vec![]);
    assert_eq!(r.code, 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_bad_script_exit_2() {
    let (r, _) = invoke(&["-p", "mock", "run", "-"], "nonsense 1\n");
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("línea 1"), "{}", r.stderr);
}

#[test]
fn run_from_stdin_releases() {
    let (r, h) = invoke(&["-p", "mock", "run", "-"], "keydown shift\nmove 100 200\n");
    assert_eq!(r.code, 0, "{}", r.stderr);
    let st = h.lock();
    assert!(st.keys.is_empty());
    assert_eq!(st.abs_pos, (100, 200));
}

#[test]
fn completion() {
    for sh in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let r = invoke_full(&["completion", sh], "", None, vec![]);
        assert_eq!(r.code, 0, "{sh}");
        assert!(r.stdout.contains("riotkvm"), "{sh}");
    }
    let r = invoke_full(&["completion", "bash"], "", None, vec![]);
    assert!(r.stdout.contains("_riotkvm"));
    assert_eq!(invoke_full(&["completion", "tcsh"], "", None, vec![]).code, 2);
}

#[test]
fn bench_with_mock() {
    let (r, _) = invoke(&["-p", "mock", "--json", "bench", "-n", "20"], "");
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(json_last(&r.stdout)["rtt_us"]["n"], 20);
}

// ---- BLE (dongle BLE v1 simulado en memoria: sin hardware)

#[test]
fn ble_mock_status_info_ping_and_keys() {
    let o = invoke_full(&["-p", "ble-mock", "--json", "status"], "", None, vec![]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["host"], true);
    assert_eq!(v["mode"], "C");
    assert_eq!(v["proto"], "report");

    let o = invoke_full(&["-p", "ble-mock", "--json", "info"], "", None, vec![]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap();
    assert!(v["reply"].as_str().unwrap().starts_with("proto=1 fw="));

    let o = invoke_full(&["-p", "ble-mock", "--json", "ping", "-n", "3"], "", None, vec![]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap();
    assert_eq!(v["via"], "ble");
    assert_eq!(v["rtt_us"]["n"], 3);

    for args in [vec!["key", "ctrl+alt+del"], vec!["type", "Hola"], vec!["mouse", "100", "200"], vec!["click", "-n", "2"]] {
        let mut a = vec!["-p", "ble-mock"];
        a.extend(args.iter());
        let o = invoke_full(&a, "", None, vec![]);
        assert_eq!(o.code, 0, "{:?}: {}", args, o.stderr);
    }
}

#[test]
fn ble_mock_refuses_serial_only_commands_with_a_clear_error() {
    let o = invoke_full(&["-p", "ble-mock", "stats"], "", None, vec![]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("solo existe por CDC"), "{}", o.stderr);
}
