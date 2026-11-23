//! Ventana principal: vídeo + captura de teclado/ratón + barra de estado.
//!
//! Hilos: UI (este) · `riotkvm-session` (dueño del `Kvm`) · `riotkvm-io` (puerto) · `riotkvm-video` · `riotkvm-hook`.
//! El hilo de UI solo traduce eventos a `Cmd` y hace `send`; nunca espera al dongle.

use crate::dlog::{BUTTONS, ERRORS, KEYS, MOVES};
use crate::{dlog, hook, theme};
use riotkvm_core::input::abs_from_pixels;
use std::sync::atomic::Ordering::Relaxed;
use riotkvm_core::session::{Cmd, Event, PasteJob, Session};
use riotkvm_core::transport::LinkState;
use riotkvm_video::{Frame, Latest, Mode, Source};
use eframe::egui::{self, Color32, Key, PointerButton, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseMode {
    Absolute,
    Relative,
}

/// Fuente de vídeo en marcha; se reinicia al cambiar de modo (1080p30 <-> 720p60...).
pub struct VideoCtl {
    pub source: Source,
    pub mode: Mode,
    pub desc: String,
    pub latest: Arc<Latest>,
    ctx: egui::Context,
}

impl VideoCtl {
    pub fn start(source: Source, desc: String, mode: Mode, ctx: egui::Context) -> VideoCtl {
        let latest = Latest::new();
        let c = ctx.clone();
        latest.set_notify(move || c.request_repaint());
        let _ = riotkvm_video::start(&source, &mode, latest.clone());
        VideoCtl { source, mode, desc, latest, ctx }
    }
    pub fn ctx_clone(&self) -> egui::Context {
        self.ctx.clone()
    }

    /// Cambia de fuente en caliente: para el stream anterior (libera el dispositivo) y abre el nuevo; si falla, reintenta cada 2 s.
    pub fn switch(&mut self, source: Source, desc: String, mode: Mode) {
        self.latest.stop();
        *self = VideoCtl::start(source, desc, mode, self.ctx.clone());
    }
    pub fn restart(&mut self, mode: Mode) {
        self.latest.stop(); // el hilo antiguo termina solo (no se espera)
        *self = VideoCtl::start(self.source.clone(), self.desc.clone(), mode, self.ctx.clone());
    }
}

/// Modos ofrecidos en el desplegable (MJPEG: el único formato que da 30-60 fps en esta capturadora).
pub const VIDEO_MODES: &[(&str, u32, u32, u32)] = &[("1080p30 · nitidez", 1920, 1080, 30), ("720p60 · fluidez", 1280, 720, 60), ("720p30 · ligero", 1280, 720, 30)];

fn mode_path() -> Option<std::path::PathBuf> {
    Some(riotkvm_core::config::state_dir()?.join("video_mode.txt"))
}

/// Última resolución elegida en el desplegable ("WxH@fps").
pub fn load_video_mode() -> Option<(u32, u32, u32)> {
    let s = std::fs::read_to_string(mode_path()?).ok()?;
    let (r, f) = s.trim().split_once('@')?;
    let (w, h) = r.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?, f.parse().ok()?))
}

fn save_video_mode(w: u32, h: u32, fps: u32) {
    if let Some(p) = mode_path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, format!("{w}x{h}@{fps}"));
    }
}

/// Textos de más de esto piden confirmación antes de teclearse (configurable: "confirmar siempre").
const CONFIRM_OVER: usize = 500;

/// Ajustes del pegado, persistidos en `%APPDATA%\riotkvm\paste.txt`.
#[derive(Clone, Copy, PartialEq)]
struct PasteCfg {
    /// "teclas físicas": posición US, sin depender del layout del servidor (BIOS).
    physical: bool,
    per_key_ms: u32,
    confirm_always: bool,
}

impl PasteCfg {
    fn load() -> PasteCfg {
        let mut c = PasteCfg { physical: false, per_key_ms: 5, confirm_always: false };
        if let Some(t) = riotkvm_core::config::load("paste.txt") {
            for l in t.lines() {
                match l.split_once('=') {
                    Some(("physical", v)) => c.physical = v.trim() == "1",
                    Some(("ms", v)) => c.per_key_ms = v.trim().parse().unwrap_or(5).min(200),
                    Some(("confirm", v)) => c.confirm_always = v.trim() == "1",
                    _ => {}
                }
            }
        }
        c
    }
    fn save(&self) {
        riotkvm_core::config::save("paste.txt", &format!("physical={}\nms={}\nconfirm={}\n", self.physical as u8, self.per_key_ms, self.confirm_always as u8));
    }
}

struct Pending {
    text: String,
    bad: Vec<char>,
    layout: String,
}

enum Action {
    Keys(String),
    Repeat,
    PasteClip,
    OpenPasteWin,
    PasteWinText,
    ConfirmPaste,
    CancelPending,
    StopJob,
}

pub struct Options {
    /// Modos MJPEG (ancho, alto, fps) de la fuente inicial (vacío = desconocidos: se ofrece la lista fija).
    pub modes: Vec<(u32, u32, u32)>,
    /// Preferencia de fuente en curso ("auto", "none" o nombre del dispositivo).
    pub src_pref: String,
    /// Aviso al arrancar (p. ej. la capturadora guardada ya no está).
    pub video_note: Option<String>,
    pub layout: String,
    pub paste_chord: hook::PasteChord,
    pub mouse: MouseMode,
    pub hook: bool,
    pub transport_desc: String,
    /// Captura de pantalla automática (verificación): (retraso, ruta).
    pub shot: Option<(Duration, std::path::PathBuf)>,
    pub exit_after: Option<Duration>,
    pub start_captured: bool,
}

