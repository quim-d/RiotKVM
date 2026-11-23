//! Captura de teclado a bajo nivel (Windows: `WH_KEYBOARD_LL`).
//!
//! Con la captura activa y la ventana en primer plano, TODAS las teclas (Win, Alt+Tab, Alt+Esc,
//! Ctrl+Esc, F-teclas...) se envían al servidor como teclas FÍSICAS y no llegan al sistema local.
//! No se pueden interceptar las secuencias que trata el propio Windows antes que los hooks:
//! Ctrl+Alt+Supr (SAS) y Win+L; para ellas está el menú "Enviar" (órdenes `ctrl+alt+del`...).
//!
//! La función del hook solo traduce y hace un `mpsc::send`; nada pesado en ese hilo.

pub const ESCAPE_HELP: &str = "Ctrl+Shift+F12";

use std::sync::atomic::{AtomicBool as AB, Ordering::Relaxed as R};
// teclas ya enviadas como pulsadas: filtra el autorepetido y evita el doble envío hook + eventos de ventana
static DOWN: [AB; 256] = [const { AB::new(false) }; 256];

/// `true` si hay que enviar este cambio (la tecla cambia de estado); `false` si ya estaba así.
pub fn claim(usage: u8, down: bool) -> bool {
    DOWN[usage as usize].swap(down, R) != down
}

pub fn clear_down() {
    for d in DOWN.iter() {
        d.store(false, R);
    }
}

#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
pub fn install(_tx: std::sync::mpsc::Sender<dkvm_core::session::Cmd>, _repaint: impl Fn() + Send + Sync + 'static) -> bool {
    false
}
#[cfg(not(windows))]
pub fn set_captured(_on: bool) {}
#[cfg(not(windows))]
pub fn take_chord_release() -> bool {
    false
}
#[cfg(not(windows))]
pub fn take_paste() -> bool {
    false
}
#[cfg(not(windows))]
pub fn take_cancel() -> bool {
    false
}
#[cfg(not(windows))]
pub fn set_blocked(_on: bool) {}
#[cfg(not(windows))]
pub fn set_paste_chord(_c: PasteChord) {}

/// Atajo local (no llega al servidor) para teclear el portapapeles en el servidor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PasteChord {
    Off,
    CtrlShiftV,
    CtrlAltV,
    CtrlShiftInsert,
}

impl PasteChord {
    pub fn parse(s: &str) -> Option<PasteChord> {
        Some(match s.to_lowercase().replace(' ', "").as_str() {
            "off" | "none" | "no" => PasteChord::Off,
            "ctrl+shift+v" => PasteChord::CtrlShiftV,
            "ctrl+alt+v" => PasteChord::CtrlAltV,
            "ctrl+shift+insert" | "ctrl+shift+ins" => PasteChord::CtrlShiftInsert,
            _ => return None,
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            PasteChord::Off => "(sin atajo)",
            PasteChord::CtrlShiftV => "Ctrl+Shift+V",
            PasteChord::CtrlAltV => "Ctrl+Alt+V",
            PasteChord::CtrlShiftInsert => "Ctrl+Shift+Insert",
        }
    }
    #[cfg_attr(not(windows), allow(dead_code))] // solo lo usa el hook de Windows
    fn code(self) -> u8 {
        self as u8
    }
    /// (vk, máscara de modificadores: 1 ctrl, 2 shift, 4 alt)
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn vk_mods(self) -> Option<(u32, u8)> {
        match self {
            PasteChord::Off => None,
            PasteChord::CtrlShiftV => Some((0x56, 3)),
            PasteChord::CtrlAltV => Some((0x56, 5)),
            PasteChord::CtrlShiftInsert => Some((0x2D, 3)),
        }
    }
    #[cfg(windows)]
    fn from_code(c: u8) -> PasteChord {
        match c {
            1 => PasteChord::CtrlShiftV,
            2 => PasteChord::CtrlAltV,
            3 => PasteChord::CtrlShiftInsert,
            _ => PasteChord::Off,
        }
    }
}

/// vk + scancode + flag extendido -> uso HID (Pausa se identifica por vk porque su scancode es especial).
#[cfg_attr(not(windows), allow(dead_code))] // hook de Windows (y sus pruebas)
pub fn translate(vk: u32, sc: u32, ext: bool) -> Option<u8> {
    if vk == 0x13 {
        return Some(0x48);
    }
    dkvm_core::scancode::hid_from_scancode(sc as u16, ext)
}

#[cfg(windows)]
mod win {
    use super::translate;
    use dkvm_core::session::Cmd;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};
    use std::sync::mpsc::Sender;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::PasteChord;
    use std::sync::atomic::AtomicU32;

