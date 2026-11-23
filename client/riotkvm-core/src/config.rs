//! Configuración persistente pequeña, compartida por la GUI y la CLI: `%APPDATA%\riotkvm\` (o `RIOTKVM_STATE_DIR`,
//! `$XDG_STATE_HOME/riotkvm`, `~/.local/state/riotkvm`). Un fichero de texto por valor.
//!
//! Compatibilidad con el nombre anterior (dkvm): las variables `DKVM_*` valen como alias de `RIOTKVM_*` ([`env`],
//! [`adopt_legacy_env`]) y el directorio de estado antiguo (`dkvm`) se migra al nuevo la primera vez ([`state_dir`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const DEFAULT_LAYOUT: &str = "es";

/// Prefijo de las variables de entorno del cliente.
pub const ENV_PREFIX: &str = "RIOTKVM_";
/// Prefijo anterior (`DKVM_*`): se sigue aceptando como alias; manda `RIOTKVM_*` si están las dos.
pub const LEGACY_ENV_PREFIX: &str = "DKVM_";

/// Nombre del directorio de estado y el anterior (se migra, ver [`state_dir`]).
const STATE_NAME: &str = "riotkvm";
const LEGACY_STATE_NAME: &str = "dkvm";

/// Variable de entorno `RIOTKVM_<name>` o, si no está, su alias antiguo `DKVM_<name>`.
pub fn env_os(name: &str) -> Option<OsString> {
    std::env::var_os(format!("{ENV_PREFIX}{name}")).or_else(|| std::env::var_os(format!("{LEGACY_ENV_PREFIX}{name}")))
}

/// Como [`env_os`], en UTF-8 (un valor no UTF-8 cuenta como ausente).
pub fn env(name: &str) -> Option<String> {
    env_os(name).and_then(|v| v.into_string().ok())
}

/// Para lo que lee el entorno por su cuenta (los `env = "RIOTKVM_..."` de clap): copia cada `DKVM_X` a
/// `RIOTKVM_X` si esta no está definida. Llamar al principio de `main`, antes de crear hilos.
pub fn adopt_legacy_env() {
    let legacy: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.to_str()?.strip_prefix(LEGACY_ENV_PREFIX)?.into(), v)))
        .collect();
    for (name, v) in legacy {
        let new = format!("{ENV_PREFIX}{}", name.to_string_lossy());
        if std::env::var_os(&new).is_none() {
            std::env::set_var(new, v);
        }
    }
}

/// Directorio de estado. Si es el de por defecto (no `RIOTKVM_STATE_DIR`) y aún no existe pero sí el del nombre
/// anterior (`%APPDATA%\dkvm`, `~/.local/state/dkvm`...), se migra antes: ajustes, último dongle, cachés.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(d) = env_os("STATE_DIR") {
        return Some(PathBuf::from(d));
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    let dir = base.join(STATE_NAME);
    migrate_state_dir(&base.join(LEGACY_STATE_NAME), &dir);
    Some(dir)
}

/// Mueve `old` a `new` si existe `old` y no `new` (renombrado; si no se puede, p. ej. otro volumen, copia y deja
/// el viejo). Devuelve si migró. Nunca pisa un `new` que ya exista.
pub fn migrate_state_dir(old: &Path, new: &Path) -> bool {
    if new.exists() || !old.is_dir() {
        return false;
    }
    if std::fs::rename(old, new).is_ok() {
        return true;
    }
    let ok = copy_dir(old, new).is_ok();
    if !ok {
        let _ = std::fs::remove_dir_all(new); // a medias no: que el próximo arranque lo reintente
    }
    ok
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dst = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dst)?;
        } else {
            std::fs::copy(e.path(), dst)?;
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("riotkvm-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn migra_el_directorio_antiguo_con_sus_ajustes() {
        let base = tmp("migra");
        let (old, new) = (base.join("dkvm"), base.join("riotkvm"));
        std::fs::create_dir_all(old.join("ventoy")).unwrap();
        for (f, v) in [("target.txt", "macos"), ("video_mode.txt", "1280x720@60"), ("paste.txt", "x"), ("ble_last.txt", "AA:BB"), ("layout.txt", "us")] {
            std::fs::write(old.join(f), v).unwrap();
        }
        std::fs::write(old.join("ventoy").join("ventoy-1.1.07-linux.tar.gz"), b"tgz").unwrap();
        assert!(migrate_state_dir(&old, &new));
        assert!(!old.exists(), "renombrado, no copiado");
        assert_eq!(std::fs::read_to_string(new.join("target.txt")).unwrap(), "macos");
        assert_eq!(std::fs::read_to_string(new.join("ble_last.txt")).unwrap(), "AA:BB");
        assert_eq!(std::fs::read(new.join("ventoy").join("ventoy-1.1.07-linux.tar.gz")).unwrap(), b"tgz");
        // segunda vez: nada que hacer
        assert!(!migrate_state_dir(&old, &new));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_pisa_un_directorio_nuevo_existente() {
        let base = tmp("nopisa");
        let (old, new) = (base.join("dkvm"), base.join("riotkvm"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("layout.txt"), "us").unwrap();
        std::fs::write(new.join("layout.txt"), "de").unwrap();
        assert!(!migrate_state_dir(&old, &new));
        assert_eq!(std::fs::read_to_string(new.join("layout.txt")).unwrap(), "de");
        assert!(old.exists());
        // sin el viejo tampoco hace nada
        assert!(!migrate_state_dir(&base.join("no-existe"), &base.join("otro")));
        assert!(!base.join("otro").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn copia_recursiva() {
        let base = tmp("copia");
        std::fs::create_dir_all(base.join("a/firmware")).unwrap();
        std::fs::write(base.join("a/firmware/riotkvm-fw-1.1.0.factory.bin"), b"fw").unwrap();
        std::fs::write(base.join("a/target.txt"), "linux").unwrap();
        copy_dir(&base.join("a"), &base.join("b")).unwrap();
        assert_eq!(std::fs::read(base.join("b/firmware/riotkvm-fw-1.1.0.factory.bin")).unwrap(), b"fw");
        assert_eq!(std::fs::read_to_string(base.join("b/target.txt")).unwrap(), "linux");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn alias_de_entorno() {
        // nombres propios del test: el entorno es global del proceso
        std::env::set_var("DKVM_ZZ_ALIAS_TEST", "viejo");
        assert_eq!(env("ZZ_ALIAS_TEST").as_deref(), Some("viejo"));
        std::env::set_var("RIOTKVM_ZZ_ALIAS_TEST", "nuevo");
        assert_eq!(env("ZZ_ALIAS_TEST").as_deref(), Some("nuevo"), "manda RIOTKVM_");
        std::env::set_var("DKVM_ZZ_ADOPT_TEST", "1");
        adopt_legacy_env();
        assert_eq!(std::env::var("RIOTKVM_ZZ_ADOPT_TEST").as_deref(), Ok("1"));
        assert_eq!(std::env::var("RIOTKVM_ZZ_ALIAS_TEST").as_deref(), Ok("nuevo"), "no pisa la nueva");
        assert_eq!(env("ZZ_NO_DEFINIDA_TEST"), None);
    }
}
