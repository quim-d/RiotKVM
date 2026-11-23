//! `dkvm fw` y `dkvm sd` sin hardware: solo modos que no escriben en ningún dispositivo ni descargan nada.

use dkvm_cli::{run, Env};
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

fn invoke(args: &[&str]) -> (i32, String, String) {
    let (o, e) = (Buf::default(), Buf::default());
    let mut env = Env {
        stdin: Box::new(std::io::empty()),
        stdout: Arc::new(Mutex::new(Box::new(o.clone()))),
        stderr: Arc::new(Mutex::new(Box::new(e.clone()))),
        transport_factory: None,
        list_ports: Box::new(Vec::new),
    };
    let mut a = vec!["dkvm"];
    a.extend_from_slice(args);
    let code = run(a, &mut env);
    let t = |b: &Buf| String::from_utf8(b.0.lock().unwrap().clone()).unwrap();
    (code, t(&o), t(&e))
}

fn image_header(chip_id: u16) -> Vec<u8> {
    let mut d = vec![0u8; 4096];
    d[0] = 0xE9;
    d[3] = 0x4F; // 16 MB
    d[12..14].copy_from_slice(&chip_id.to_le_bytes());
    d
}

#[test]
fn fw_flash_dry_run_with_own_bin() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("fw.bin");
    std::fs::write(&f, image_header(9)).unwrap();
    let (code, out, err) = invoke(&["--json", "fw", "flash", "--dry-run", "--file", f.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["dry_run"], true);
    assert!(v["plan"][0].as_str().unwrap().contains("0x0009"));
    assert!(err.contains("no se verifica"));
    // sin cabecera ESP: rechazado antes de tocar nada
    std::fs::write(&f, vec![0u8; 4096]).unwrap();
    let (code, _, err) = invoke(&["fw", "flash", "--dry-run", "--file", f.to_str().unwrap()]);
    assert_eq!(code, 5, "{err}");
    assert!(err.contains("0xE9"));
}

#[test]
fn sd_ventoy_needs_target_and_checks_size_first() {
    let (code, _, _) = invoke(&["sd", "ventoy"]);
    assert_eq!(code, 2);
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("small.img");
    std::fs::File::create(&f).unwrap().set_len(16 << 20).unwrap();
    let (code, _, err) = invoke(&["sd", "ventoy", "--image", f.to_str().unwrap()]);
    assert_eq!(code, 5);
    assert!(err.contains("pequeño"), "{err}");
    let (code, _, _) = invoke(&["sd", "ventoy", "--disk", "disk9", "--image", f.to_str().unwrap()]);
    assert_eq!(code, 2, "--disk e --image son excluyentes");
}
