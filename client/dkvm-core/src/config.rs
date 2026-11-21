//! Configuración persistente pequeña, compartida por la GUI y la CLI: `%APPDATA%\dkvm\` (o `DKVM_STATE_DIR`,
//! `~/.local/state/dkvm`). Un fichero de texto por valor.

use std::path::PathBuf;

pub const DEFAULT_LAYOUT: &str = "es";

pub fn state_dir() -> Option<PathBuf> {
    std::env::var_os("DKVM_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("dkvm")))
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|a| PathBuf::from(a).join("dkvm")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/dkvm")))
}

pub fn load(name: &str) -> Option<String> {
    let s = std::fs::read_to_string(state_dir()?.join(name)).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn save(name: &str, value: &str) {
    if let Some(d) = state_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(name), value);
    }
}

/// Distribución del teclado del SERVIDOR guardada (nombre canónico), si hay y es válida.
pub fn load_layout() -> Option<String> {
    crate::layouts::canonical(&load("layout.txt")?).map(String::from)
}

pub fn save_layout(name: &str) {
    if let Some(c) = crate::layouts::canonical(name) {
        save("layout.txt", c);
    }
}

/// Layout efectivo: el pedido explícito (flag/variable de entorno) > el guardado > `es`.
pub fn effective_layout(explicit: Option<&str>) -> String {
    explicit.map(String::from).or_else(load_layout).unwrap_or_else(|| DEFAULT_LAYOUT.to_string())
}
