//! Versión del firmware del dongle (semver `X.Y.Z`), para comparar con la última release publicada.
//!
//! Fuentes: INFO BLE (bytes [1] major, [2] minor, [6] patch; un firmware anterior deja [6] a 0) y el campo
//! `fw=` de la respuesta a `info` (CDC: `fw=1.0.0 name=RiotKVM ...`; BLE ya formateado: `proto=1 fw=1.0.0 ...`).
//! Firmwares previos al versionado responden `fw=etapa2` por CDC (no es una versión: `None`) y 1.0 por INFO BLE
//! (`1.0.0`); la primera versión semver es la 1.1.0, así que ambos casos quedan por debajo de cualquier release.

use std::fmt;
use std::str::FromStr;

/// Versión semver del firmware. El orden es el de semver (major, luego minor, luego patch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FwVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl FwVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> FwVersion {
        FwVersion { major, minor, patch }
    }

    /// Desde la característica INFO BLE (8 bytes). `None` si es demasiado corta.
    pub fn from_info(b: &[u8]) -> Option<FwVersion> {
        if b.len() < 3 {
            return None;
        }
        Some(FwVersion::new(b[1] as u32, b[2] as u32, b.get(6).copied().unwrap_or(0) as u32))
    }

    /// Busca el campo `fw=` en la respuesta de `info` (CDC o BLE). `None` si falta o no es semver.
    pub fn from_info_reply(reply: &str) -> Option<FwVersion> {
        reply.split_whitespace().find_map(|t| t.strip_prefix("fw=")).and_then(|v| v.parse().ok())
    }
}

impl fmt::Display for FwVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Acepta `X.Y.Z`, `X.Y` (patch 0, como lo mostraban clientes antiguos) y un prefijo `v` opcional
/// (las etiquetas de GitHub). Nada de sufijos de pre-release.
impl FromStr for FwVersion {
    type Err = String;

    fn from_str(s: &str) -> Result<FwVersion, String> {
        let bad = || format!("versión no válida: {s}");
        let t = s.trim();
        let parts: Vec<&str> = t.strip_prefix('v').unwrap_or(t).split('.').collect();
        if !(2..=3).contains(&parts.len()) {
            return Err(bad());
        }
        // solo dígitos: u32::from_str aceptaría `+1`
        let n = |p: &str| if p.bytes().all(|c| c.is_ascii_digit()) { p.parse::<u32>().map_err(|_| bad()) } else { Err(bad()) };
        Ok(FwVersion::new(n(parts[0])?, n(parts[1])?, if parts.len() == 3 { n(parts[2])? } else { 0 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_and_order() {
        let v: FwVersion = "1.2.3".parse().unwrap();
        assert_eq!(v, FwVersion::new(1, 2, 3));
        assert_eq!(v.to_string(), "1.2.3");
        assert_eq!("v1.0.0".parse::<FwVersion>().unwrap(), FwVersion::new(1, 0, 0));
        assert_eq!("1.0".parse::<FwVersion>().unwrap(), FwVersion::new(1, 0, 0));
        assert_eq!(" v4.5.6\n".parse::<FwVersion>().unwrap(), FwVersion::new(4, 5, 6));
        for bad in ["etapa2", "mock", "1", "1.2.3.4", "1..2", "1.2.x", "1.2.3-rc1", "", "v", "+1.2.3", "1.-2.3", "99999999999.0.0"] {
            assert!(bad.parse::<FwVersion>().is_err(), "{bad}");
        }
        assert!(FwVersion::new(1, 0, 10) > FwVersion::new(1, 0, 9));
        assert!(FwVersion::new(1, 1, 0) > FwVersion::new(1, 0, 99));
        assert!(FwVersion::new(2, 0, 0) > FwVersion::new(1, 99, 99));
    }

    #[test]
    fn from_info_bytes_and_reply() {
        assert_eq!(FwVersion::from_info(&[1, 1, 0, 2, 0x3f, 0, 4, 0]), Some(FwVersion::new(1, 0, 4)));
        // firmware sin patch en INFO: [6] reservado a 0
        assert_eq!(FwVersion::from_info(&[1, 1, 0, 2, 0x3f, 0]), Some(FwVersion::new(1, 0, 0)));
        assert_eq!(FwVersion::from_info(&[1, 1]), None);
        let cdc = "fw=1.0.0 name=RiotKVM proto=1 mode=C rst=1 crashes=0 safe=0 ble_started=1 [x] build=\"Nov 23 2026 10:00:00\"";
        assert_eq!(FwVersion::from_info_reply(cdc), Some(FwVersion::new(1, 0, 0)));
        assert_eq!(FwVersion::from_info_reply("proto=1 fw=1.2.3 mode=C caps=teclado"), Some(FwVersion::new(1, 2, 3)));
        assert_eq!(FwVersion::from_info_reply("fw=etapa2 name=RiotKVM proto=1"), None);
        assert_eq!(FwVersion::from_info_reply("fw=mock build=\"mock\""), None);
        assert_eq!(FwVersion::from_info_reply("ok"), None);
    }
}
