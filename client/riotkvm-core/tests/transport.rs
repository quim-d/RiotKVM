//! Transporte de línea (hilo de E/S, lotes, ventana, fusión de movimiento) contra un enlace falso.
//! Port de las pruebas Python de SerialTransport + pruebas nuevas de latencia.

use riotkvm_core::transport::{find_dongle_in, IoConfig, IoTransport, Link, PortInfo};
use riotkvm_core::{Error, Transport};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Fake {
    lines: Vec<String>,
    writes: usize,
    buf: Vec<u8>,
    held: Vec<String>,
    hold: bool,
    silent: bool,
    err_on: Option<String>,
    write_delay: Duration,
}

impl Fake {
    fn reply(&mut self, ln: &str) {
        if self.silent {
            return;
        }
        if ln == "stats" {
            self.buf.extend_from_slice(b"cmd_n=1\r\nsd_n=2\r\n");
        } else if ln == "status" {
            self.buf.extend_from_slice(b"host=1 leds=0x00 hidready=1\r\n");
        } else if self.err_on.as_deref() == Some(ln) {
            self.buf.extend_from_slice(b"err comando desconocido\r\n");
        } else {
            self.buf.extend_from_slice(b"ok\r\n");
        }
    }
}

#[derive(Clone, Default)]
struct FakeLink(Arc<Mutex<Fake>>);

impl FakeLink {
    fn release(&self) {
        let mut f = self.0.lock().unwrap();
        f.hold = false;
        let held = std::mem::take(&mut f.held);
        for l in held {
            f.reply(&l);
        }
    }
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().lines.clone()
    }
}

impl Link for FakeLink {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        let delay = self.0.lock().unwrap().write_delay;
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let mut f = self.0.lock().unwrap();
        f.writes += 1;
        for ln in String::from_utf8(data.to_vec()).unwrap().lines() {
            f.lines.push(ln.to_string());
            if f.hold {
                f.held.push(ln.to_string());
            } else {
                f.reply(ln);
            }
        }
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut f = self.0.lock().unwrap();
        let n = f.buf.len().min(buf.len());
        buf[..n].copy_from_slice(&f.buf[..n]);
        f.buf.drain(..n);
        Ok(n)
    }
    fn idle_hint(&self) -> Option<Duration> {
        Some(Duration::from_micros(100))
    }
}

fn io_t(window: usize, timeout_ms: u64) -> (IoTransport, FakeLink) {
    let l = FakeLink::default();
    let cfg = IoConfig { name: "FAKE".into(), window, reply_timeout: Duration::from_millis(timeout_ms), coalesce: true };
    (IoTransport::new(Box::new(l.clone()), cfg), l)
}

/// Margen amplio para que el hilo de E/S procese lo encolado (es solo un límite inferior: nada se mide contra él).
fn settle() {
    std::thread::sleep(Duration::from_millis(500));
}

/// Espera por condición (hasta 20 s).
fn wait_for(f: impl Fn() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(20), "timeout esperando la condición");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn pipelining_window_and_flush() {
    let (mut t, l) = io_t(4, 2000);
    l.0.lock().unwrap().hold = true;
    for i in 0..10 {
        // distintos para que la fusión no los toque
        t.send(&format!("kd 0x{:02X}", i + 4), false).unwrap();
    }
    wait_for(|| l.lines().len() >= 4);
    settle(); // si la ventana no limitara, aquí habría ya más de 4
    assert_eq!(l.lines().len(), 4, "la ventana limita las órdenes en vuelo");
    l.release();
    t.flush().unwrap();
    assert_eq!(l.lines().len(), 10);
    assert_eq!(t.send("status", true).unwrap().unwrap(), "host=1 leds=0x00 hidready=1");
    t.close().unwrap();
}

#[test]
fn err_reported_without_desync() {
    let (mut t, l) = io_t(16, 2000);
    l.0.lock().unwrap().err_on = Some("ku 0x05".into());
    t.send("kd 0x04", false).unwrap();
    t.send("ku 0x05", false).unwrap();
    t.send("kd 0x06", false).unwrap();
    assert!(matches!(t.flush().unwrap_err(), Error::Device(_)));
    assert_eq!(t.send("status", true).unwrap().unwrap(), "host=1 leds=0x00 hidready=1"); // sigue sincronizado
    t.flush().unwrap();
}

#[test]
fn err_on_waited_send() {
    let (mut t, l) = io_t(16, 2000);
    l.0.lock().unwrap().err_on = Some("nope".into());
    assert_eq!(t.send("nope", true).unwrap_err().exit_code(), 4);
}

#[test]
fn timeout_is_transport_error() {
    let (mut t, l) = io_t(16, 150);
    l.0.lock().unwrap().silent = true;
    let e = t.send("status", true).unwrap_err();
    assert_eq!(e.exit_code(), 5, "{e}");
    // sin respuesta en un envío sin espera: el error sale en el flush
    t.send("kd 0x04", false).unwrap();
    assert_eq!(t.flush().unwrap_err().exit_code(), 5);
}

