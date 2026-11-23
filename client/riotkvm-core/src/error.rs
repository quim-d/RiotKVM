//! Errores del cliente y su código de salida de la CLI.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Argumento o uso incorrecto (2).
    Usage(String),
    /// No hay dongle / no se pudo abrir el puerto (3).
    DeviceNotFound(String),
    /// El dongle respondió `err ...` (4).
    Device(String),
    /// Fallo de E/S, timeout o respuesta ilegible (5).
    Transport(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::DeviceNotFound(_) => 3,
            Error::Device(_) => 4,
            Error::Transport(_) => 5,
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Error::Usage(m) | Error::DeviceNotFound(m) | Error::Device(m) | Error::Transport(m) => m,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

pub fn usage<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Usage(msg.into()))
}

/// Error `err ...` del dongle con una pista legible según el código.
pub fn device_error(line: &str, reply: &str) -> Error {
    if line.split_whitespace().next() == Some("led") && reply.trim() == "err unknown" {
        return Error::Device(crate::led::UNSUPPORTED.into()); // firmware anterior a 1.2.0
    }
    let hint = match reply.split_whitespace().nth(1) {
        Some("nohost") => " (el servidor no ha enumerado el dongle: ¿USB conectado y servidor encendido?)",
        Some("bootproto") => " (el servidor está en protocolo de arranque/BIOS: el ratón no está disponible; usa el teclado)",
        Some("busy") => " (endpoint USB ocupado o servidor suspendido: el informe se perdió)",
        Some("overflow") => " (línea de más de 255 bytes)",
        Some("rollover") => " (más de 6 teclas pulsadas a la vez)",
        _ => "",
    };
    Error::Device(format!("el dongle rechazó {}: {}{}", pyrepr(line), reply, hint))
}

/// Representación estilo Python (`repr`) de una cadena, para mensajes idénticos a la CLI de referencia.
pub fn pyrepr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c)
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}
