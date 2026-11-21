//! Protocolo de texto actual del dongle (CDC). Una línea por orden, respuesta `ok` | `err ...`.
//!
//! Órdenes (src/main.cpp): type, kd, ku, kall, mm, ma, mbr, mba, status, bootloader.
//! Todo lo que sea layout/texto->HID se resuelve en el cliente; aquí solo se codifica.

use crate::error::{usage, Error, Result};
use serde_json::{Map, Value};

pub const ABS_MAX: i64 = 32767;
/// Máximo de bytes de texto por orden `type` (línea de 255 bytes del firmware menos "type ").
pub const TYPE_CHUNK: usize = 240;
pub const BTN_LEFT: u8 = 1;
pub const BTN_RIGHT: u8 = 2;
pub const BTN_MIDDLE: u8 = 4;

fn rng(v: i64, lo: i64, hi: i64, what: &str) -> Result<i64> {
    if !(lo..=hi).contains(&v) {
        return usage(format!("{what}={v} fuera de rango [{lo}, {hi}]"));
    }
    Ok(v)
}

pub fn type_text(text: &str) -> Result<String> {
    if text.contains('\n') || text.contains('\r') {
        return usage("'type' del firmware no admite saltos de línea");
    }
    Ok(format!("type {text}"))
}

pub fn key_down(usage: i64) -> Result<String> {
    Ok(format!("kd 0x{:02X}", rng(usage, 1, 0xE7, "usage")?))
}

pub fn key_up(usage: i64) -> Result<String> {
    Ok(format!("ku 0x{:02X}", rng(usage, 1, 0xE7, "usage")?))
}

pub fn release_all_keys() -> String {
    "kall".into()
}

pub fn mouse_rel(dx: i64, dy: i64, wheel: i64) -> Result<String> {
    Ok(format!(
        "mm {} {} {}",
        rng(dx, -128, 127, "dx")?,
        rng(dy, -128, 127, "dy")?,
        rng(wheel, -128, 127, "wheel")?
    ))
}

pub fn mouse_abs(x: i64, y: i64, wheel: i64) -> Result<String> {
    Ok(format!(
        "ma {} {} {}",
        rng(x, 0, ABS_MAX, "x")?,
        rng(y, 0, ABS_MAX, "y")?,
        rng(wheel, -128, 127, "wheel")?
    ))
}

pub fn buttons_rel(mask: i64) -> Result<String> {
    Ok(format!("mbr {}", rng(mask, 0, 7, "botones")?))
}

pub fn buttons_abs(mask: i64) -> Result<String> {
    Ok(format!("mba {}", rng(mask, 0, 7, "botones")?))
}

pub fn info() -> String {
    "info".into()
}

/// `stats` (sin `reset`) responde dos líneas; el resto, una.
pub fn stats(reset: bool) -> String {
    if reset { "stats reset".into() } else { "stats".into() }
}

/// Líneas de respuesta que produce una orden.
pub fn reply_lines(line: &str) -> usize {
    let mut it = line.split_whitespace();
    if it.next() == Some("stats") && it.next() != Some("reset") {
        2
    } else {
        1
    }
}

pub fn status() -> String {
    "status".into()
}

/// Como `int(s, 0)` de Python (prefijos 0x/0o/0b, signo).
pub fn parse_int0(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let lower = body.to_ascii_lowercase();
    let v = if let Some(h) = lower.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()?
    } else if let Some(o) = lower.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()?
    } else if let Some(b) = lower.strip_prefix("0b") {
        i64::from_str_radix(b, 2).ok()?
    } else {
        if lower.is_empty() || !lower.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        lower.parse::<i64>().ok()?
    };
    Some(if neg { -v } else { v })
}

/// `host=1 leds=0x02 hidready=1` -> {"host": true, "leds": 2, "hidready": true} (orden de aparición).
pub fn parse_status(reply: &str) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for part in reply.split_whitespace() {
        let Some((k, v)) = part.split_once('=') else { continue };
        let val = match parse_int0(v) {
            None => Value::String(v.to_string()),
            Some(n) if matches!(k, "host" | "hidready" | "susp" | "sd") => Value::Bool(n != 0),
            Some(n) => Value::from(n),
        };
        out.insert(k.to_string(), val);
    }
    if out.is_empty() {
        return Err(Error::Transport(format!(
            "respuesta de status ilegible: {}",
            crate::error::pyrepr(reply)
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding() {
        assert_eq!(key_down(0x04).unwrap(), "kd 0x04");
        assert_eq!(mouse_rel(-5, 3, 0).unwrap(), "mm -5 3 0");
        assert_eq!(mouse_abs(32767, 0, 0).unwrap(), "ma 32767 0 0");
        assert_eq!(buttons_abs(5).unwrap(), "mba 5");
    }

    #[test]
    fn ranges() {
        assert!(mouse_rel(128, 0, 0).is_err());
        assert!(mouse_abs(-1, 0, 0).is_err());
        assert!(mouse_abs(32768, 0, 0).is_err());
        assert!(buttons_abs(8).is_err());
        assert!(key_down(0).is_err());
        assert!(type_text("a\nb").is_err());
        assert_eq!(key_down(0).unwrap_err().exit_code(), 2);
    }

    #[test]
    fn status_parse() {
        let m = parse_status("host=1 leds=0x02 hidready=0").unwrap();
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"host":true,"leds":2,"hidready":false}"#);
        assert!(parse_status("basura").is_err());
    }

    #[test]
    fn int0() {
        assert_eq!(parse_int0("0x1F"), Some(31));
        assert_eq!(parse_int0("-5"), Some(-5));
        assert_eq!(parse_int0("zz"), None);
        assert_eq!(parse_int0(""), None);
    }
}