pub struct App {
    session: Session,
    video: VideoCtl,
    opts: Options,
    tex: Option<TextureHandle>,
    vsize: (u32, u32),
    mode: MouseMode,
    captured: bool,
    hook_ok: bool,
    link: LinkState,
    /// (host USB, hid listo, LEDs, SD, modo A/B/C, boot protocol)
    status: Option<(bool, bool, u8, bool, Option<String>, bool)>,
    rtt_us: Option<u64>,
    dev_err: Option<(String, Instant)>,
    // vídeo
    frames: u32,
    fps_t: Instant,
    fps: f32,
    age_ms: f32,
    age_end_ms: f32,
    last_arrived: Option<Instant>,
    last_stat_log: Instant,
    dec_ms: f32,
    // entrada
    mods_prev: egui::Modifiers,
    held: u8,
    wheel_acc: f32,
    rel_acc: Vec2,
    started: Instant,
    shot_done: bool,
    skull: Option<TextureHandle>,
    mode_icons: Vec<(&'static str, TextureHandle)>,
    // fuente de vídeo
    devices: Vec<riotkvm_video::DeviceInfo>,
    dev_rx: Option<std::sync::mpsc::Receiver<Vec<riotkvm_video::DeviceInfo>>>,
    modes: Vec<(u32, u32, u32)>,
    /// Cambio de fuente a medias: espera a conocer los modos del dispositivo nuevo antes de abrirlo.
    switching: Option<(Source, String, std::sync::mpsc::Receiver<Vec<(u32, u32, u32)>>)>,
    src_pref: String,
    last_dev_refresh: Instant,
    // distribución del teclado del servidor, pegado y envío de teclas
    layout: String,
    paste: PasteCfg,
    paste_win: bool,
    paste_text: String,
    pending: Option<Pending>,
    job: Option<(String, usize, usize)>,
    note: Option<(String, Instant, bool)>,
    custom_keys: String,
    rep_combo: String,
    rep_interval: u32,
    rep_secs: f32,
    settings: crate::settings::Settings,
    tools: crate::tools::Tools,
}

/// Combinaciones que el SO local intercepta o son incómodas de teclear.
const KEYS_SYSTEM: &[(&str, &str)] = &[
    ("Ctrl+Alt+Supr / Del", "ctrl+alt+del"),
    ("Ctrl+Shift+Esc", "ctrl+shift+esc"),
    ("Ctrl+Esc", "ctrl+esc"),
    ("Win", "win"),
    ("Win+R", "win+r"),
    ("Win+L", "win+l"),
    ("Win+D", "win+d"),
    ("Alt+Tab", "alt+tab"),
    ("Alt+Esc", "alt+esc"),
    ("Alt+F4", "alt+f4"),
    ("Impr Pant", "printscreen"),
    ("Alt+SysRq (luego la tecla de orden)", "alt+sysrq"),
];
const KEYS_TTY: &[(&str, &str)] = &[
    ("Ctrl+Alt+F1", "ctrl+alt+f1"),
    ("Ctrl+Alt+F2", "ctrl+alt+f2"),
    ("Ctrl+Alt+F3", "ctrl+alt+f3"),
    ("Ctrl+Alt+F4", "ctrl+alt+f4"),
    ("Ctrl+Alt+F5", "ctrl+alt+f5"),
    ("Ctrl+Alt+F6", "ctrl+alt+f6"),
    ("Ctrl+Alt+F7", "ctrl+alt+f7"),
    ("Ctrl+Alt+Retroceso", "ctrl+alt+backspace"),
    ("Ctrl+Alt+Supr / Del", "ctrl+alt+del"),
];
const KEYS_BIOS: &[(&str, &str)] = &[
    ("F1", "f1"),
    ("F2", "f2"),
    ("F3", "f3"),
    ("F4", "f4"),
    ("F5", "f5"),
    ("F6", "f6"),
    ("F7", "f7"),
    ("F8", "f8"),
    ("F9", "f9"),
    ("F10", "f10"),
    ("F11", "f11"),
    ("F12", "f12"),
    ("Esc", "esc"),
    ("Supr", "del"),
    ("Insert", "insert"),
    ("Enter", "enter"),
    ("Tab", "tab"),
    ("RePág", "pageup"),
    ("AvPág", "pagedown"),
];

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, session: Session, video: VideoCtl, opts: Options) -> App {
        let ctx = cc.egui_ctx.clone();
        let hook_ok = opts.hook && hook::install(session.tx.clone(), {
            let c = ctx.clone();
            move || c.request_repaint()
        });
        let mode = opts.mouse;
        let start_captured = opts.start_captured;
        let mut app = App {
            session,
            video,
            opts,
            tex: None,
            vsize: (0, 0),
            mode,
            captured: false,
            hook_ok,
            link: LinkState::Searching,
            status: None,
            rtt_us: None,
            dev_err: None,
            frames: 0,
            fps_t: Instant::now(),
            fps: 0.0,
            age_ms: 0.0,
            age_end_ms: 0.0,
            last_arrived: None,
            last_stat_log: Instant::now(),
            dec_ms: 0.0,
            mods_prev: egui::Modifiers::NONE,
            held: 0,
            wheel_acc: 0.0,
            rel_acc: Vec2::ZERO,
            started: Instant::now(),
            shot_done: false,
            skull: load_png(&ctx, "skull", include_bytes!("../assets/skull_64.png")),
            mode_icons: vec![
                ("CARGO", load_png(&ctx, "mA", include_bytes!("../assets/mode_A_cargo_40.png"))),
                ("BOARDING", load_png(&ctx, "mB", include_bytes!("../assets/mode_B_boarding_40.png"))),
                ("FULL", load_png(&ctx, "mC", include_bytes!("../assets/mode_C_full_40.png"))),
            ]
            .into_iter()
            .filter_map(|(n, t)| t.map(|t| (n, t)))
            .collect(),
            devices: Vec::new(),
            dev_rx: None,
            modes: Vec::new(),
            switching: None,
            src_pref: String::new(),
            last_dev_refresh: Instant::now() - Duration::from_secs(60),
            layout: String::new(),
            paste: PasteCfg::load(),
            paste_win: false,
            paste_text: String::new(),
            pending: None,
            job: None,
            note: None,
            custom_keys: String::new(),
            rep_combo: "f2".into(),
            rep_interval: 250,
            rep_secs: 10.0,
            settings: crate::settings::Settings::new(),
            tools: Default::default(),
        };
        app.layout = riotkvm_core::layouts::canonical(&app.opts.layout).unwrap_or("es").to_string();
        app.modes = app.opts.modes.clone();
        app.src_pref = app.opts.src_pref.clone();
        if let Some(n) = app.opts.video_note.clone() {
            app.note(n, true);
        }
        app.refresh_devices();
        hook::set_paste_chord(app.opts.paste_chord);
        if start_captured {
            app.set_capture(&ctx, true);
        }
        app
    }

    /// Enumera cámaras en segundo plano (solo consulta: no abre ni enciende ninguna).
    fn refresh_devices(&mut self) {
        if self.dev_rx.is_some() {
            return;
        }
        self.last_dev_refresh = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        self.dev_rx = Some(rx);
        let c = self.video.ctx_clone();
        std::thread::spawn(move || {
            let _ = tx.send(riotkvm_video::list_devices());
            c.request_repaint();
        });
    }

    /// Elige una fuente ("auto", "none" o nombre): para el stream anterior, averigua los modos del nuevo (sin
    /// encenderlo) y lo abre con el modo más parecido al actual.
    fn select_source(&mut self, pref: &str) {
        let (src, warn) = riotkvm_video::source_from_pref(pref, None, &self.devices);
        let (resolved, desc) = riotkvm_video::resolve(&src);
        let (name, idx) = match (&src, &resolved) {
            (Source::None, _) => ("none".to_string(), None),
            (Source::Auto, _) => ("auto".to_string(), None),
            (Source::Native { name, index }, _) => (name.clone().unwrap_or_default(), *index),
            _ => ("auto".to_string(), None),
        };
        riotkvm_core::config::save("video_source.txt", &format!("{name}\n{}", idx.map(|i| i.to_string()).unwrap_or_default()));
        dlog!("fuente de vídeo -> {name}");
        self.src_pref = name;
        self.video.latest.stop(); // libera el dispositivo anterior ya
        self.tex = None;
        self.vsize = (0, 0);
        self.modes.clear();
        let (tx, rx) = std::sync::mpsc::channel();
        let probe = match &resolved {
            Source::Native { index: Some(i), .. } => Some(*i),
            _ => None,
        };
        let c = self.video.ctx_clone();
        std::thread::spawn(move || {
            let m = probe.map(riotkvm_video::native::modes).unwrap_or_default();
            let _ = tx.send(m);
            c.request_repaint();
        });
        self.switching = Some((resolved, desc, rx));
        if let Some(w) = warn {
            self.note(w, true);
        }
    }

    /// Termina un cambio de fuente cuando llegan los modos del dispositivo nuevo.
    fn poll_video_source(&mut self) {
        if let Some(rx) = &self.dev_rx {
            if let Ok(d) = rx.try_recv() {
                self.devices = d;
                self.dev_rx = None;
            }
        }
        // sin vídeo (error o dispositivo ausente): se vuelve a enumerar cada 4 s por si aparece
        if self.dev_rx.is_none() && self.switching.is_none() && self.last_dev_refresh.elapsed() > Duration::from_secs(4) && (self.video.latest.error().is_some() || self.devices.is_empty()) {
            self.refresh_devices();
        }
        let done = match &self.switching {
            Some((_, _, rx)) => rx.try_recv().ok(),
            None => None,
        };
        if let Some(modes) = done {
            if let Some((src, desc, _)) = self.switching.take() {
                let cur = (self.video.mode.w, self.video.mode.h, self.video.mode.fps);
                let chosen = riotkvm_video::pick_mode(cur, &modes).unwrap_or(cur);
                if chosen != cur {
                    self.note(format!("el modo {}x{}@{} no existe en esta fuente: se usa {}x{}@{}", cur.0, cur.1, cur.2, chosen.0, chosen.1, chosen.2), false);
                    save_video_mode(chosen.0, chosen.1, chosen.2);
                }
                self.modes = modes;
                let mode = Mode { w: chosen.0, h: chosen.1, fps: chosen.2, format: "mjpeg".into() };
                self.video.switch(src, desc, mode);
            }
        }
    }

    fn note(&mut self, msg: impl Into<String>, err: bool) {
        self.note = Some((msg.into(), Instant::now(), err));
    }

    fn paste_layout(&self) -> String {
        if self.paste.physical {
            "us".into()
        } else {
            self.layout.clone()
        }
    }

    fn paste_clipboard(&mut self) {
        match crate::clip::read_text() {
            Ok(t) => self.start_paste(t),
            Err(m) => self.note(m, true),
        }
    }

    fn start_paste(&mut self, text: String) {
        if self.job.is_some() {
            self.note("ya hay un trabajo en curso (Esc lo cancela)", true);
            return;
        }
        let n = text.chars().count();
        if n == 0 {
            self.note("no hay texto que teclear", true);
            return;
        }
        let layout = self.paste_layout();
        let bad = riotkvm_core::layouts::get_layout(&layout).map(|l| l.text_strokes_lossy(&text).1).unwrap_or_default();
        if self.paste.confirm_always || n > CONFIRM_OVER {
            self.pending = Some(Pending { text, bad, layout });
            return;
        }
        self.run_paste(text, layout, bad);
    }

    fn run_paste(&mut self, text: String, layout: String, bad: Vec<char>) {
        let n = text.chars().count();
        // el contenido nunca se registra: solo longitud y parámetros
        dlog!("pegar: {n} caracteres, layout {layout}, {} ms/tecla, {} no representables", self.paste.per_key_ms, bad.len());
        self.job = Some(("pegar".into(), 0, n));
        hook::set_blocked(true);
        if !bad.is_empty() {
            let mut seen: Vec<char> = Vec::new();
            for c in &bad {
                if !seen.contains(c) && seen.len() < 8 {
                    seen.push(*c);
                }
            }
            let list: String = seen.iter().map(|c| if c.is_control() { format!("U+{:04X}", *c as u32) } else { c.to_string() }).collect::<Vec<_>>().join(" ");
            self.note(format!("AVISO: {} caracteres no representables en el layout {layout} se saltarán: {list}", bad.len()), true);
        } else {
            self.note(format!("tecleando {n} caracteres (Esc cancela)..."), false);
        }
        self.send(Cmd::Paste(PasteJob { text, layout, per_key_ms: self.paste.per_key_ms }));
    }

    fn send_keys(&mut self, combo: &str) {
        let combo = combo.trim();
        if combo.is_empty() {
            return;
        }
        if let Ok(lay) = riotkvm_core::layouts::get_layout(&self.layout) {
            for tok in combo.split_whitespace() {
                if let Err(e) = riotkvm_core::keys::parse_combo(tok, lay) {
                    self.note(format!("combinación no válida: {}", e.message()), true);
                    return;
                }
            }
        }
        dlog!("enviar teclas: {combo}");
        self.note(format!("enviando: {combo}"), false);
        self.send(Cmd::Keys(combo.to_string()));
    }

    fn cancel_job(&mut self) {
        self.session.cancel_job();
        dlog!("trabajo cancelado por el usuario");
    }

    /// ¿Este evento es el atajo de pegar el portapapeles?
    fn paste_chord_hit(&self, ev: &egui::Event) -> bool {
        let egui::Event::Key { key, pressed: true, modifiers: m, repeat: false, .. } = ev else { return false };
        match self.opts.paste_chord {
            hook::PasteChord::Off => false,
            hook::PasteChord::CtrlShiftV => *key == Key::V && m.ctrl && m.shift && !m.alt,
            hook::PasteChord::CtrlAltV => *key == Key::V && m.ctrl && m.alt && !m.shift,
            hook::PasteChord::CtrlShiftInsert => *key == Key::Insert && m.ctrl && m.shift,
        }
    }

    fn apply_action(&mut self, a: Action) {
        match a {
            Action::Keys(c) => self.send_keys(&c),
            Action::Repeat => {
                let combo = self.rep_combo.trim().to_string();
                if combo.is_empty() || self.job.is_some() {
                    return;
                }
                dlog!("repetir {combo} cada {} ms durante {} s", self.rep_interval, self.rep_secs);
                self.job = Some((format!("repetir {combo}"), 0, 0));
                self.note(format!("repitiendo {combo} cada {} ms durante {:.0} s (Esc / Parar lo detiene)", self.rep_interval, self.rep_secs), false);
                hook::set_blocked(true);
                self.send(Cmd::Repeat { combo, interval_ms: self.rep_interval, secs: self.rep_secs });
            }
            Action::PasteClip => self.paste_clipboard(),
            Action::OpenPasteWin => self.paste_win = true,
            Action::PasteWinText => {
                let t = self.paste_text.clone();
                self.start_paste(t);
            }
            Action::ConfirmPaste => {
                if let Some(p) = self.pending.take() {
                    self.run_paste(p.text, p.layout, p.bad);
                }
            }
            Action::CancelPending => self.pending = None,
            Action::StopJob => self.cancel_job(),
        }
    }

    /// Distribución del teclado del servidor (barra "Teclado" y Ajustes): se guarda y se aplica en caliente.
    fn set_layout(&mut self, n: &'static str) {
        self.layout = n.to_string();
        riotkvm_core::config::save_layout(n);
        self.send(Cmd::SetLayout(n.to_string()));
        self.note(format!("distribución del teclado del servidor: {}", riotkvm_core::layouts::layout_label(n)), false);
    }

    fn send(&self, c: Cmd) {
        riotkvm_core::latency::mark_event();
        self.session.send(c);
    }

    fn set_capture(&mut self, ctx: &egui::Context, on: bool) {
        self.set_capture_why(ctx, on, "ui")
    }

    fn set_capture_why(&mut self, ctx: &egui::Context, on: bool, why: &str) {
        if on == self.captured {
            return;
        }
        dlog!("captura {} ({why}) ratón={:?} hook={}", if on { "ON" } else { "OFF" }, self.mode, self.hook_ok);
        self.captured = on;
        hook::set_captured(on && self.hook_ok);
        hook::clear_down();
        if !on {
            self.send(Cmd::ReleaseAll);
            self.held = 0;
            self.mods_prev = egui::Modifiers::NONE;
        }
        let rel = self.mode == MouseMode::Relative;
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!(on && rel)));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(grab_mode(on && rel)));
    }

    fn drain_events(&mut self) {
        while let Ok(e) = self.session.events.try_recv() {
            match e {
                Event::Link(l) => {
                    dlog!("enlace: {l:?}");
                    self.tools.on_link(l.is_connected());
                    if !l.is_connected() {
                        self.status = None;
                        self.rtt_us = None;
                    }
                    self.link = l;
                }
                Event::Status { host, hidready, leds, sd, mode, boot } => {
                    let new = Some((host, hidready, leds, sd, mode, boot));
                    if new != self.status {
                        dlog!("estado: {new:?}");
                    }
                    self.status = new;
                }
                Event::Rtt(us) => self.rtt_us = Some(us),
                Event::Job { label, done, total } => self.job = Some((label, done, total)),
                Event::JobDone { label, summary, error, cancelled } => {
                    self.job = None;
                    hook::set_blocked(false);
                    dlog!("trabajo '{label}' terminado: cancelado={cancelled} error={}", error.is_some());
                    let (msg, err) = match error {
                        Some(e) => (format!("{label}: ERROR {e}. {summary}"), true),
                        None => (format!("{label}: {summary}"), cancelled || summary.contains("NO representables")),
                    };
                    self.note(msg, err);
                }
                Event::Led(l) => self.settings.on_led(l),
                Event::Name(n) => self.tools.on_name(n),
                Event::DeviceError(m) => {
                    dlog::bump(&ERRORS);
                    dlog!("ERROR del dongle: {m}");
                    self.dev_err = Some((m, Instant::now()));
                }
            }
        }
    }

    fn upload_video(&mut self, ctx: &egui::Context) {
        let Some(f) = self.video.latest.take() else { return };
        let Frame { w, h, rgba, arrived, decode, .. } = f;
        // RGBA -> Color32 SIN copia (mismo búfer: alineación 1 y longitud múltiplo de 4); con alfa 255 premultiplicado =
        // sin premultiplicar. Solo si el búfer no encaja (no debería) se copia.
        let pixels: Vec<Color32> = bytemuck::allocation::try_cast_vec(rgba).unwrap_or_else(|(_, v)| bytemuck::pod_collect_to_vec(&v));
        let img = egui::ColorImage::new([w as usize, h as usize], pixels);
        match &mut self.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("video", img, TextureOptions::LINEAR)),
        }
        self.vsize = (w, h);
        self.age_ms = arrived.elapsed().as_secs_f32() * 1e3;
        self.last_arrived = Some(arrived);
        self.dec_ms = decode.as_secs_f32() * 1e3;
        self.frames += 1;
        let dt = self.fps_t.elapsed().as_secs_f32();
        if dt >= 1.0 {
            self.fps = self.frames as f32 / dt;
            self.frames = 0;
            self.fps_t = Instant::now();
        }
    }

    fn flush_mouse(&self, abs: &mut Option<(i64, i64)>, rel: &mut (i64, i64)) {
        if let Some((x, y)) = abs.take() {
            dlog::bump(&MOVES);
            self.send(Cmd::MouseAbs { x, y });
        }
        if *rel != (0, 0) {
            dlog::bump(&MOVES);
            self.send(Cmd::MouseRel { dx: rel.0, dy: rel.1 });
            *rel = (0, 0);
        }
    }

    /// Posición en pantalla -> coordenadas absolutas 0..32767 según la resolución mostrada.
    fn abs_at(&self, pos: Pos2, img: Rect) -> (i64, i64) {
        map_abs(pos, img, self.vsize)
    }

    fn handle_input(&mut self, ctx: &egui::Context, img: Rect, hovered_video: bool, clicked_video: bool) {
        if dlog::enabled() {
            for e in ctx.input(|i| i.events.clone()) {
                if let egui::Event::PointerButton { pos, button, pressed, .. } = e {
                    dlog!("ventana: botón {button:?} {} en {pos:?} (captura={}, vídeo={img:?})", if pressed { "down" } else { "up" }, self.captured);
                }
            }
        }
        if self.hook_ok && hook::take_paste() {
            self.paste_clipboard();
        }
        if hook::take_cancel() && self.job.is_some() {
            self.cancel_job();
        }
        if self.hook_ok && hook::take_chord_release() {
            self.set_capture_why(ctx, false, "atajo Ctrl+Shift+F12 (hook)");
        }
        let focused = ctx.input(|i| i.focused);
        if self.captured && !focused && !self.opts.start_captured {
            self.set_capture_why(ctx, false, "pérdida de foco"); // suelta todo (kall, mbr 0, mba 0)
        }
        if !self.captured {
            if clicked_video {
                self.set_capture(ctx, true);
            }
            // sin captura: Esc cancela un trabajo; atajo de pegar; Ctrl+V (texto del portapapeles de la ventana)
            let evs = ctx.input(|i| i.events.clone());
            if self.job.is_some() && evs.iter().any(|e| matches!(e, egui::Event::Key { key: Key::Escape, pressed: true, .. })) {
                self.cancel_job();
            }
            let chord = evs.iter().any(|e| self.paste_chord_hit(e));
            if chord {
                self.paste_clipboard();
            } else {
                for e in &evs {
                    if let egui::Event::Paste(t) = e {
                        self.start_paste(t.clone());
                    }
                }
            }
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        if self.job.is_some() {
            // pegado o repetición en curso: no se reenvía nada (Esc cancela); el hook ya traga las teclas
            if events.iter().any(|e| matches!(e, egui::Event::Key { key: Key::Escape, pressed: true, .. })) {
                self.cancel_job();
            }
            return;
        }
        let rel = self.mode == MouseMode::Relative;
        let (mut pend_abs, mut pend_rel): (Option<(i64, i64)>, (i64, i64)) = (None, (0, 0));
        for ev in &events {
            // el orden respecto a teclas, botones y rueda se conserva: antes de ellos se vuelca el movimiento acumulado
            if matches!(ev, egui::Event::Key { .. } | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. }) {
                self.flush_mouse(&mut pend_abs, &mut pend_rel);
            }
            match ev {
                egui::Event::Key { key, physical_key, pressed, repeat, modifiers, .. } => {
                    if self.paste_chord_hit(ev) {
                        self.send(Cmd::ReleaseAll);
                        self.paste_clipboard();
                        continue;
                    }
                    if *key == Key::F12 && *pressed && modifiers.ctrl && modifiers.shift {
                        self.set_capture_why(ctx, false, "atajo Ctrl+Shift+F12 (evento)");
                        return;
                    }
                    if *repeat {
                        continue; // el autorepetido lo hace el servidor
                    }
                    // Vía de respaldo por eventos de ventana: el hook traga las teclas que procesa, así que lo que llega
                    // aquí no pasó por él (hook roto o sin eventos). `claim` evita duplicados.
                    if let Some(u) = egui_hid(physical_key.unwrap_or(*key)) {
                        if !hook::claim(u, *pressed) {
                            continue;
                        }
                        dlog::bump(&KEYS);
                        dlog!("evento ventana -> Key usage={u:#04x} down={pressed}");
                        self.send(Cmd::Key { usage: u, down: *pressed });
                    }
                }
                egui::Event::Text(_) | egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) => {}
                egui::Event::PointerMoved(p) if !rel => {
                    if img.contains(*p) || self.held != 0 {
                        pend_abs = Some(self.abs_at(*p, img)); // último valor gana; se envía una vez por fotograma
                    }
                }
                egui::Event::MouseMoved(d) if rel => {
                    self.rel_acc += *d;
                    let (dx, dy) = (self.rel_acc.x.trunc(), self.rel_acc.y.trunc());
                    self.rel_acc -= Vec2::new(dx, dy);
                    pend_rel.0 += dx as i64;
                    pend_rel.1 += dy as i64;
                }
                egui::Event::PointerButton { pos, button, pressed, .. } => {
                    let (name, bit) = match button {
                        PointerButton::Primary => ("left", 1u8),
                        PointerButton::Secondary => ("right", 2),
                        PointerButton::Middle => ("middle", 4),
                        _ => continue,
                    };
                    if *pressed && !rel && !img.contains(*pos) {
                        continue; // clic fuera del vídeo (barras): no es para el servidor
                    }
                    if *pressed {
                        self.held |= bit;
                    } else if self.held & bit == 0 {
                        continue;
                    } else {
                        self.held &= !bit;
                    }
                    if !rel {
                        let (x, y) = self.abs_at(*pos, img);
                        dlog!("botón {name} {} abs ({x},{y}) vídeo={:?}", if *pressed { "down" } else { "up" }, self.vsize);
                        self.send(Cmd::MouseAbs { x, y });
                    } else {
                        dlog!("botón {name} {} rel", if *pressed { "down" } else { "up" });
                    }
                    dlog::bump(&BUTTONS);
                    self.send(Cmd::Button { name, down: *pressed, rel });
                }
                egui::Event::MouseWheel { unit, delta, .. } if hovered_video || rel => {
                    self.wheel_acc += match unit {
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Page => delta.y * 10.0,
                        egui::MouseWheelUnit::Point => delta.y / 40.0,
                    };
                    let n = self.wheel_acc.trunc();
                    if n != 0.0 {
                        self.wheel_acc -= n;
                        self.send(Cmd::Wheel { amount: n as i64, rel });
                    }
                }
                _ => {}
            }
        }
        self.flush_mouse(&mut pend_abs, &mut pend_rel);
        // modificadores por diferencia de estado de la ventana (con hook sanos no hay cambios que ver)
        {
            let m = ctx.input(|i| i.modifiers);
            let p = self.mods_prev;
            for (now, before, usage) in [
                (m.ctrl, p.ctrl, 0xE0u8),
                (m.shift, p.shift, 0xE1),
                (m.alt, p.alt, 0xE2),
                (m.mac_cmd, p.mac_cmd, 0xE3),
            ] {
                if now != before && hook::claim(usage, now) {
                    dlog::bump(&KEYS);
                    dlog!("modificador ventana usage={usage:#04x} down={now}");
                    self.send(Cmd::Key { usage, down: now });
                }
            }
            self.mods_prev = m;
        }
    }
}

