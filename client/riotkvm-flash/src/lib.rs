//! riotkvm-flash: herramientas del RiotKVM sin GUI (las usan `riotkvm-gui` y `riotkvm`).
//!
//! - [`release`]: última release de GitHub (`manifest.json`, `SHA256SUMS.txt`), descarga verificada del firmware.
//! - [`usb`]: qué hay enchufado (dongle RiotKVM en modo A/B/C, ROM de descarga de Espressif, otros).
//! - [`flash`]: flasheo del ESP32 con `espflash` (dongle -> `bootloader` -> ROM -> escribir en 0x0 -> reset -> `info`).
//! - [`ventoy`]: microSD con Ventoy en modo MBR escrita por nosotros (Ventoy no tiene instalador para macOS).
//! - [`exfat`]: formateador exFAT propio (la partición 1 de Ventoy), para escribirlo todo por un único descriptor.
//! - [`disks`] y [`rawdev`]: discos externos candidatos y apertura del dispositivo crudo (authopen/udisks2/fichero).
//!
//! Todo lo que escribe en un dispositivo acepta también un fichero imagen: así se prueba sin tocar hardware.

pub mod disks;
pub mod exfat;
pub mod flash;
pub mod http;
pub mod rawdev;
pub mod release;
pub mod usb;
pub mod ventoy;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Error de las herramientas: mensaje para el usuario (en castellano) o cancelación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Msg(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn msg(m: impl Into<String>) -> Error {
        Error::Msg(m.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => f.write_str("cancelado"),
            Error::Msg(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Msg(e.to_string())
    }
}

/// Señal de cancelación compartida entre la UI y el hilo de trabajo.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    /// `Err(Cancelled)` si ya se pidió cancelar.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Avance de un trabajo largo: etapa legible y bytes hechos/total (total 0 = indeterminado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub stage: String,
    pub done: u64,
    pub total: u64,
}

/// Receptor de avances y mensajes (la GUI lo convierte en eventos de canal; la CLI imprime).
pub trait Reporter: Send {
    fn progress(&self, p: Progress);
    fn log(&self, line: &str);
}

/// Reporter que no hace nada (pruebas).
pub struct Silent;

impl Reporter for Silent {
    fn progress(&self, _p: Progress) {}
    fn log(&self, _line: &str) {}
}

pub fn stage(r: &dyn Reporter, s: impl Into<String>) {
    r.progress(Progress { stage: s.into(), done: 0, total: 0 });
}

/// SHA-256 en hexadecimal minúsculas.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

/// Directorio de estado/caché del cliente (`~/.local/state/riotkvm`, `%APPDATA%\riotkvm`, `DKVM_STATE_DIR`).
pub fn state_dir() -> Result<std::path::PathBuf> {
    riotkvm_core::config::state_dir().ok_or_else(|| Error::msg("no hay directorio de estado (HOME/APPDATA sin definir)"))
}

/// Tamaño legible (GB/MB decimales, como los anuncian los fabricantes de tarjetas).
pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.1} kB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}