    static CAPTURED: AtomicBool = AtomicBool::new(false);
    static CHORD: AtomicBool = AtomicBool::new(false);
    // vk cuyo "soltar" hay que tragarse tras un atajo local (F12 del escape, V/Insert del pegado)
    static EATEN_VK: AtomicU32 = AtomicU32::new(0);
    static PASTE: AtomicBool = AtomicBool::new(false);
    static CANCEL: AtomicBool = AtomicBool::new(false);
    // mientras teclea el portapapeles: se traga todo (no se mezcla con el pegado) y Esc cancela
    static BLOCKED: AtomicBool = AtomicBool::new(false);
    static PASTE_CHORD: AtomicU8 = AtomicU8::new(1);
    // solo pruebas (DKVM_HOOK_INJECTED=1): acepta teclas inyectadas con SendInput
    static TESTKEYS: AtomicBool = AtomicBool::new(false);
    static DEBUG: AtomicBool = AtomicBool::new(false);
    static ACCEPT_INJECTED: AtomicBool = AtomicBool::new(true);
    static MODS: AtomicU8 = AtomicU8::new(0); // bit0 ctrl, bit1 shift, bit2 alt
    static TX: OnceLock<Sender<Cmd>> = OnceLock::new();
    static REPAINT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

    /// Solo pruebas (DKVM_HOOK_TESTKEYS=1): F13..F24 se tratan como "de nuestra ventana" aunque no tenga el foco,
    /// para poder verificar el hook sin robar el foco ni tragarse teclas reales del usuario.
    fn test_key(vk: u32) -> bool {
        TESTKEYS.load(Relaxed) && (0x7B..=0x87).contains(&vk)
    }

    pub fn set_captured(on: bool) {
        super::clear_down();
        CAPTURED.store(on, Relaxed);
    }

    pub fn set_paste_chord(c: PasteChord) {
        PASTE_CHORD.store(c.code(), Relaxed);
    }

    /// Bloquea el reenvío (pegado en curso): el hook traga todo y solo Esc (cancelar) y el atajo de escape actúan.
    pub fn set_blocked(on: bool) {
        BLOCKED.store(on, Relaxed);
    }

    /// `true` una vez si se pulsó el atajo local de pegar el portapapeles.
    pub fn take_paste() -> bool {
        PASTE.swap(false, Relaxed)
    }

    /// `true` una vez si se pulsó Esc durante un pegado (cancelar).
    pub fn take_cancel() -> bool {
        CANCEL.swap(false, Relaxed)
    }

    /// `true` una vez si el hook liberó la captura por el atajo de escape.
    pub fn take_chord_release() -> bool {
        CHORD.swap(false, Relaxed)
    }

