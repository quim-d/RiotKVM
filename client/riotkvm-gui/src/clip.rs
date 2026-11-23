//! Lectura del portapapeles del sistema local (solo texto). El contenido nunca se registra.

#[cfg(windows)]
pub fn read_text() -> Result<String, String> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard};
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    const CF_BITMAP: u32 = 2;
    const CF_DIB: u32 = 8;
    const CF_HDROP: u32 = 15;
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !opened {
            return Err("no se pudo abrir el portapapeles (¿lo está usando otro programa?)".into());
        }
        let r = (|| {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
                return Err(if IsClipboardFormatAvailable(CF_HDROP) != 0 {
                    "el portapapeles contiene archivos: solo se teclea texto".to_string()
                } else if IsClipboardFormatAvailable(CF_BITMAP) != 0 || IsClipboardFormatAvailable(CF_DIB) != 0 {
                    "el portapapeles contiene una imagen: solo se teclea texto".to_string()
                } else {
                    "el portapapeles no contiene texto".to_string()
                });
            }
            let h = GetClipboardData(CF_UNICODETEXT);
            if h.is_null() {
                return Err("no se pudo leer el portapapeles".to_string());
            }
            let p = GlobalLock(h) as *const u16;
            if p.is_null() {
                return Err("no se pudo bloquear el portapapeles".to_string());
            }
            let mut len = 0usize;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            GlobalUnlock(h);
            Ok(s)
        })();
        CloseClipboard();
        r
    }
}

#[cfg(not(windows))]
pub fn read_text() -> Result<String, String> {
    Err("la lectura directa del portapapeles solo existe en Windows: usa Ctrl+V con la ventana sin capturar".into())
}
