//! Luz del dongle (LED RGB APA102), firmware 1.2.0+: automática (color según modo / BLE / fallo de SD), color fijo
//! con brillo, o apagada. Texto común a CDC, BLE (el codec lo traduce a SET_LED), CLI, GUI y `led.txt`:
//! `auto` | `off` | `RRGGBB@NN` (lo que devuelve `info` en `led=`) y la orden `led auto|off|RRGGBB [brillo]`.

use crate::error::{Error, Result};
use std::fmt;

/// Brillo por defecto del color fijo: el mismo que el modo automático (90/255).
pub const DEFAULT_BRIGHT: u8 = 35;
/// Mensaje cuando el firmware no conoce la orden (anterior a 1.2.0).
pub const UNSUPPORTED: &str = "el firmware no admite control de la luz: actualízalo (1.2.0 o posterior)";
/// Fichero del directorio de estado donde la GUI guarda el ajuste.
pub const STATE_FILE: &str = "led.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Led {
    Auto,
    Off,
    /// Color fijo; `bright` en % (0..100).
    Color { rgb: [u8; 3], bright: u8 },
}

impl Led {
    /// `auto`, `off`, `RRGGBB` (admite `#`), `RRGGBB@NN` o `RRGGBB NN`. Sin brillo: [`DEFAULT_BRIGHT`].
    pub fn parse(s: &str) -> Result<Led> {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "auto" | "automatico" | "automático" => return Ok(Led::Auto),
            "off" | "apagado" | "apagada" => return Ok(Led::Off),
            _ => {}
        }
        let (hex, br) = match s.split_once(['@', ' ']) {
            Some((h, b)) => (h, Some(b.trim())),
            None => (s, None),
        };
        let rgb = parse_hex(hex).ok_or_else(|| Error::Usage(format!("luz: usa auto, off o un color RRGGBB (p. ej. ff8800), no {s:?}")))?;
        let bright = match br {
            None => DEFAULT_BRIGHT,
            Some(b) => parse_bright(b)?,
        };
        Ok(Led::Color { rgb, bright })
    }

    /// Orden de texto (CDC; el codec BLE la traduce a SET_LED).
    pub fn cmd(&self) -> String {
        match self {
            Led::Auto => "led auto".into(),
            Led::Off => "led off".into(),
            Led::Color { rgb: [r, g, b], bright } => format!("led {r:02X}{g:02X}{b:02X} {bright}"),
        }
    }

    /// Payload de SET_LED: `mode, r, g, b, bright`. En auto/apagado el color va a 0 y el firmware lo guarda igual.
    pub fn payload(&self) -> [u8; 5] {
        match *self {
            Led::Auto => [0, 0, 0, 0, 0],
            Led::Off => [2, 0, 0, 0, 0],
            Led::Color { rgb: [r, g, b], bright } => [1, r, g, b, bright],
        }
    }

    /// INFO BLE [8..12] -> ajuste. `None` si el firmware no lo envía (INFO de 8 bytes) o es ilegible.
    pub fn from_info_bytes(b: &[u8]) -> Option<Led> {
        let p = b.get(8..13)?;
        match p[0] {
            0 => Some(Led::Auto),
            1 if p[4] <= 100 => Some(Led::Color { rgb: [p[1], p[2], p[3]], bright: p[4] }),
            2 => Some(Led::Off),
            _ => None,
        }
    }

    /// Campo `led=` de la respuesta de `info` (CDC o BLE). `None` = firmware sin control de la luz.
    pub fn from_info_reply(reply: &str) -> Option<Led> {
        reply.split_whitespace().find_map(|t| t.strip_prefix("led=")).and_then(|v| Led::parse(v).ok())
    }

    /// Ajuste guardado por la GUI (`led.txt`), si hay y es válido.
    pub fn load_saved() -> Option<Led> {
        Led::parse(&crate::config::load(STATE_FILE)?).ok()
    }

    pub fn save(&self) {
        crate::config::save(STATE_FILE, &self.to_string());
    }
}

impl fmt::Display for Led {
    /// `auto` | `off` | `RRGGBB@NN` (como el `led=` del firmware).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Led::Auto => f.write_str("auto"),
            Led::Off => f.write_str("off"),
            Led::Color { rgb: [r, g, b], bright } => write!(f, "{r:02X}{g:02X}{b:02X}@{bright}"),
        }
    }
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Brillo 0..100 (admite `%`).
pub fn parse_bright(s: &str) -> Result<u8> {
    let t = s.trim().trim_end_matches('%');
    match t.parse::<u32>() {
        Ok(v) if v <= 100 => Ok(v as u8),
        _ => Err(Error::Usage(format!("brillo fuera de rango (0..100): {s:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_round_trip() {
        assert_eq!(Led::parse("auto").unwrap(), Led::Auto);
        assert_eq!(Led::parse(" OFF ").unwrap(), Led::Off);
        let c = Led::parse("#ff8800").unwrap();
        assert_eq!(c, Led::Color { rgb: [0xFF, 0x88, 0], bright: DEFAULT_BRIGHT });
        assert_eq!(Led::parse("00ff46@80").unwrap(), Led::Color { rgb: [0, 0xFF, 0x46], bright: 80 });
        assert_eq!(Led::parse("00FF46 5").unwrap(), Led::Color { rgb: [0, 0xFF, 0x46], bright: 5 });
        for l in [Led::Auto, Led::Off, Led::Color { rgb: [1, 2, 3], bright: 100 }] {
            assert_eq!(Led::parse(&l.to_string()).unwrap(), l);
        }
        assert_eq!(Led::Color { rgb: [0xAB, 0, 1], bright: 7 }.to_string(), "AB0001@7");
        assert_eq!(Led::Color { rgb: [0xAB, 0, 1], bright: 7 }.cmd(), "led AB0001 7");
        for bad in ["", "rojo", "ff00", "ff00000", "ff0000@101", "ff0000@-1", "gg0000"] {
            assert!(Led::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn info_fields() {
        let cdc = "fw=1.2.0 name=RiotKVM proto=1 mode=C led=FF0000@50 rst=1";
        assert_eq!(Led::from_info_reply(cdc), Some(Led::Color { rgb: [255, 0, 0], bright: 50 }));
        assert_eq!(Led::from_info_reply("fw=1.1.0 name=RiotKVM proto=1 mode=C rst=1"), None);
        assert_eq!(Led::from_info_bytes(&[1, 1, 2, 2, 0x7f, 0, 0, 0, 2, 0, 0, 0, 0]), Some(Led::Off));
        assert_eq!(Led::from_info_bytes(&[1, 1, 2, 2, 0x7f, 0, 0, 0, 1, 9, 8, 7, 60]), Some(Led::Color { rgb: [9, 8, 7], bright: 60 }));
        assert_eq!(Led::from_info_bytes(&[1, 1, 1, 2, 0x3f, 0, 0, 0]), None);
        assert_eq!(Led::Color { rgb: [9, 8, 7], bright: 60 }.payload(), [1, 9, 8, 7, 60]);
    }
}
