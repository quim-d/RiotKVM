//! Tiempo real "blando" para la ruta de entrada: resolución de temporizador de 1 ms y prioridad alta del hilo.
//! Windows: `timeBeginPeriod(1)` (por defecto el sleep/espera tiene granularidad de 15,6 ms) y
//! `SetThreadPriority(HIGHEST)`. Otros sistemas: no hace nada (Linux/macOS ya tienen temporizadores finos).

/// Pide resolución de 1 ms al planificador mientras viva el proceso. Idempotente.
pub fn timer_resolution_1ms() {
    #[cfg(windows)]
    {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| unsafe {
            windows::Win32::Media::timeBeginPeriod(1);
        });
    }
}

/// Sube la prioridad del hilo actual (los hilos de envío de entrada y BLE). Devuelve si se pudo.
pub fn boost_current_thread() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
        timer_resolution_1ms();
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST).is_ok()
    }
    #[cfg(not(windows))]
    {
        false
    }
}
