//! Codec "BLE v1" (docs/PROTOCOL.md, sección "BLE v1 (etapa 2) - CONTRATO").
//!
//! Traduce las órdenes de texto de alto nivel que ya emite `Kvm` (`kd`, `ku`, `kall`, `mm`, `ma`, `mbr`, `mba`,
//! `status`, `info`, `type`) a registros `[op][len][payload]` concatenables (LE), y decodifica EVT
//! (STATE, PONG, ERR) al formato de texto del CDC para reutilizar el análisis existente.

use crate::error::{Error, Result};
use crate::protocol::parse_int0;
use crate::transport::ble::{Action, Codec, Decoded, Encoded};

pub const PROTO_VERSION: u8 = 1;

// CMD
pub const KEY_DOWN: u8 = 0x01;
pub const KEY_UP: u8 = 0x02;
pub const RELEASE_ALL: u8 = 0x03;
pub const MOUSE_REL: u8 = 0x04;
pub const MOUSE_ABS: u8 = 0x05;
pub const BUTTONS_REL: u8 = 0x06;
pub const BUTTONS_ABS: u8 = 0x07;
pub const WHEEL: u8 = 0x08;
pub const PING: u8 = 0x10;
pub const GET_STATE: u8 = 0x11;
pub const SET_MODE: u8 = 0x12;
/// Luz (firmware 1.2.0+): `mode, r, g, b, bright`. Ver [`crate::led`].
pub const SET_LED: u8 = 0x13;
// EVT
pub const STATE: u8 = 0x81;
pub const PONG: u8 = 0x82;
pub const ERR: u8 = 0x83;

/// Un registro `[op][len][payload]`.
pub fn record(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + payload.len());
    v.push(op);
    v.push(payload.len() as u8);
    v.extend_from_slice(payload);
    v
}

/// Separa una escritura/notificación en registros. Un registro que excede el final corta el resto
/// (igual que el firmware: `MALFORMED`); devuelve también si hubo truncado.
pub fn split_records(buf: &[u8]) -> (Vec<(u8, &[u8])>, bool) {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 <= buf.len() {
        let (op, len) = (buf[i], buf[i + 1] as usize);
        if i + 2 + len > buf.len() {
            return (out, true);
        }
        out.push((op, &buf[i + 2..i + 2 + len]));
        i += 2 + len;
    }
    (out, i != buf.len())
}

pub fn err_name(code: u8) -> &'static str {
    match code {
        1 => "busy",
        2 => "range",
        3 => "unknownop",
        4 => "malformed",
        5 => "nohost",
        6 => "nohid",
        7 => "bootproto",
        8 => "rollover",
        _ => "desconocido",
    }
}

fn err_hint(code: u8) -> &'static str {
    match code {
        1 => " (endpoint USB ocupado o servidor suspendido: el informe se perdió)",
        5 => " (el servidor no ha enumerado el dongle: ¿USB conectado y servidor encendido?)",
        6 => " (el modo USB actual no tiene HID: cambia a modo B o C)",
        7 => " (el servidor está en protocolo de arranque/BIOS: el ratón no está disponible; usa el teclado)",
        8 => " (más de 6 teclas pulsadas a la vez)",
        _ => "",
    }
}

pub fn mode_letter(m: u8) -> char {
    match m {
        0 => 'A',
        1 => 'B',
        2 => 'C',
        _ => '?',
    }
}

/// STATE (`flags, leds, mode`) -> línea de texto estilo `status` del CDC.
pub fn state_text(flags: u8, leds: u8, mode: u8) -> String {
    let b = |bit: u8| (flags >> bit) & 1;
    let hidready = b(0) & b(4);
    format!(
        "host={} leds=0x{:02x} hidready={} susp={} proto={} sd={} mode={}",
        b(0),
        leds,
        hidready,
        b(1),
        if b(3) == 1 { "boot" } else { "report" },
        b(2),
        mode_letter(mode)
    )
}

