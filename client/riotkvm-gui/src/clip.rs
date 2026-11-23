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
            return Err(crate::i18n::t("cb.open").into());
        }
        let r = (|| {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
                return Err(if IsClipboardFormatAvailable(CF_HDROP) != 0 {
                    crate::i18n::t("cb.files").to_string()
                } else if IsClipboardFormatAvailable(CF_BITMAP) != 0 || IsClipboardFormatAvailable(CF_DIB) != 0 {
                    crate::i18n::t("cb.image").to_string()
                } else {
                    crate::i18n::t("cb.no_text").to_string()
                });
            }
            let h = GetClipboardData(CF_UNICODETEXT);
            if h.is_null() {
                return Err(crate::i18n::t("cb.read").to_string());
            }
            let p = GlobalLock(h) as *const u16;
            if p.is_null() {
                return Err(crate::i18n::t("cb.lock").to_string());
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
    Err(crate::i18n::t("cb.windows_only").into())
}
