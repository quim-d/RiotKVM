//! Dongle simulado: interpreta el protocolo de texto igual que src/main.cpp y guarda el estado HID.
//! Como el transporte real, los `err` de envíos sin espera se devuelven en `flush()`.

use super::Transport;
use crate::error::{device_error, Error, Result};
use crate::protocol::parse_int0;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const KNOWN: [&str; 13] = ["type", "kd", "ku", "kall", "mm", "ma", "mbr", "mba", "status", "info", "stats", "bootloader", "led"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockEvent {
    Kd(u8),
    Ku(u8),
    Kall,
    Mm(i64, i64, i64),
    Ma(i64, i64, i64),
    Mbr(u8),
    Mba(u8),
}

#[derive(Debug, Default)]
pub struct MockState {
    pub sent: Vec<String>,
    pub keys: BTreeSet<u8>,
    pub typed: Vec<String>,
    pub rel_buttons: u8,
    pub abs_buttons: u8,
    pub abs_pos: (i64, i64),
    pub rel_total: [i64; 2],
    pub wheel_total: i64,
    pub events: Vec<MockEvent>,
    /// Respuesta `err ...` para la siguiente orden.
    pub fail_next: Option<String>,
    /// Respuesta `err ...` fija para estas líneas exactas.
    pub fail_lines: HashMap<String, String>,
    /// Ajuste de la luz (`led`); `None` = nunca tocado (auto).
    pub led: Option<crate::Led>,
}

impl MockState {
    /// Usos de las pulsaciones `kd`, en orden.
    pub fn kd_usages(&self) -> Vec<u8> {
        self.events.iter().filter_map(|e| if let MockEvent::Kd(u) = e { Some(*u) } else { None }).collect()
    }
}

/// Asa compartida para inspeccionar el mock después de entregarlo a un `Kvm`.
#[derive(Clone)]
pub struct MockHandle(Arc<Mutex<MockState>>);

impl MockHandle {
    pub fn lock(&self) -> MutexGuard<'_, MockState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()) // un pánico en otro hilo no debe envenenar el mock
    }
}

pub struct MockTransport {
    state: Arc<Mutex<MockState>>,
    echo: Option<Box<dyn Fn(&str) + Send>>,
    latency: Duration,
    pending_err: Option<Error>,
    opened: bool,
}

fn i8v(v: i64) -> i64 {
    let v = v & 0xFF;
    if v > 127 {
        v - 256
    } else {
        v
    }
}

impl MockTransport {
    pub fn new() -> MockTransport {
        MockTransport {
            state: Arc::new(Mutex::new(MockState::default())),
            echo: None,
            latency: Duration::ZERO,
            pending_err: None,
            opened: true,
        }
    }

    pub fn with_handle() -> (MockTransport, MockHandle) {
        let m = MockTransport::new();
        let h = MockHandle(m.state.clone());
        (m, h)
    }

    /// Un mock nuevo que comparte el estado de `h` (para fábricas de transporte en pruebas).
    pub fn sharing(h: &MockHandle) -> MockTransport {
        let mut m = MockTransport::new();
        m.state = h.0.clone();
        m
    }

    pub fn handle(&self) -> MockHandle {
        MockHandle(self.state.clone())
    }

    /// Vuelca cada línea enviada (dry-run / -v).
    pub fn with_echo(mut self, f: impl Fn(&str) + Send + 'static) -> Self {
        self.echo = Some(Box::new(f));
        self
    }

    pub fn with_latency(mut self, d: Duration) -> Self {
        self.latency = d;
        self
    }

    fn handle_line(st: &mut MockState, line: &str) -> String {
        if let Some(r) = st.fail_next.take() {
            return r;
        }
        if let Some(r) = st.fail_lines.get(line) {
            return r.clone();
        }
        let line = line.trim();
        let (c, a) = match line.split_once(' ') {
            Some((c, a)) => (c, a),
            None => (line, ""),
        };
        if !KNOWN.contains(&c) {
            return "err comando desconocido".into();
        }
        let mut n: Vec<i64> = Vec::new();
        for p in a.split_whitespace().take(3) {
            match parse_int0(p) {
                Some(v) => n.push(v),
                None => break,
            }
        }
        n.resize(3, 0);
        match c {
            "type" => st.typed.push(a.to_string()),
            "kd" | "ku" => {
                let Some(v) = parse_int0(a) else { return "err argumento".into() };
                let u = (v & 0xFF) as u8;
                if c == "kd" {
                    st.keys.insert(u);
                    st.events.push(MockEvent::Kd(u));
                } else {
                    st.keys.remove(&u);
                    st.events.push(MockEvent::Ku(u));
                }
            }
            "kall" => {
                st.keys.clear();
                st.events.push(MockEvent::Kall);
            }
            "mm" => {
                let (dx, dy, w) = (i8v(n[0]), i8v(n[1]), i8v(n[2]));
                st.rel_total[0] += dx;
                st.rel_total[1] += dy;
                st.wheel_total += w;
                st.events.push(MockEvent::Mm(dx, dy, w));
            }
            "ma" => {
                let (x, y, w) = (n[0] & 0xFFFF, n[1] & 0xFFFF, i8v(n[2]));
                st.abs_pos = (x, y);
                st.wheel_total += w;
                st.events.push(MockEvent::Ma(x, y, w));
            }
            "mbr" => {
                st.rel_buttons = (n[0] & 0xFF) as u8;
                st.events.push(MockEvent::Mbr(st.rel_buttons));
            }
            "mba" => {
                st.abs_buttons = (n[0] & 0xFF) as u8;
                st.events.push(MockEvent::Mba(st.abs_buttons));
            }
            "status" => return "host=1 leds=0x00 hidready=1".into(),
            "info" => return format!("fw=mock led={} build=\"mock\"", st.led.unwrap_or(crate::Led::Auto)),
            "led" if a.trim().is_empty() => return format!("led={}", st.led.unwrap_or(crate::Led::Auto)),
            "led" => match crate::Led::parse(a) {
                Ok(l) => st.led = Some(l),
                Err(_) => return "err args".into(),
            },
            "stats" if a.trim() != "reset" => return "cmd_n=0 snd_n=0\nsd_n=0".into(),
            _ => {}
        }
        "ok".into()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for MockTransport {
    fn name(&self) -> &str {
        "mock"
    }

    fn send(&mut self, line: &str, wait: bool) -> Result<Option<String>> {
        if !self.opened {
            return Err(Error::Transport("MockTransport no abierto".into()));
        }
        if let Some(e) = &self.echo {
            e(line);
        }
        if !self.latency.is_zero() {
            std::thread::sleep(self.latency);
        }
        let reply = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.sent.push(line.to_string());
            Self::handle_line(&mut st, line)
        };
        if reply.starts_with("err") {
            let e = device_error(line, &reply);
            if wait {
                return Err(e);
            }
            self.pending_err.get_or_insert(e);
            return Ok(None);
        }
        Ok(if wait { Some(reply) } else { None })
    }

    fn flush(&mut self) -> Result<()> {
        match self.pending_err.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn close(&mut self) -> Result<()> {
        self.opened = false;
        Ok(())
    }
}