/// INFO (8 bytes; 13 con la luz desde 1.2.0) -> texto legible (`led=` solo si el firmware lo envía). `fw=X.Y.Z` con el patch del byte [6] (0 en firmwares que no lo envían);
/// la versión estructurada sale de [`crate::FwVersion::from_info`] o de `FwVersion::from_info_reply`.
pub fn info_text(b: &[u8]) -> String {
    if b.len() < 6 {
        return format!("info ilegible ({} bytes)", b.len());
    }
    let caps = u16::from_le_bytes([b[4], b[5]]);
    let names = ["teclado", "raton-rel", "raton-abs", "msc/sd", "set-mode", "ping", "luz"];
    let c: Vec<&str> = names.iter().enumerate().filter(|(i, _)| caps & (1 << i) != 0).map(|(_, n)| *n).collect();
    let fw = crate::FwVersion::from_info(b).expect("len >= 6");
    let led = crate::Led::from_info_bytes(b).map(|l| format!(" led={l}")).unwrap_or_default();
    format!("proto={} fw={} mode={} caps={}{led}", b[0], fw, mode_letter(b[3]), c.join(","))
}

fn num(it: &mut std::str::SplitWhitespace, name: &str) -> Result<i64> {
    let t = it.next().ok_or_else(|| Error::Usage(format!("falta {name}")))?;
    parse_int0(t).ok_or_else(|| Error::Usage(format!("{name} no es un entero: {t}")))
}

pub struct BleV1;

impl BleV1 {
    pub fn new() -> BleV1 {
        BleV1
    }
}

impl Default for BleV1 {
    fn default() -> Self {
        BleV1
    }
}

/// Un registro que se puede fusionar con el siguiente (movimiento de ratón).
fn merge_records(a: &[u8], b: &[u8]) -> Option<Vec<u8>> {
    if a.len() < 2 || b.len() < 2 || a[0] != b[0] {
        return None;
    }
    match a[0] {
        // absoluto: gana el último destino; la rueda se suma si cabe
        MOUSE_ABS if a.len() == 6 && b.len() == 6 || a.len() == 8 && b.len() == 8 => {
            if a.len() == 8 {
                let w = (a[6] as i8 as i16 + b[6] as i8 as i16).clamp(-127, 127) as i8;
                let p = (a[7] as i8 as i16 + b[7] as i8 as i16).clamp(-127, 127) as i8;
                let mut m = b.to_vec();
                m[6] = w as u8;
                m[7] = p as u8;
                Some(m)
            } else {
                Some(b.to_vec())
            }
        }
        MOUSE_REL if a.len() == b.len() && (a.len() == 4 || a.len() == 6) => {
            let sum = |i: usize| a[i] as i8 as i16 + b[i] as i8 as i16;
            let (dx, dy) = (sum(2), sum(3));
            if !(-127..=127).contains(&dx) || !(-127..=127).contains(&dy) {
                return None;
            }
            let mut m = a.to_vec();
            m[2] = dx as i8 as u8;
            m[3] = dy as i8 as u8;
            if a.len() == 6 {
                let (w, p) = (sum(4), sum(5));
                if !(-127..=127).contains(&w) || !(-127..=127).contains(&p) {
                    return None;
                }
                m[4] = w as i8 as u8;
                m[5] = p as i8 as u8;
            }
            Some(m)
        }
        _ => None,
    }
}

impl Codec for BleV1 {
    fn name(&self) -> &str {
        "BLE v1"
    }