#[test]
fn batches_writes_while_device_is_slow() {
    let (mut t, l) = io_t(16, 2000);
    l.0.lock().unwrap().write_delay = Duration::from_millis(200); // la 1.ª escritura es lenta: lo demás se agrupa
    for i in 0..60 {
        t.send(&format!("kd 0x{:02X}", 4 + (i % 100)), false).unwrap();
    }
    t.flush().unwrap();
    let f = l.0.lock().unwrap();
    assert_eq!(f.lines.len(), 60);
    assert!(f.writes <= 30, "esperaba escrituras por lotes, hubo {} para 60 líneas", f.writes);
}

#[test]
fn coalesces_motion_when_window_is_full() {
    let (mut t, l) = io_t(2, 2000);
    l.0.lock().unwrap().hold = true;
    t.send("kd 0x04", false).unwrap();
    t.send("ku 0x04", false).unwrap();
    wait_for(|| l.lines().len() >= 2); // ventana llena (2 en vuelo)
    settle();
    for i in 0..100 {
        t.send(&format!("ma {} {} 0", i, i + 1), false).unwrap();
    }
    for _ in 0..50 {
        t.send("mm 1 -1 0", false).unwrap();
    }
    t.send("mba 1", false).unwrap();
    t.send("ma 7 8 0", false).unwrap();
    settle();
    l.release();
    t.flush().unwrap();
    assert_eq!(l.lines(), ["kd 0x04", "ku 0x04", "ma 99 100 0", "mm 50 -50 0", "mba 1", "ma 7 8 0"]);
    assert_eq!(t.trace().unwrap().summary().coalesced, 99 + 49);
}

#[test]
fn does_not_merge_waited_or_wheel() {
    let (mut t, l) = io_t(1, 2000);
    l.0.lock().unwrap().hold = true;
    t.send("kd 0x04", false).unwrap();
    wait_for(|| l.lines().len() >= 1);
    t.send("ma 1 1 0", false).unwrap();
    t.send("ma 2 2 3", false).unwrap(); // con rueda: no se fusiona
    t.send("mm 100 0 0", false).unwrap();
    t.send("mm 100 0 0", false).unwrap(); // 200 > 127: no cabe
    settle();
    l.release();
    t.flush().unwrap();
    assert_eq!(l.lines(), ["kd 0x04", "ma 1 1 0", "ma 2 2 3", "mm 100 0 0", "mm 100 0 0"]);
}

#[test]
fn trace_records_timestamps() {
    let (mut t, _l) = io_t(16, 2000);
    for _ in 0..5 {
        t.send("status", true).unwrap();
    }
    let tr = t.trace().unwrap();
    let s = tr.samples();
    assert_eq!(s.len(), 5);
    assert!(s.iter().all(|x| x.t_enq_us <= x.t_write_us && x.t_write_us <= x.t_reply_us && x.ok));
    assert_eq!(tr.summary().count, 5);
    assert!(tr.to_csv().lines().count() == 6);
}

#[test]
fn closed_transport_errors() {
    let (mut t, _l) = io_t(16, 2000);
    t.close().unwrap();
    assert!(t.send("status", true).is_err());
    assert!(t.close().is_ok());
}

#[test]
fn rejects_newlines() {
    let (mut t, _l) = io_t(16, 2000);
    assert_eq!(t.send("a\nb", false).unwrap_err().exit_code(), 5);
}

fn port(name: &str, dongle: bool) -> PortInfo {
    PortInfo {
        port: name.into(),
        vid: None,
        pid: None,
        description: "x".into(),
        serial_number: None,
        is_dongle: dongle,
    }
}

#[test]
fn autodetect() {
    assert_eq!(find_dongle_in(&[port("COM3", false), port("COM9", true)]).unwrap(), "COM9");
}

#[test]
fn no_dongle_or_several() {
    assert_eq!(find_dongle_in(&[]).unwrap_err().exit_code(), 3);
    assert_eq!(find_dongle_in(&[port("A", true), port("B", true)]).unwrap_err().exit_code(), 3);
}

#[test]
fn stats_has_two_reply_lines_and_stays_in_sync() {
    let (mut t, _l) = io_t(16, 2000);
    t.send("kd 0x04", false).unwrap();
    assert_eq!(t.send("stats", true).unwrap().unwrap(), "cmd_n=1\nsd_n=2");
    t.send("ku 0x04", false).unwrap();
    assert_eq!(t.send("status", true).unwrap().unwrap(), "host=1 leds=0x00 hidready=1");
    t.flush().unwrap();
}

#[test]
fn device_error_hints() {
    let e = riotkvm_core::error::device_error("ma 1 1 0", "err bootproto");
    assert_eq!(e.exit_code(), 4);
    assert!(e.to_string().contains("BIOS"), "{e}");
    assert!(riotkvm_core::error::device_error("kd 0x04", "err nohost").to_string().contains("enumerado"));
}