/// Posición del puntero sobre el rectángulo del vídeo -> 0..32767 (resolución `vsize`; si aún no hay vídeo, la del rectángulo).
pub fn map_abs(pos: Pos2, img: Rect, vsize: (u32, u32)) -> (i64, i64) {
    let (vw, vh) = if vsize.0 > 1 { (vsize.0 as i64, vsize.1 as i64) } else { (img.width() as i64, img.height() as i64) };
    let px = (pos.x - img.min.x) / img.width().max(1.0) * vw as f32;
    let py = (pos.y - img.min.y) / img.height().max(1.0) * vh as f32;
    abs_from_pixels(px as f64, py as f64, vw.max(2), vh.max(2)).unwrap_or((0, 0))
}

fn egui_hid(key: Key) -> Option<u8> {
    let n = format!("{key:?}");
    let b = n.as_bytes();
    Some(match n.as_str() {
        _ if b.len() == 1 && b[0].is_ascii_uppercase() => 0x04 + (b[0] - b'A'),
        "Num0" => 0x27,
        _ if n.starts_with("Num") && b.len() == 4 && b[3].is_ascii_digit() => 0x1E + (b[3] - b'1'),
        _ if n.starts_with('F') && n[1..].parse::<u8>().map(|v| (1..=12).contains(&v)).unwrap_or(false) => 0x3A + n[1..].parse::<u8>().unwrap() - 1,
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2A,
        "Tab" => 0x2B,
        "Space" => 0x2C,
        "Minus" => 0x2D,
        "Equals" => 0x2E,
        "OpenBracket" => 0x2F,
        "CloseBracket" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backtick" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "Insert" => 0x49,
        "Home" => 0x4A,
        "PageUp" => 0x4B,
        "Delete" => 0x4C,
        "End" => 0x4D,
        "PageDown" => 0x4E,
        "ArrowRight" => 0x4F,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        _ => return None,
    })
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events();
        self.poll_video_source();
        self.upload_video(&ctx);

        // ---- barra superior
        let mut action: Option<Action> = None;
        let paste_before = self.paste;
        let (mode_name, mode_col) = theme::mode_style(self.status.as_ref().and_then(|s| s.4.as_deref()));
        // cabecera: franja de 1 línea con el color del modo
        egui::Panel::top("head").frame(egui::Frame::new().fill(theme::NEGRO).inner_margin(egui::Margin::symmetric(8, 4)).stroke(egui::Stroke::new(1.0, mode_col))).show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(t) = &self.skull {
                    ui.add(egui::Image::new(t).fit_to_exact_size(Vec2::splat(24.0)));
                }
                ui.label(egui::RichText::new("RiotKVM").strong().size(16.0).color(theme::HUESO));
                if let Some(t) = self.mode_icons.iter().find(|(n, _)| *n == mode_name).map(|(_, t)| t) {
                    ui.add(egui::Image::new(t).fit_to_exact_size(Vec2::splat(20.0)));
                }
                ui.label(egui::RichText::new(mode_name).strong().size(16.0).color(mode_col));
                ui.separator();
                let label = if self.captured { "[ Liberar ]" } else { "[ Capturar ]" };
                if ui.button(label).clicked() {
                    let on = !self.captured;
                    self.set_capture(&ctx, on);
                }
                ui.separator();
                let mut new_mouse = self.mode;
                ui.label(egui::RichText::new("ratón").color(theme::GRIS));
                ui.selectable_value(&mut new_mouse, MouseMode::Absolute, "abs");
                ui.selectable_value(&mut new_mouse, MouseMode::Relative, "rel");
                if new_mouse != self.mode {
                    self.mode = new_mouse;
                    dlog!("modo de ratón -> {:?} (captura={})", self.mode, self.captured);
                    if self.captured {
                        // cambio en caliente: se sueltan botones y se reajusta el cursor local
                        self.send(Cmd::ReleaseAll);
                        self.held = 0;
                        self.rel_acc = Vec2::ZERO;
                        let rel = self.mode == MouseMode::Relative;
                        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!rel));
                        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(grab_mode(rel)));
                    }
                }
                ui.separator();
                // ---- Vídeo
                ui.menu_button("Vídeo", |ui| {
                    ui.label(egui::RichText::new("fuente").color(theme::GRIS));
                    let mut pick_src: Option<String> = None;
                    if ui.selectable_label(self.src_pref == "auto" || self.src_pref.is_empty(), "Automática (capturadora si hay)").clicked() {
                        pick_src = Some("auto".into());
                    }
                    for d in &self.devices {
                        let l = format!("{}{}{}{}", d.name, if d.node.is_empty() { String::new() } else { format!(" [{}]", d.node) }, if d.capture_card { " (capturadora)" } else { "" }, if d.virtual_cam { " (virtual)" } else { "" });
                        if ui.selectable_label(self.src_pref.eq_ignore_ascii_case(&d.name), l).clicked() {
                            pick_src = Some(d.name.clone());
                        }
                    }
                    if ui.selectable_label(self.src_pref == "none", "Ninguna (sin vídeo)").clicked() {
                        pick_src = Some("none".into());
                    }
                    if ui.small_button("Actualizar lista").on_hover_text("Buscar cámaras y capturadoras (no enciende ninguna); si se detuvieron los reintentos, vuelve a abrir la fuente").clicked() {
                        self.refresh_devices();
                        if self.video.latest.error().map(|e| e.contains("se detienen los reintentos")).unwrap_or(false) {
                            let p = if self.src_pref.is_empty() { "auto".to_string() } else { self.src_pref.clone() };
                            self.select_source(&p);
                        }
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("resolución / fps").color(theme::GRIS));
                    let avail: Vec<(String, u32, u32, u32)> = if self.modes.is_empty() {
                        VIDEO_MODES.iter().map(|m| (m.0.to_string(), m.1, m.2, m.3)).collect()
                    } else {
                        self.modes.iter().map(|(w, h, f)| (format!("{w}x{h} · {f} fps{}", if (*w, *h, *f) == (1280, 720, 60) { " (baja latencia)" } else { "" }), *w, *h, *f)).collect()
                    };
                    let mut pick: Option<(u32, u32, u32)> = None;
                    for (label, w, h, fps) in &avail {
                        let on = (*w, *h, *fps) == (self.video.mode.w, self.video.mode.h, self.video.mode.fps);
                        if ui.selectable_label(on, label.as_str()).clicked() {
                            pick = Some((*w, *h, *fps));
                        }
                    }
                    if let Some(p) = pick_src {
                        self.select_source(&p);
                        ui.close();
                    }
                    if let Some((w, h, fps)) = pick {
                        save_video_mode(w, h, fps);
                        let mode = Mode { w, h, fps, format: "mjpeg".into() };
                        self.video.restart(mode);
                        self.tex = None;
                        self.vsize = (0, 0);
                        ui.close();
                    }
                });
                // ---- Teclado (distribución del servidor)
                ui.menu_button("Enviar teclas", |ui| {
                    for (title, list) in [("Sistema", KEYS_SYSTEM), ("Linux TTY", KEYS_TTY), ("Funciones / BIOS", KEYS_BIOS)] {
                        ui.menu_button(title, |ui| {
                            for (label, combo) in list {
                                if ui.button(*label).clicked() {
                                    action = Some(Action::Keys((*combo).into()));
                                    ui.close();
                                }
                            }
                        });
                    }
                    ui.separator();
                    ui.label("Combinación personalizada (como riotkvm key):");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.custom_keys).hint_text("ctrl+alt+del   o   alt+f4 enter").desired_width(220.0));
                    if (ui.button("Enviar").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))) && !self.custom_keys.trim().is_empty() {
                        action = Some(Action::Keys(self.custom_keys.clone()));
                        ui.close();
                    }
                    ui.separator();
                    ui.label("Mantener pulsada / repetir (entrar en la BIOS):");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.rep_combo).desired_width(70.0));
                        ui.add(egui::DragValue::new(&mut self.rep_interval).range(20..=2000).suffix(" ms"));
                        ui.add(egui::DragValue::new(&mut self.rep_secs).range(1.0..=120.0).suffix(" s"));
                    });
                    if self.job.is_some() {
                        if ui.button("Parar").clicked() {
                            action = Some(Action::StopJob);
                        }
                    } else if ui.button("Iniciar repetición").clicked() {
                        action = Some(Action::Repeat);
                        ui.close();
                    }
                });
                ui.menu_button("Pegar", |ui| {
                    if ui.button(format!("Pegar portapapeles ({})", self.opts.paste_chord.label())).clicked() {
                        action = Some(Action::PasteClip);
                        ui.close();
                    }
                    if ui.button("Escribir texto...").clicked() {
                        action = Some(Action::OpenPasteWin);
                        ui.close();
                    }
                    ui.separator();
                    ui.radio_value(&mut self.paste.physical, false, "texto (por layout del servidor)");
                    ui.radio_value(&mut self.paste.physical, true, "teclas físicas (posición US, para BIOS)");
                    ui.add(egui::Slider::new(&mut self.paste.per_key_ms, 0..=50).text("ms por tecla"));
                    ui.checkbox(&mut self.paste.confirm_always, format!("confirmar siempre (si no, solo > {CONFIRM_OVER} caracteres)"));
                });
                if ui.add(egui::Button::new("Ajustes").selected(self.settings.open)).clicked() {
                    self.settings.open = !self.settings.open;
                }
                if ui.button("Tools").on_hover_text("Flashear el firmware del dongle · preparar una microSD con Ventoy").clicked() {
                    self.tools.open = true;
                }
                let info = self.video.latest.info();
                ui.label(egui::RichText::new(if info.is_empty() { self.video.desc.clone() } else { info }).color(theme::GRIS));
            });
        });

        // ---- barra de estado
        // línea propia y siempre visible mientras haya captura: cómo salir
        if self.captured {
            egui::Panel::bottom("escape").show(ui, |ui| {
                let teclado = if self.hook_ok { "teclado total" } else { "teclado limitado" };
                let raton = if self.mode == MouseMode::Absolute { "absoluto" } else { "relativo" };
                ui.label(egui::RichText::new(format!("CAPTURADO ({teclado}, ratón {raton}) · para SOLTAR pulsa {} (o cambia de ventana)", hook::ESCAPE_HELP)).strong().color(theme::AMBAR));
            });
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let tg = |ui: &mut egui::Ui, t: theme::Tag, label: &str, val: &str, vcol: Color32| {
                    let (txt, col) = theme::tag(t);
                    ui.label(egui::RichText::new(txt).color(col));
                    ui.label(egui::RichText::new(label).color(theme::HUESO));
                    if !val.is_empty() {
                        ui.label(egui::RichText::new(val).color(vcol));
                    }
                };
                match &self.link {
                    LinkState::Connected { name, .. } => tg(ui, theme::Tag::Ok, "ble", self.tools.dongle_name().unwrap_or(name), theme::MAGENTA),
                    LinkState::Searching => tg(ui, theme::Tag::Wait, "ble", &format!("scan ({})", self.opts.transport_desc), theme::AMBAR),
                    LinkState::Connecting => tg(ui, theme::Tag::Wait, "ble", "connecting", theme::AMBAR),
                    LinkState::Reconnecting { attempt, reason } => tg(ui, theme::Tag::Fail, "ble", &format!("retry {attempt}: {reason}"), theme::ROJO),
                }
                match &self.status {
                    Some((host, hid, leds, sd, mode, boot)) => {
                        tg(ui, if *host { theme::Tag::Ok } else { theme::Tag::Wait }, "usb", if *host { "" } else { "wait" }, theme::CIAN);
                        tg(ui, if *hid { theme::Tag::Ok } else if mode.as_deref() == Some("A") { theme::Tag::Na } else { theme::Tag::Wait }, "hid", "", theme::CIAN);
                        tg(ui, if *sd { theme::Tag::Ok } else if mode.as_deref() == Some("B") { theme::Tag::Na } else { theme::Tag::Fail }, "sd", if *sd { "" } else { "no card" }, theme::ROJO);
                        if *boot {
                            ui.label(egui::RichText::new("BIOS (ratón no disponible)").color(theme::AMBAR));
                        }
                        let led = |on: bool, n: &str| egui::RichText::new(n).color(if on { theme::AMBAR } else { theme::GRIS });
                        ui.label(led(leds & 1 != 0, "Num"));
                        ui.label(led(leds & 2 != 0, "Caps"));
                        ui.label(led(leds & 4 != 0, "Scroll"));
                    }
                    None => {
                        tg(ui, theme::Tag::Wait, "usb", "?", theme::AMBAR);
                    }
                }
                // línea compacta; el detalle (latencias, edad del cuadro, contadores) va en el tooltip
                ui.separator();
                let rtt = self.rtt_us.map(|r| format!(" · {:.0} ms", r as f64 / 1000.0)).unwrap_or_default();
                let errs = ERRORS.load(Relaxed);
                let ws = riotkvm_core::latency::snapshot();
                let mut det = format!(
                    "vídeo {}x{} · edad {:.1}→{:.1} ms · dec {:.1} ms · descartados {}\nenviado: teclas {} · clics {} · mov {} · err {} · hook {} (vio {})",
                    self.vsize.0, self.vsize.1, self.age_ms, self.age_end_ms, self.dec_ms,
                    self.video.latest.dropped.load(std::sync::atomic::Ordering::Relaxed),
                    KEYS.load(Relaxed), BUTTONS.load(Relaxed), MOVES.load(Relaxed), errs, if self.hook_ok { "ok" } else { "NO" }, dlog::HOOK_SEEN.load(Relaxed)
                );
                if let Some(r) = self.rtt_us {
                    det.push_str(&format!("\nRTT {:.1} ms", r as f64 / 1000.0));
                }
                if ws.writes > 0 {
                    det.push_str(&format!("\nevento→write p50 {:.1} p99 {:.1} ms (n {}) · write {:.2} ms", ws.ev_p50_ms, ws.ev_p99_ms, ws.ev_n, ws.write_avg_ms));
                }
                ui.label(format!("{}p {:.0} fps{rtt}", self.vsize.1, self.fps)).on_hover_text(det);
                if errs > 0 {
                    ui.colored_label(theme::ROJO, format!("err {errs}"));
                }
                if !self.captured {
                    ui.separator();
                    ui.label(egui::RichText::new("clic para capturar").color(theme::GRIS));
                }
                if let Some((m, t, err)) = &self.note {
                    if t.elapsed() < Duration::from_secs(25) {
                        ui.separator();
                        ui.colored_label(if *err { theme::AMBAR } else { theme::VERDE }, m.as_str());
                    }
                }
                if let Some((label, done, total)) = &self.job {
                    ui.separator();
                    if *total > 0 {
                        ui.label(format!("{label}: {done}/{total}"));
                    } else {
                        ui.label(label.as_str());
                    }
                    if ui.small_button("Cancelar (Esc)").clicked() {
                        action = Some(Action::StopJob);
                    }
                }
                if let Some((m, t)) = &self.dev_err {
                    if t.elapsed() < Duration::from_secs(6) {
                        ui.separator();
                        ui.colored_label(theme::ROJO, m.as_str());
                    }
                }
            });
        });

        // ---- vídeo
        let mut img_rect = Rect::NOTHING;
        let (mut hov, mut clicked) = (false, false);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::BLACK)).show(ui, |ui| {
            let avail = ui.available_rect_before_wrap();
            let resp = ui.allocate_rect(avail, Sense::click_and_drag());
            if let Some(t) = &self.tex {
                let (vw, vh) = (self.vsize.0.max(1) as f32, self.vsize.1.max(1) as f32);
                let s = (avail.width() / vw).min(avail.height() / vh);
                img_rect = Rect::from_center_size(avail.center(), Vec2::new(vw * s, vh * s));
                ui.painter().image(t.id(), img_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                if self.captured {
                    // marco verde mientras hay captura (el atajo para soltar ya está en la barra inferior)
                    ui.painter().rect_stroke(img_rect, 0.0, egui::Stroke::new(2.0, theme::VERDE), egui::StrokeKind::Inside);
                }
            } else {
                img_rect = avail;
                let msg = self.video.latest.error().map(|e| format!("Sin vídeo: {e}\n(si dice 'recursos': otro programa o ventana usa la capturadora; se reintenta cada 2 s)")).unwrap_or_else(|| "Esperando vídeo...".into());
                ui.painter().text(avail.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(18.0), Color32::GRAY);
            }
            hov = resp.hovered();
            clicked = resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed()) && img_rect.contains(ui.input(|i| i.pointer.interact_pos()).unwrap_or(Pos2::ZERO));
        });
        // ---- ventana "Escribir texto" y confirmación de pegados largos
        if self.paste_win {
            let mut open = true;
            let mut text = std::mem::take(&mut self.paste_text);
            egui::Window::new("Escribir texto en el servidor").open(&mut open).default_width(460.0).resizable(true).show(&ctx, |ui| {
                ui.label(format!("Se teclea con el layout {} ({}).", if self.paste.physical { "us (teclas físicas)" } else { self.layout.as_str() }, if self.paste.physical { "BIOS" } else { "servidor" }));
                let alto = (ui.available_height() - 32.0).max(120.0); // el cuadro crece con la ventana
                ui.add_sized([ui.available_width(), alto], egui::TextEdit::multiline(&mut text).desired_rows(8));
                ui.horizontal(|ui| {
                    ui.label(format!("{} caracteres", text.chars().count()));
                    if ui.button("Teclear en el servidor").clicked() {
                        action = Some(Action::PasteWinText);
                    }
                    if ui.button("Limpiar").clicked() {
                        text.clear();
                    }
                });
            });
            self.paste_text = text;
            self.paste_win = open;
        }
        if let Some(p) = &self.pending {
            let n = p.text.chars().count();
            let lines = p.text.lines().count();
            let est = n as f32 * self.paste.per_key_ms as f32 / 1000.0;
            let preview: String = p.text.chars().take(200).collect();
            let mut bads: Vec<char> = Vec::new();
            for c in &p.bad {
                if !bads.contains(c) && bads.len() < 8 {
                    bads.push(*c);
                }
            }
            egui::Window::new("Confirmar pegado").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).show(&ctx, |ui| {
                ui.label(format!("{n} caracteres, {lines} líneas, layout {}; tiempo estimado ~{est:.0} s (+ esperas del enlace).", p.layout));
                if !p.bad.is_empty() {
                    ui.colored_label(theme::AMBAR, format!("{} caracteres NO representables en este layout (se saltarán): {}", p.bad.len(), bads.iter().map(|c| if c.is_control() { format!("U+{:04X}", *c as u32) } else { c.to_string() }).collect::<Vec<_>>().join(" ")));
                }
                ui.add(egui::Label::new(egui::RichText::new(preview).monospace().weak()).wrap());
                ui.horizontal(|ui| {
                    if ui.button("Teclear").clicked() {
                        action = Some(Action::ConfirmPaste);
                    }
                    if ui.button("Cancelar").clicked() {
                        action = Some(Action::CancelPending);
                    }
                });
            });
        }
        // ---- ventana de Ajustes (settings.rs)
        let info = crate::settings::Info { layout: &self.layout, mouse_abs: self.mode == MouseMode::Absolute, paste_chord: self.opts.paste_chord.label(), transport: &self.opts.transport_desc };
        match self.settings.show(&ctx, &info) {
            Some(crate::settings::Change::Layout(n)) => self.set_layout(n),
            Some(crate::settings::Change::Opts(o, msg)) => {
                self.send(Cmd::SetOpts(o));
                self.note(msg, false);
            }
            Some(crate::settings::Change::Led(l)) => self.send(Cmd::SetLed(l)),
            None => {}
        }
        self.tools.show(&ctx);
        if let Some(n) = self.tools.take_rename() {
            self.send(Cmd::SetName(n));
        }
        if self.paste != paste_before {
            self.paste.save();
        }
        if let Some(a) = action.take() {
            self.apply_action(a);
        }
        self.handle_input(&ctx, img_rect, hov, clicked);

        // ---- verificación automática (--shot / --exit-after)
        if let Some((delay, path)) = self.opts.shot.clone() {
            if !self.shot_done && self.started.elapsed() >= delay {
                self.shot_done = true;
                let p = path.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| save_png(&p, &img))));
            }
        }
        if let Some(d) = self.opts.exit_after {
            if self.started.elapsed() >= d {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if let Some(t) = self.last_arrived {
            self.age_end_ms = t.elapsed().as_secs_f32() * 1e3; // edad del cuadro mostrado al terminar de construir la UI (justo antes de presentar)
        }
        if dlog::enabled() && self.last_stat_log.elapsed() >= Duration::from_secs(1) {
            self.last_stat_log = Instant::now();
            dlog!("stats vídeo: {:.0} fps edad {:.1}→{:.1} ms dec {:.1} ms | hook_vio={} BLE {:?}", self.fps, self.age_ms, self.age_end_ms, self.dec_ms, dlog::HOOK_SEEN.load(Relaxed), riotkvm_core::latency::snapshot());
        }
        ctx.request_repaint_after(Duration::from_millis(200)); // refresco de la barra aunque no haya vídeo
    }

    fn on_exit(&mut self) {
        dlog!("salida: soltando todo");
        hook::set_captured(false);
        self.video.latest.stop();
        self.session.send(Cmd::ReleaseAll);
    }
}