    fn encode(&mut self, line: &str) -> Result<Encoded> {
        let line = line.trim();
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut it = rest.split_whitespace();
        let one = |op: u8, payload: &[u8]| Ok(Encoded { action: Action::Frames(vec![record(op, payload)]), reply: false });
        match cmd {
            "kd" | "ku" => {
                let u = num(&mut it, "uso")?;
                if !((1..=0xA4).contains(&u) || (0xE0..=0xE7).contains(&u)) {
                    return Err(Error::Usage(format!("uso HID fuera de rango: {u}")));
                }
                one(if cmd == "kd" { KEY_DOWN } else { KEY_UP }, &[u as u8])
            }
            "kall" => one(RELEASE_ALL, &[]),
            "mm" => {
                let (dx, dy) = (num(&mut it, "dx")?, num(&mut it, "dy")?);
                let w = it.next().and_then(parse_int0).unwrap_or(0);
                let c = |v: i64| v.clamp(-127, 127) as i8 as u8;
                if w != 0 {
                    one(MOUSE_REL, &[c(dx), c(dy), c(w), 0])
                } else {
                    one(MOUSE_REL, &[c(dx), c(dy)])
                }
            }
            "ma" => {
                let (x, y) = (num(&mut it, "x")?.clamp(0, 32767) as u16, num(&mut it, "y")?.clamp(0, 32767) as u16);
                let w = it.next().and_then(parse_int0).unwrap_or(0).clamp(-127, 127) as i8 as u8;
                let mut p = Vec::with_capacity(6);
                p.extend_from_slice(&x.to_le_bytes());
                p.extend_from_slice(&y.to_le_bytes());
                if w != 0 {
                    p.extend_from_slice(&[w, 0]);
                }
                one(MOUSE_ABS, &p)
            }
            "mbr" | "mba" => {
                let m = num(&mut it, "máscara")?;
                one(if cmd == "mbr" { BUTTONS_REL } else { BUTTONS_ABS }, &[m as u8])
            }
            "status" => Ok(Encoded { action: Action::Frames(vec![record(GET_STATE, &[])]), reply: true }),
            "info" => Ok(Encoded { action: Action::ReadInfo, reply: true }),
            "type" => {
                // el CDC escribe con layout US fijo; aquí se expande a KEY_DOWN/KEY_UP con el mismo layout
                let text = rest;
                let layout = crate::layouts::get_layout("us")?;
                let mut frames = Vec::new();
                for (usage, mods) in layout.text_strokes(text)? {
                    let mu = crate::keys::mods_to_usages(mods);
                    for m in &mu {
                        frames.push(record(KEY_DOWN, &[*m as u8]));
                    }
                    frames.push(record(KEY_DOWN, &[usage as u8]));
                    frames.push(record(KEY_UP, &[usage as u8]));
                    for m in mu.iter().rev() {
                        frames.push(record(KEY_UP, &[*m as u8]));
                    }
                }
                Ok(Encoded { action: Action::Frames(frames), reply: false })
            }
            "mode" => {
                let m = match rest.trim().to_ascii_uppercase().as_str() {
                    "A" | "0" => 0,
                    "B" | "1" => 1,
                    "C" | "2" => 2,
                    o => return Err(Error::Usage(format!("modo desconocido {o:?}: usa A, B o C"))),
                };
                one(SET_MODE, &[m])
            }
            "led" => {
                let l = crate::Led::parse(rest)?;
                one(SET_LED, &l.payload())
            }
            "stats" | "bootloader" => Err(Error::Usage(format!("'{cmd}' solo existe por CDC (serie): usa -p COMx"))),
            other => Err(Error::Usage(format!("orden desconocida para BLE: {other:?}"))),
        }
    }

    fn decode(&mut self, evt: &[u8]) -> Vec<Decoded> {
        let (recs, _) = split_records(evt);
        let mut out = Vec::new();
        for (op, p) in recs {
            match (op, p.len()) {
                (STATE, 3) => {
                    let t = state_text(p[0], p[1], p[2]);
                    out.push(Decoded::Reply(t.clone())); // por si alguien esperaba GET_STATE
                    out.push(Decoded::Status(t));
                }
                (PONG, 6) => out.push(Decoded::Pong { seq: u16::from_le_bytes([p[0], p[1]]) as u32 }),
                (ERR, 2) if p[0] == SET_LED && p[1] == 3 => out.push(Decoded::Err(crate::led::UNSUPPORTED.into())),
                (ERR, 2) => out.push(Decoded::Err(format!("el dongle notificó err {} (op 0x{:02x}){}", err_name(p[1]), p[0], err_hint(p[1])))),
                _ => {}
            }
        }
        out
    }