    fn ours() -> bool {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return false;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            pid == GetCurrentProcessId()
        }
    }

    unsafe extern "system" fn proc(code: i32, w: usize, l: isize) -> isize {
        if code == HC_ACTION as i32 {
            let k = &*(l as *const KBDLLHOOKSTRUCT);
            let up = k.flags & LLKHF_UP != 0;
            let ext = k.flags & LLKHF_EXTENDED != 0;
            let _ = w;
            crate::dlog::bump(&crate::dlog::HOOK_SEEN);
            if DEBUG.load(Relaxed) {
                eprintln!("hook vk={:#x} sc={:#x} fl={:#x} captured={} ours={}", k.vkCode, k.scanCode, k.flags, CAPTURED.load(Relaxed), ours());
            }
            // modificadores locales (para el atajo de escape)
            let bit = match k.vkCode {
                0xA2 | 0xA3 | 0x11 => 1,
                0xA0 | 0xA1 | 0x10 => 2,
                0xA4 | 0xA5 | 0x12 => 4,
                _ => 0,
            };
            if bit != 0 {
                if up {
                    MODS.fetch_and(!bit, Relaxed);
                } else {
                    MODS.fetch_or(bit, Relaxed);
                }
            }
            if crate::dlog::enabled() && CAPTURED.load(Relaxed) {
                let (inj, o) = (k.flags & LLKHF_INJECTED != 0, ours());
                crate::dlog!("hook vk={:#x} sc={:#x} {} ext={} injected={} ours={}", k.vkCode, k.scanCode, if up { "up" } else { "down" }, ext, inj, o);
            }
            if CAPTURED.load(Relaxed) && (k.flags & LLKHF_INJECTED == 0 || ACCEPT_INJECTED.load(Relaxed)) && (ours() || test_key(k.vkCode)) {
                if !up {
                    // atajo local de pegar el portapapeles (no llega al servidor)
                    if let Some((vk, m)) = PasteChord::from_code(PASTE_CHORD.load(Relaxed)).vk_mods() {
                        if k.vkCode == vk && MODS.load(Relaxed) & 7 == m {
                            PASTE.store(true, Relaxed);
                            EATEN_VK.store(vk, Relaxed);
                            if let Some(tx) = TX.get() {
                                let _ = tx.send(Cmd::ReleaseAll); // los modificadores ya pulsados no deben quedar en el servidor
                            }
                            if let Some(r) = REPAINT.get() {
                                r();
                            }
                            return 1;
                        }
                    }
                }
                if BLOCKED.load(Relaxed) && !(k.vkCode == 0x7B && !up && MODS.load(Relaxed) & 3 == 3) {
                    if !up && k.vkCode == 0x1B {
                        CANCEL.store(true, Relaxed);
                        if let Some(r) = REPAINT.get() {
                            r();
                        }
                    }
                    return 1; // pegado en curso: nada se reenvía
                }
                if k.vkCode == 0x7B {
                    // F12
                    if !up && MODS.load(Relaxed) & 3 == 3 {
                        CAPTURED.store(false, Relaxed);
                        CHORD.store(true, Relaxed);
                        EATEN_VK.store(0x7B, Relaxed);
                        if let Some(tx) = TX.get() {
                            let _ = tx.send(Cmd::ReleaseAll);
                        }
                        if let Some(r) = REPAINT.get() {
                            r();
                        }
                        return 1;
                    }
                }
                if let Some(u) = translate(k.vkCode, k.scanCode, ext) {
                    if super::claim(u, !up) {
                        if let Some(tx) = TX.get() {
                            dkvm_core::latency::mark_event();
                            let _ = tx.send(Cmd::Key { usage: u, down: !up });
                            crate::dlog::bump(&crate::dlog::KEYS);
                            crate::dlog!("hook -> Key usage={:#04x} down={}", u, !up);
                        }
                    }
                } else {
                    crate::dlog!("hook: vk={:#x} sc={:#x} sin uso HID (tecla tragada, no enviada)", k.vkCode, k.scanCode);
                }
                return 1; // se traga TODA la tecla mientras dura la captura
            }
            if up && k.vkCode != 0 && EATEN_VK.load(Relaxed) == k.vkCode {
                EATEN_VK.store(0, Relaxed);
                return 1; // el soltar de la tecla del atajo (F12, V, Insert)
            }
        }
        CallNextHookEx(std::ptr::null_mut(), code, w, l)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::mpsc::channel;

        fn call(vk: u32, sc: u32, flags: u32) -> isize {
            let mut k: KBDLLHOOKSTRUCT = unsafe { std::mem::zeroed() };
            k.vkCode = vk;
            k.scanCode = sc;
            k.flags = flags;
            unsafe { proc(HC_ACTION as i32, WM_KEYDOWN as usize, &k as *const _ as isize) }
        }

        /// Recorre `proc` con eventos sintéticos (sin hook real, sin tocar el teclado del usuario):
        /// se traga la tecla, la manda como física, filtra el autorepetido y el atajo de escape libera.
        #[test]
        fn proc_swallows_forwards_and_releases_on_chord() {
            let (tx, rx) = channel();
            TX.set(tx).unwrap();
            TESTKEYS.store(true, Relaxed);
            set_captured(true);
            // F15 (vk 0x7E, sc 0x64: sin tabla -> se traga pero no se envía); usamos Win izq (0x5B) y 'a' vía vk F13..F24 no aplica:
            // el filtro de pruebas solo admite F12..F24, así que se prueba con F13 (sc 0x64) y con el atajo.
            assert_eq!(call(0x7C, 0x64, 0), 1, "se traga la tecla");
            assert!(rx.try_recv().is_err(), "scancode sin tabla: no se envía");
            // atajo Ctrl+Shift+F12
            call(0xA2, 0x1D, 0);
            call(0xA0, 0x2A, 0);
            assert_eq!(call(0x7B, 0x58, 0), 1);
            assert!(matches!(rx.try_recv(), Ok(Cmd::ReleaseAll)));
            assert!(take_chord_release());
            assert!(!CAPTURED.load(Relaxed), "la captura se libera");
            // el soltar de F12 también se traga; las teclas posteriores pasan al sistema
            assert_eq!(call(0x7B, 0x58, LLKHF_UP), 1);
            call(0xA0, 0x2A, LLKHF_UP);
            call(0xA2, 0x1D, LLKHF_UP);
            assert_ne!(call(0x7C, 0x64, 0), 1);
            // captura de nuevo: F12 sola (sin modificadores) sí se reenvía como tecla física
            set_captured(true);
            assert_eq!(call(0x7B, 0x58, 0), 1);
            assert!(matches!(rx.try_recv(), Ok(Cmd::Key { usage: 0x45, down: true })));
            assert_eq!(call(0x7B, 0x58, 0), 1);
            assert!(rx.try_recv().is_err(), "autorepetido filtrado");
            assert_eq!(call(0x7B, 0x58, LLKHF_UP), 1);
            assert!(matches!(rx.try_recv(), Ok(Cmd::Key { usage: 0x45, down: false })));
        }
    }

    /// Instala el hook en un hilo propio con bucle de mensajes. `false` si Windows lo rechaza.
    pub fn install(tx: Sender<Cmd>, repaint: impl Fn() + Send + Sync + 'static) -> bool {
        if TX.set(tx).is_err() {
            return true; // ya instalado
        }
        let _ = REPAINT.set(Box::new(repaint));
        TESTKEYS.store(std::env::var_os("DKVM_HOOK_TESTKEYS").is_some(), Relaxed);
        DEBUG.store(std::env::var_os("DKVM_HOOK_DEBUG").is_some(), Relaxed);
        // teclas "inyectadas" (PowerToys, AutoHotkey, escritorio remoto, teclados con software) también cuentan; DKVM_HOOK_NOINJECTED=1 las filtra
        ACCEPT_INJECTED.store(std::env::var_os("DKVM_HOOK_NOINJECTED").is_none(), Relaxed);
        let (rtx, rrx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("dkvm-hook".into())
            .spawn(move || unsafe {
                dkvm_core::rt::boost_current_thread(); // el hook tiene ~300 ms de margen o Windows lo da de baja en silencio
                let h = SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc), GetModuleHandleW(std::ptr::null()), 0);
                if h.is_null() {
                    eprintln!("dkvm-gui: SetWindowsHookExW falló (GetLastError={})", windows_sys::Win32::Foundation::GetLastError());
                }
                let _ = rtx.send(!h.is_null());
                if h.is_null() {
                    return;
                }
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            })
            .ok();
        rrx.recv().unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::translate;
    #[test]
    fn pause_by_vk_and_win_keys() {
        assert_eq!(translate(0x13, 0x45, false), Some(0x48));
        assert_eq!(translate(0x5B, 0x5B, true), Some(0xE3));
        assert_eq!(translate(0x09, 0x0F, false), Some(0x2B)); // Tab (Alt+Tab)
        assert_eq!(translate(0xA5, 0x38, true), Some(0xE6)); // AltGr
    }
}
