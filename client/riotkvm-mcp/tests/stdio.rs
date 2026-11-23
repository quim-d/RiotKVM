//! El binario de punta a punta por stdio, con el dongle BLE simulado (`RIOTKVM_PORT=ble-mock`) y sin vídeo.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct Srv {
    child: Child,
    stdin: ChildStdin,
    out: Receiver<Value>,
}

impl Srv {
    fn start() -> Srv {
        let mut child = Command::new(env!("CARGO_BIN_EXE_riotkvm-mcp"))
            .env("RIOTKVM_PORT", "ble-mock")
            .env("RIOTKVM_MCP_VIDEO", "none")
            .env("RIOTKVM_STATE_DIR", std::env::temp_dir().join("riotkvm-mcp-test-state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, out) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines() {
                let Ok(l) = l else { break };
                let _ = tx.send(serde_json::from_str(&l).unwrap_or(json!({"no-json": l})));
            }
        });
        Srv { child, stdin, out }
    }
    fn send_raw(&mut self, b: &[u8]) {
        self.stdin.write_all(b).unwrap();
        self.stdin.flush().unwrap();
    }
    fn send(&mut self, v: Value) {
        self.send_raw(format!("{v}\n").as_bytes());
    }
    fn recv(&self, secs: u64) -> Option<Value> {
        self.out.recv_timeout(Duration::from_secs(secs)).ok()
    }
    fn exec(&mut self, id: i64, code: &str, timeout: u64) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"execute","arguments":{"code":code,"timeoutSeconds":timeout}}}));
        self.recv(30).expect("respuesta")
    }
}

impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn text(r: &Value) -> String {
    r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
}

#[test]
fn stdio_execute_con_ble_mock_y_limites() {
    let mut s = Srv::start();
    s.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}));
    assert_eq!(s.recv(10).unwrap()["result"]["serverInfo"]["name"], "riotkvm");
    let t0 = Instant::now();
    let r = s.exec(2, "riotkvm.mouse.scroll(1e15); riotkvm.mouse.moveRel(1e15, 0); riotkvm.type('hola'); return riotkvm.connection().link", 20);
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(t0.elapsed() < Duration::from_secs(15), "{:?}", t0.elapsed());
    let r = s.exec(3, "riotkvm.raw('reboot\\u0000')", 20);
    assert!(text(&r).contains("control"), "{r}");
    let r = s.exec(4, "riotkvm.screenshot()", 20);
    assert!(text(&r).contains("vídeo desactivado"), "{r}");
    let r = s.exec(5, "riotkvm.sleep(1e300)", 2);
    assert!(text(&r).contains("tiempo agotado"), "{r}");
}

#[test]
fn stdio_no_utf8_y_lote_no_tumban_el_servidor() {
    let mut s = Srv::start();
    s.send_raw(b"\xff\xfe{\"jsonrpc\":\"2.0\"}\n");
    assert_eq!(s.recv(10).unwrap()["error"]["code"], -32700);
    s.send(json!([{"jsonrpc":"2.0","id":1,"method":"ping"}]));
    assert_eq!(s.recv(10).unwrap()["error"]["code"], -32600);
    s.send(json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    assert_eq!(s.recv(10).unwrap()["id"], 2);
}

#[test]
fn stdio_cancelacion_corta_execute() {
    let mut s = Srv::start();
    s.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"execute","arguments":{"code":"while (true) {}","timeoutSeconds":120}}}));
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    s.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7,"reason":"test"}}));
    s.send(json!({"jsonrpc":"2.0","id":8,"method":"ping"}));
    let r = s.recv(10).expect("el ping debe responderse");
    assert_eq!(r["id"], 8, "a la petición cancelada no se responde: {r}");
    assert!(t0.elapsed() < Duration::from_secs(5));
}