    fn ping(&mut self, seq: u32) -> Option<Vec<u8>> {
        Some(record(PING, &(seq as u16).to_le_bytes()))
    }

    fn concat_ok(&self) -> bool {
        true
    }

    fn merge(&self, prev: &[u8], next: &[u8]) -> Option<Vec<u8>> {
        merge_records(prev, next)
    }

    fn info_text(&self, bytes: &[u8]) -> String {
        info_text(bytes)
    }

    fn check_info(&self, bytes: &[u8]) -> Result<()> {
        match bytes.first() {
            Some(&PROTO_VERSION) => Ok(()),
            Some(v) => Err(Error::Transport(format!("el dongle habla el protocolo BLE v{v}; este cliente solo entiende v{PROTO_VERSION}: actualiza el cliente o el firmware"))),
            None => Err(Error::Transport("INFO vacío".into())),
        }
    }
}

/// Dongle BLE v1 simulado (para pruebas y `--port ble-mock`): interpreta registros CMD, mantiene el estado
/// HID y responde con STATE/PONG/ERR como describe PROTOCOL.md.
pub mod sim {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default, Clone)]
    pub struct SimState {
        pub keys: BTreeSet<u8>,
        pub buttons_rel: u8,
        pub buttons_abs: u8,
        pub abs: (u16, u16),
        pub rel_total: (i64, i64),
        pub wheel_total: i64,
        pub mode: u8,
        pub flags: u8,
        pub leds: u8,
        pub log: Vec<(u8, Vec<u8>)>,
        /// Ajuste de la luz (SET_LED). `None` = simula un firmware anterior a 1.2.0 (responde UNKNOWN_OP).
        pub led: Option<[u8; 5]>,
        pub t0: Option<std::time::Instant>,
    }

    #[derive(Clone)]
    pub struct Sim(pub Arc<Mutex<SimState>>);

    impl Sim {
        /// Estado inicial: host enumerado, HID y SD disponibles, modo C.
        pub fn new() -> Sim {
            Sim(Arc::new(Mutex::new(SimState { mode: 2, flags: 0b0011_0101, t0: Some(std::time::Instant::now()), led: Some([0, 0, 255, 70, 35]), ..Default::default() })))
        }
        pub fn state(&self) -> SimState {
            self.0.lock().unwrap().clone()
        }
        pub fn set_flags(&self, flags: u8) {
            self.0.lock().unwrap().flags = flags;
        }
        /// INFO como el firmware 1.2.0 (13 bytes con la luz) o de 8 bytes si simula uno anterior (`led: None`).
        pub fn info_bytes(&self) -> Vec<u8> {
            let s = self.0.lock().unwrap();
            let mut b = vec![PROTO_VERSION, 1, 2, s.mode, 0x3f, 0, 0, 0];
            if let Some(l) = s.led {
                b[4] |= 0x40;
                b.extend_from_slice(&l);
            }
            b
        }
        pub fn state_evt(&self) -> Vec<u8> {
            let s = self.0.lock().unwrap();
            record(STATE, &[s.flags, s.leds, s.mode])
        }
        /// Procesa una escritura CMD y devuelve las notificaciones EVT a enviar.
        pub fn handle(&self, write: &[u8]) -> Vec<Vec<u8>> {
            let mut s = self.0.lock().unwrap();
            let (recs, truncated) = split_records(write);
            let mut evts: Vec<u8> = Vec::new();
            let hid_ok = s.flags & 0x10 != 0;
            let host = s.flags & 1 != 0;
            for (op, p) in recs {
                s.log.push((op, p.to_vec()));
                let err = |code: u8, evts: &mut Vec<u8>| evts.extend(record(ERR, &[op, code]));
                match (op, p.len()) {
                    (KEY_DOWN | KEY_UP | RELEASE_ALL | MOUSE_REL | MOUSE_ABS | BUTTONS_REL | BUTTONS_ABS | WHEEL, _) if !hid_ok => err(6, &mut evts),
                    (KEY_DOWN | KEY_UP | RELEASE_ALL | MOUSE_REL | MOUSE_ABS | BUTTONS_REL | BUTTONS_ABS | WHEEL, _) if !host => err(5, &mut evts),
                    (KEY_DOWN, 1) => {
                        let u = p[0];
                        if !((1..=0xA4).contains(&u) || (0xE0..=0xE7).contains(&u)) {
                            err(2, &mut evts)
                        } else if !(0xE0..=0xE7).contains(&u) && s.keys.iter().filter(|k| !(0xE0..=0xE7).contains(*k)).count() >= 6 && !s.keys.contains(&u) {
                            err(8, &mut evts)
                        } else {
                            s.keys.insert(u);
                        }
                    }
                    (KEY_UP, 1) => {
                        s.keys.remove(&p[0]);
                    }
                    (RELEASE_ALL, 0) => {
                        s.keys.clear();
                        s.buttons_rel = 0;
                        s.buttons_abs = 0;
                    }
                    (MOUSE_REL, 2 | 4) => {
                        let c = |v: u8| (v as i8).max(-127) as i64;
                        s.rel_total.0 += c(p[0]);
                        s.rel_total.1 += c(p[1]);
                        if p.len() == 4 {
                            s.wheel_total += c(p[2]);
                        }
                    }
                    (MOUSE_ABS, 4 | 6) => {
                        s.abs = (u16::from_le_bytes([p[0], p[1]]).min(32767), u16::from_le_bytes([p[2], p[3]]).min(32767));
                        if p.len() == 6 {
                            s.wheel_total += (p[4] as i8) as i64;
                        }
                    }
                    (BUTTONS_REL, 1) => {
                        if p[0] > 0x1F {
                            err(2, &mut evts)
                        } else {
                            s.buttons_rel = p[0]
                        }
                    }
                    (BUTTONS_ABS, 1) => {
                        if p[0] > 0x1F {
                            err(2, &mut evts)
                        } else {
                            s.buttons_abs = p[0]
                        }
                    }
                    (WHEEL, 2) => s.wheel_total += (p[0] as i8) as i64,
                    (PING, 2) => {
                        let t = s.t0.map(|t| t.elapsed().as_micros() as u32).unwrap_or(0);
                        let mut pl = p.to_vec();
                        pl.extend_from_slice(&t.to_le_bytes());
                        evts.extend(record(PONG, &pl));
                    }
                    (GET_STATE, 0) => evts.extend(record(STATE, &[s.flags, s.leds, s.mode])),
                    (SET_MODE, 1) if p[0] <= 2 => {
                        s.mode = p[0];
                        evts.extend(record(STATE, &[s.flags, s.leds, s.mode]));
                    }
                    (SET_MODE, 1) => err(2, &mut evts),
                    (SET_LED, _) if s.led.is_none() => err(3, &mut evts),
                    (SET_LED, 5) if p[0] <= 2 && p[4] <= 100 => s.led = Some([p[0], p[1], p[2], p[3], p[4]]),
                    (SET_LED, 5) => err(2, &mut evts),
                    (0x01..=0x13, _) => err(4, &mut evts),
                    _ => err(3, &mut evts),
                }
            }
            if truncated {
                evts.extend(record(ERR, &[0, 4]));
            }
            if evts.is_empty() {
                vec![]
            } else {
                vec![evts]
            }
        }
    }

    impl Default for Sim {
        fn default() -> Self {
            Sim::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(c: &mut BleV1, l: &str) -> Vec<Vec<u8>> {
        match c.encode(l).unwrap().action {
            Action::Frames(f) => f,
            Action::ReadInfo => panic!("info"),
        }
    }

    #[test]
    fn keyboard_and_buttons() {
        let mut c = BleV1;
        assert_eq!(enc(&mut c, "kd 0x04"), vec![vec![0x01, 1, 0x04]]);
        assert_eq!(enc(&mut c, "ku 0xE0"), vec![vec![0x02, 1, 0xE0]]);
        assert_eq!(enc(&mut c, "kall"), vec![vec![0x03, 0]]);
        assert_eq!(enc(&mut c, "mbr 5"), vec![vec![0x06, 1, 5]]);
        assert_eq!(enc(&mut c, "mba 1"), vec![vec![0x07, 1, 1]]);
        assert!(c.encode("kd 0").is_err());
        assert!(c.encode("kd 0xA5").is_err());
    }

    #[test]
    fn mouse_records() {
        let mut c = BleV1;
        assert_eq!(enc(&mut c, "mm -5 3 0"), vec![vec![0x04, 2, (-5i8) as u8, 3]]);
        assert_eq!(enc(&mut c, "mm 1 2 -1"), vec![vec![0x04, 4, 1, 2, 0xFF, 0]]);
        assert_eq!(enc(&mut c, "ma 32767 0 0"), vec![vec![0x05, 4, 0xFF, 0x7F, 0, 0]]);
        assert_eq!(enc(&mut c, "ma 16384 256 3"), vec![vec![0x05, 6, 0x00, 0x40, 0x00, 0x01, 3, 0]]);
        // se recorta como el firmware
        assert_eq!(enc(&mut c, "ma 99999 -4 0"), vec![vec![0x05, 4, 0xFF, 0x7F, 0, 0]]);
        assert_eq!(enc(&mut c, "mm 500 -500 0"), vec![vec![0x04, 2, 127, (-127i8) as u8]]);
    }

    #[test]
    fn requests_and_unsupported() {
        let mut c = BleV1;
        let s = c.encode("status").unwrap();
        assert!(s.reply && matches!(s.action, Action::Frames(ref f) if f == &vec![vec![0x11, 0]]));
        assert!(matches!(c.encode("info").unwrap().action, Action::ReadInfo));
        assert!(matches!(c.encode("stats"), Err(Error::Usage(_))));
        assert_eq!(enc(&mut c, "mode C"), vec![vec![0x12, 1, 2]]);
        assert_eq!(c.ping(0x1_0003), Some(vec![0x10, 2, 3, 0]));
    }

    #[test]
    fn led_records_info_and_old_firmware() {
        let mut c = BleV1;
        assert_eq!(enc(&mut c, "led auto"), vec![vec![SET_LED, 5, 0, 0, 0, 0, 0]]);
        assert_eq!(enc(&mut c, "led off"), vec![vec![SET_LED, 5, 2, 0, 0, 0, 0]]);
        assert_eq!(enc(&mut c, "led FF8800 60"), vec![vec![SET_LED, 5, 1, 0xFF, 0x88, 0, 60]]);
        assert!(c.encode("led ff8800 101").is_err());
        assert!(c.encode("led rojo").is_err());
        // INFO de 13 bytes: caps bit6 y led=
        assert_eq!(info_text(&[1, 1, 2, 2, 0x7f, 0, 0, 0, 1, 0xFF, 0, 0, 50]), "proto=1 fw=1.2.0 mode=C caps=teclado,raton-rel,raton-abs,msc/sd,set-mode,ping,luz led=FF0000@50");
        // simulador: aplica, valida rangos y, como firmware viejo, responde UNKNOWN_OP -> mensaje claro
        let sim = sim::Sim::new();
        assert!(sim.handle(&enc(&mut c, "led 010203 4").concat()).is_empty());
        assert_eq!(sim.state().led, Some([1, 1, 2, 3, 4]));
        assert_eq!(sim.handle(&record(SET_LED, &[3, 0, 0, 0, 0])), vec![record(ERR, &[SET_LED, 2])]);
        assert_eq!(sim.handle(&record(SET_LED, &[1, 0, 0, 0, 101])), vec![record(ERR, &[SET_LED, 2])]);
        assert_eq!(sim.handle(&record(SET_LED, &[1, 0, 0])), vec![record(ERR, &[SET_LED, 4])]);
        sim.0.lock().unwrap().led = None;
        let r = sim.handle(&enc(&mut c, "led off").concat());
        assert_eq!(r, vec![record(ERR, &[SET_LED, 3])]);
        assert_eq!(c.decode(&r[0]), vec![Decoded::Err(crate::led::UNSUPPORTED.into())]);
    }

    #[test]
    fn type_expands_with_us_layout() {
        let mut c = BleV1;
        let f = enc(&mut c, "type Ab");
        // 'A' = shift+a: kd shift, kd a, ku a, ku shift; 'b' = kd b, ku b
        let flat: Vec<(u8, u8)> = f.iter().map(|r| (r[0], r[2])).collect();
        assert_eq!(flat, vec![(1, 0xE1), (1, 0x04), (2, 0x04), (2, 0xE1), (1, 0x05), (2, 0x05)]);
    }

    #[test]
    fn decode_state_pong_err() {
        let mut c = BleV1;
        // STATE: host+suspend no, sd, hid, sd avail; leds caps; mode C
        let evt = [record(STATE, &[0b0011_0101, 0x02, 2]), record(PONG, &[7, 0, 0x10, 0x27, 0, 0]), record(ERR, &[0x01, 5])].concat();
        let d = c.decode(&evt);
        assert_eq!(d[0], Decoded::Reply("host=1 leds=0x02 hidready=1 susp=0 proto=report sd=1 mode=C".into()));
        assert_eq!(d[1], Decoded::Status("host=1 leds=0x02 hidready=1 susp=0 proto=report sd=1 mode=C".into()));
        assert_eq!(d[2], Decoded::Pong { seq: 7 });
        assert!(matches!(&d[3], Decoded::Err(m) if m.contains("nohost") && m.contains("0x01")), "{:?}", d[3]);
        // el texto de STATE lo entiende parse_status
        let m = crate::protocol::parse_status("host=1 leds=0x02 hidready=1 susp=0 proto=boot sd=1 mode=C").unwrap();
        assert_eq!(m["leds"], 2);
        assert_eq!(m["proto"], "boot");
    }

    #[test]
    fn info_text_and_split() {
        assert_eq!(info_text(&[1, 0, 3, 2, 0b100111, 0, 0, 0]), "proto=1 fw=0.3.0 mode=C caps=teclado,raton-rel,raton-abs,ping");
        assert_eq!(info_text(&[1, 1, 0, 2, 0b111111, 0, 7, 0]), "proto=1 fw=1.0.7 mode=C caps=teclado,raton-rel,raton-abs,msc/sd,set-mode,ping");
        // 6 bytes (sin [6..7]) sigue siendo legible: patch 0
        assert_eq!(info_text(&[1, 1, 2, 0, 1, 0]), "proto=1 fw=1.2.0 mode=A caps=teclado");
        let buf = [record(KEY_DOWN, &[4]), record(RELEASE_ALL, &[])].concat();
        let (r, trunc) = split_records(&buf);
        assert_eq!(r.len(), 2);
        assert!(!trunc);
        let (r, trunc) = split_records(&[0x01, 5, 1]); // len excede
        assert!(r.is_empty() && trunc);
    }

    #[test]
    fn merge_motion() {
        let a = record(MOUSE_REL, &[10, 5]);
        let b = record(MOUSE_REL, &[20, (-3i8) as u8]);
        assert_eq!(merge_records(&a, &b), Some(record(MOUSE_REL, &[30, 2])));
        assert!(merge_records(&record(MOUSE_REL, &[100, 0]), &record(MOUSE_REL, &[100, 0])).is_none(), "se pasa de 127");
        let (x, y) = (record(MOUSE_ABS, &[1, 0, 2, 0]), record(MOUSE_ABS, &[9, 0, 8, 0]));
        assert_eq!(merge_records(&x, &y), Some(y.clone()));
        assert!(merge_records(&a, &y).is_none());
        assert!(merge_records(&record(KEY_DOWN, &[4]), &record(KEY_DOWN, &[4])).is_none());
    }

    #[test]
    fn sim_dongle_follows_the_spec() {
        let sim = sim::Sim::new();
        let mut c = BleV1;
        let w = [enc(&mut c, "kd 4"), enc(&mut c, "kd 0xE0"), enc(&mut c, "mm 3 4 0"), enc(&mut c, "mba 1")].concat().concat();
        assert!(sim.handle(&w).is_empty());
        let s = sim.state();
        assert!(s.keys.contains(&4) && s.keys.contains(&0xE0));
        assert_eq!(s.rel_total, (3, 4));
        assert_eq!(s.buttons_abs, 1);
        let r = sim.handle(&enc(&mut c, "kall").concat());
        assert!(r.is_empty());
        assert!(sim.state().keys.is_empty() && sim.state().buttons_abs == 0);
        // sin host: ERR NO_HOST
        sim.set_flags(0b0001_0000);
        let r = sim.handle(&enc(&mut c, "kd 4").concat());
        assert_eq!(r, vec![record(ERR, &[KEY_DOWN, 5])]);
        // modo sin HID
        sim.set_flags(0);
        assert_eq!(sim.handle(&enc(&mut c, "kd 4").concat()), vec![record(ERR, &[KEY_DOWN, 6])]);
        // ping
        let r = sim.handle(&c.ping(9).unwrap());
        let d = c.decode(&r[0]);
        assert_eq!(d, vec![Decoded::Pong { seq: 9 }]);
        // registro desconocido se salta con UNKNOWN_OP; truncado -> MALFORMED
        let r = sim.handle(&[0x55, 1, 0, 0x11, 0]);
        assert_eq!(r[0][..4], [ERR, 2, 0x55, 3]);
        let r = sim.handle(&[0x01, 9, 1]);
        assert_eq!(r[0], record(ERR, &[0, 4]));
    }

    #[test]
    fn incompatible_protocol_version_is_refused_and_info_reads() {
        use crate::transport::ble::{mockble::MockDongle, BleOptions, BleTransport};
        use crate::transport::{LinkState, Transport};
        let d = MockDongle::new();
        d.set_info(&[2, 0, 1, 2, 0, 0, 0, 0]);
        let t = BleTransport::spawn(d.connector(), Box::new(BleV1), BleOptions::default());
        // espera por condición (hasta 20 s), no un sleep fijo
        let t0 = std::time::Instant::now();
        loop {
            match t.link_state() {
                Some(LinkState::Reconnecting { reason, .. }) if reason.contains("v2") => break,
                other => assert!(t0.elapsed() < std::time::Duration::from_secs(20), "debía rechazar el dongle: {other:?}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(t);
        // y con v1 la orden `info` devuelve la lectura de INFO ya formateada
        let d = MockDongle::new();
        let mut t = BleTransport::open(d.connector(), Box::new(BleV1), BleOptions::default(), std::time::Duration::from_secs(20)).unwrap();
        let r = t.send("info", true).unwrap().unwrap();
        assert!(r.starts_with("proto=1 fw=0.1.0 mode=C caps=teclado,raton-rel,raton-abs"), "{r}");
        assert_eq!(crate::FwVersion::from_info_reply(&r), Some(crate::FwVersion::new(0, 1, 0)));
    }
}
