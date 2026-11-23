//! Registro de diagnóstico opcional (`--log FICHERO` o `DKVM_GUI_LOG`): una línea por suceso con ms desde el arranque.
//! Sin fichero configurado no hace nada (un `OnceLock` leído).

use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static LOG: OnceLock<(Mutex<std::fs::File>, Instant)> = OnceLock::new();

/// Contadores visibles en la barra de estado (comandos que el cliente ENCOLA hacia el dongle).
pub static KEYS: AtomicU32 = AtomicU32::new(0);
pub static BUTTONS: AtomicU32 = AtomicU32::new(0);
pub static MOVES: AtomicU32 = AtomicU32::new(0);
/// Eventos de teclado que ve el hook (con o sin captura): diagnóstico de si el hook recibe algo.
pub static HOOK_SEEN: AtomicU32 = AtomicU32::new(0);
pub static ERRORS: AtomicU32 = AtomicU32::new(0);

pub fn init(path: &std::path::Path) {
    if let Ok(f) = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(path) {
        let _ = LOG.set((Mutex::new(f), Instant::now()));
        log(format_args!("log iniciado"));
    } else {
        eprintln!("riotkvm-gui: no se pudo abrir el log {}", path.display());
    }
}

pub fn enabled() -> bool {
    LOG.get().is_some()
}

pub fn log(args: std::fmt::Arguments) {
    if let Some((f, t0)) = LOG.get() {
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{:>9.1} {}", t0.elapsed().as_secs_f64() * 1e3, args);
        }
    }
}

#[macro_export]
macro_rules! dlog {
    ($($a:tt)*) => { $crate::dlog::log(format_args!($($a)*)) };
}

pub fn bump(c: &AtomicU32) {
    c.fetch_add(1, Relaxed);
}
