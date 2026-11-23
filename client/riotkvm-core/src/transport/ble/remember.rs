//! Recuerda el último dongle conectado (identificador del sistema) para preferirlo al reconectar.

use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    Some(crate::config::state_dir()?.join("ble_last.txt"))
}

pub fn load() -> Option<String> {
    let s = std::fs::read_to_string(path()?).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn save(id: &str) {
    if let Some(p) = path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, id);
    }
}
