//! Recuerda el último dongle conectado (identificador del sistema) para preferirlo al reconectar.

use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("DKVM_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("riotkvm")))
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|a| PathBuf::from(a).join("riotkvm")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/riotkvm")))?;
    Some(base.join("ble_last.txt"))
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