fn load_png(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    Some(ctx.load_texture(name, ci, TextureOptions::NEAREST))
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) {
    let (w, h) = (img.size[0] as u32, img.size[1] as u32);
    let mut raw = Vec::with_capacity((w * h * 4) as usize);
    for p in &img.pixels {
        raw.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
    }
    if let Some(buf) = image::RgbaImage::from_raw(w, h, raw) {
        let _ = buf.save(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abs_mapping_corners_and_center() {
        // vídeo 1920x1080 mostrado escalado en un rectángulo 960x540 situado en (100, 50)
        let img = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(960.0, 540.0));
        assert_eq!(map_abs(Pos2::new(100.0, 50.0), img, (1920, 1080)), (0, 0));
        assert_eq!(map_abs(Pos2::new(1060.0, 590.0), img, (1920, 1080)), (32767, 32767));
        let (cx, cy) = map_abs(Pos2::new(580.0, 320.0), img, (1920, 1080));
        assert!((cx - 16384).abs() <= 20 && (cy - 16384).abs() <= 20, "{cx},{cy}");
        // fuera del rectángulo se satura
        assert_eq!(map_abs(Pos2::new(0.0, 0.0), img, (1920, 1080)), (0, 0));
        assert_eq!(map_abs(Pos2::new(5000.0, 5000.0), img, (1920, 1080)), (32767, 32767));
    }

    #[test]
    fn egui_key_names() {
        assert_eq!(egui_hid(Key::A), Some(0x04));
        assert_eq!(egui_hid(Key::Z), Some(0x1D));
        assert_eq!(egui_hid(Key::Num1), Some(0x1E));
        assert_eq!(egui_hid(Key::Num0), Some(0x27));
        assert_eq!(egui_hid(Key::F5), Some(0x3E));
        assert_eq!(egui_hid(Key::ArrowUp), Some(0x52));
        assert_eq!(egui_hid(Key::Delete), Some(0x4C));
    }
}

/// Modo de captura del cursor en modo relativo. macOS (winit) NO admite `Confined` (devuelve `NotSupported` y el
/// cursor sigue moviéndose por el escritorio local): allí se usa `Locked`, que lo deja quieto y sigue entregando
/// los desplazamientos crudos (`MouseMoved`). En Windows y Linux se mantiene `Confined`, que ya funcionaba.
fn grab_mode(on: bool) -> egui::CursorGrab {
    if !on {
        egui::CursorGrab::None
    } else if cfg!(target_os = "macos") {
        egui::CursorGrab::Locked
    } else {
        egui::CursorGrab::Confined
    }
}
