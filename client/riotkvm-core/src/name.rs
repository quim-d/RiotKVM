//! Nombre del dongle (firmware 1.3.0+), por si tienes varios: es su nombre BLE (el UUID del servicio no cambia, así que
//! se le sigue encontrando por UUID) y el de la cabecera de su pantalla. Persistente en el dongle; de fábrica `RiotKVM`.
//! CDC: `name` / `name NUEVO` e `info` (`name=`); BLE: SET_NAME (0x14) e INFO [13..] (caps bit7).

use crate::error::{Error, Result};
use crate::FwVersion;

/// Nombre de fábrica.
pub const DEFAULT: &str = "RiotKVM";
/// Longitud máxima (cabe en el scan response legacy junto a su cabecera).
pub const MAX_LEN: usize = 20;
/// Primer firmware que admite renombrar.
pub const MIN_FW: FwVersion = FwVersion::new(1, 3, 0);
/// Mensaje cuando el firmware no conoce la orden (anterior a 1.3.0).
pub const UNSUPPORTED: &str = "el firmware no admite renombrar el dongle: actualízalo (1.3.0 o posterior)";

/// 1..20 caracteres de `A-Z a-z 0-9 . _ -` (sin espacios: el `info` del CDC lo muestra como `name=` sin comillas).
pub fn validate(s: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() || t.len() > MAX_LEN || !t.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')) {
        return Err(Error::Usage(format!("nombre no válido {t:?}: 1..{MAX_LEN} caracteres de A-Z a-z 0-9 . _ - (sin espacios)")));
    }
    Ok(t.to_string())
}

/// Orden de texto (CDC; el codec BLE la traduce a SET_NAME).
pub fn cmd(name: &str) -> String {
    format!("name {name}")
}

/// Payload de SET_NAME: `n, nombre`.
pub fn payload(name: &str) -> Vec<u8> {
    let mut p = vec![name.len() as u8];
    p.extend_from_slice(name.as_bytes());
    p
}

/// INFO BLE [13..] -> nombre. `None` si el firmware no lo envía (sin caps bit7) o es ilegible.
pub fn from_info_bytes(b: &[u8]) -> Option<String> {
    let caps = u16::from_le_bytes([*b.get(4)?, *b.get(5)?]);
    if caps & 0x80 == 0 {
        return None;
    }
    let n = *b.get(13)? as usize;
    let s = std::str::from_utf8(b.get(14..14 + n)?).ok()?;
    validate(s).ok()
}

/// Campo `name=` de la respuesta de `info` (CDC o BLE ya formateado).
pub fn from_info_reply(reply: &str) -> Option<String> {
    reply.split_whitespace().find_map(|t| t.strip_prefix("name=")).map(str::to_string)
}

/// Nombre configurable según `info`: `Some(nombre)` si el firmware admite renombrar, `None` si no (anterior a 1.3.0:
/// su `info` CDC dice siempre `name=RiotKVM` y su INFO BLE no lleva nombre). Una versión no semver (mock) cuenta como sí.
pub fn renameable(reply: &str) -> Option<String> {
    let name = from_info_reply(reply)?;
    match FwVersion::from_info_reply(reply) {
        Some(v) if v < MIN_FW => None,
        None if reply.contains("fw=etapa") => None,
        _ => Some(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_names() {
        assert_eq!(validate(" Rack-3_a.b ").unwrap(), "Rack-3_a.b");
        assert_eq!(validate("12345678901234567890").unwrap().len(), 20);
        for bad in ["", "   ", "con espacio", "ñandú", "a=b", "123456789012345678901", "x\"y"] {
            assert!(validate(bad).is_err(), "{bad:?}");
        }
        assert_eq!(cmd("Rack3"), "name Rack3");
        assert_eq!(payload("ab"), vec![2, b'a', b'b']);
    }

    #[test]
    fn info_fields() {
        let mut b = vec![1, 1, 3, 2, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 5];
        b.extend_from_slice(b"Rack3");
        assert_eq!(from_info_bytes(&b), Some("Rack3".into()));
        b[4] = 0x7f; // sin caps bit7
        assert_eq!(from_info_bytes(&b), None);
        assert_eq!(from_info_bytes(&[1, 1, 2, 2, 0x7f, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(renameable("fw=1.3.0 name=Rack3 proto=1"), Some("Rack3".into()));
        assert_eq!(renameable("fw=1.2.0 name=RiotKVM proto=1"), None, "1.2.0 no renombra");
        assert_eq!(renameable("fw=etapa2 name=QKVM"), None);
        assert_eq!(renameable("proto=1 fw=1.2.0 mode=C caps=teclado"), None);
        assert_eq!(renameable("fw=mock name=Mock led=auto"), Some("Mock".into()));
    }
}
