//! Ventana de Ajustes (pestañas): equipo controlado (compatibilidad de los clics del ratón), teclado del servidor, luz
//! del dongle e información. No toca la sesión directamente: devuelve un `Change` y `App` lo aplica (`Cmd::SetOpts`,
//! `Cmd::SetLayout`, `Cmd::SetLed`).
//!
//! Luz: el ajuste vive en el dongle (NVS, firmware 1.2.0+). La pestaña muestra el que el dongle reporta al conectar
//! (`Event::Led`); sin enlace, el último conocido (`led.txt`). Se aplica en caliente al tocarla.
//!
//! Compatibilidad con el SO destino (comprobado con hardware): Fedora no reacciona a los botones del informe ABSOLUTO;
//! el macOS destino solo reacciona a ellos; Windows funciona con los botones por el ratón relativo (el defecto).
//! Se guarda en `target.txt` (directorio de estado de `riotkvm_core::config`); `RIOTKVM_ABS_BUTTONS` manda al arrancar.

use crate::{dlog, hook, theme};
use riotkvm_core::led::{Led, DEFAULT_BRIGHT};
use riotkvm_core::session::{abs_buttons_env_override, SessionOpts, TargetOs};
use eframe::egui;

const FILE: &str = "target.txt";

/// Compatibilidad con el equipo controlado: un SO (preajuste) o manual (`os: None`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Compat {
    pub os: Option<TargetOs>,
    /// Clics y rueda del ratón absoluto por el ratón relativo (true) o por el informe absoluto (false).
    pub via_rel: bool,
}

impl Default for Compat {
    fn default() -> Self {
        Compat { os: Some(TargetOs::Windows), via_rel: TargetOs::Windows.abs_buttons_via_rel() }
    }
}

impl Compat {
    pub fn opts(&self) -> SessionOpts {
        SessionOpts { abs_buttons_via_rel: self.os.map(|o| o.abs_buttons_via_rel()).unwrap_or(self.via_rel) }
    }

    /// `os=macos|windows|linux|fedora|manual` y `clicks=abs|rel` (solo cuenta en manual).
    pub fn from_text(t: &str) -> Compat {
        let mut c = Compat::default();
        let mut manual = false;
        for l in t.lines() {
            match l.split_once('=') {
                Some(("os", v)) if v.trim() == "manual" => manual = true,
                Some(("os", v)) => {
                    if let Some(o) = TargetOs::parse(v) {
                        c.os = Some(o);
                    }
                }
                Some(("clicks", v)) => c.via_rel = v.trim() != "abs",
                _ => {}
            }
        }
        if manual {
            c.os = None;
        } else if let Some(o) = c.os {
            c.via_rel = o.abs_buttons_via_rel();
        }
        c
    }

    pub fn to_text(&self) -> String {
        let via_rel = self.opts().abs_buttons_via_rel;
        format!("os={}\nclicks={}\n", self.os.map(|o| o.name()).unwrap_or("manual"), if via_rel { "rel" } else { "abs" })
    }

    fn load() -> Compat {
        riotkvm_core::config::load(FILE).map(|t| Compat::from_text(&t)).unwrap_or_default()
    }

    fn save(&self) {
        riotkvm_core::config::save(FILE, &self.to_text());
    }
}

/// Opciones con las que arranca la sesión: `RIOTKVM_ABS_BUTTONS` > lo guardado > Windows (clics por el relativo).
pub fn initial_session_opts() -> SessionOpts {
    match abs_buttons_env_override() {
        Some(v) => SessionOpts { abs_buttons_via_rel: v },
        None => Compat::load().opts(),
    }
}

fn path_label(via_rel: bool) -> &'static str {
    if via_rel {
        "ratón relativo"
    } else {
        "informe absoluto"
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Target,
    Keyboard,
    Light,
    Info,
}

/// Muestras rápidas de color: la paleta de docs/DESIGN.md.
const SWATCHES: [(egui::Color32, &str); 7] = [
    (egui::Color32::from_rgb(0x00, 0xFF, 0x00), "verde"),
    (egui::Color32::from_rgb(0xFF, 0x00, 0x00), "rojo"),
    (egui::Color32::from_rgb(0xFF, 0x80, 0x00), "naranja"),
    (egui::Color32::from_rgb(0x00, 0x00, 0xFF), "azul"),
    (egui::Color32::from_rgb(0x80, 0x00, 0xFF), "lila"),
    (egui::Color32::from_rgb(0xFF, 0xFF, 0x00), "amarillo"),
    (egui::Color32::from_rgb(0xFF, 0xFF, 0xFF), "blanco"),
];

/// Estado de la pestaña Luz.
struct Light {
    /// Ajuste mostrado (el del dongle si se conoce).
    led: Led,
    /// Color y brillo para "Color" aunque ahora esté en auto/apagado.
    rgb: [u8; 3],
    bright: u8,
    /// `Some(false)` = el firmware conectado no admite control de la luz.
    supported: Option<bool>,
}

impl Light {
    fn new(led: Led) -> Light {
        let mut l = Light { led, rgb: [0x00, 0xFF, 0x00], bright: DEFAULT_BRIGHT, supported: None };
        l.remember(led);
        l
    }

    fn remember(&mut self, led: Led) {
        self.led = led;
        if let Led::Color { rgb, bright } = led {
            self.rgb = rgb;
            self.bright = bright;
        }
    }
}

/// Lo que la ventana necesita saber de la app para pintarse.
pub struct Info<'a> {
    pub layout: &'a str,
    pub mouse_abs: bool,
    pub paste_chord: &'a str,
    pub transport: &'a str,
}

/// Cambio pedido desde la ventana; lo aplica `App`.
pub enum Change {
    Layout(&'static str),
    /// Opciones nuevas de la sesión + texto para la barra de estado.
    Opts(SessionOpts, String),
    /// Luz del dongle.
    Led(Led),
}

pub struct Settings {
    pub open: bool,
    tab: Tab,
    compat: Compat,
    /// `RIOTKVM_ABS_BUTTONS` fijó el arranque (se muestra el aviso hasta que se elija otra cosa aquí).
    env_note: Option<bool>,
    light: Light,
}

impl Settings {
    pub fn new() -> Settings {
        let mut compat = Compat::load();
        let env = abs_buttons_env_override();
        if let Some(v) = env {
            if v != compat.opts().abs_buttons_via_rel {
                compat = Compat { os: None, via_rel: v }; // se muestra lo que de verdad está en uso (no se guarda)
            }
        }
        Settings { open: false, tab: Tab::Target, compat, env_note: env, light: Light::new(Led::load_saved().unwrap_or(Led::Auto)) }
    }

    /// Luz reportada por la sesión (`Event::Led`): el estado real del dongle, o `None` si su firmware no la admite.
    pub fn on_led(&mut self, led: Option<Led>) {
        match led {
            Some(l) => {
                self.light.supported = Some(true);
                if l != self.light.led {
                    l.save();
                }
                self.light.remember(l);
            }
            None => self.light.supported = Some(false),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, info: &Info) -> Option<Change> {
        if !self.open {
            return None;
        }
        let mut open = true;
        let mut change = None;
        egui::Window::new("Ajustes").open(&mut open).default_pos([60.0, 80.0]).default_width(460.0).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Target, "Servidor");
                ui.selectable_value(&mut self.tab, Tab::Keyboard, "Teclado");
                ui.selectable_value(&mut self.tab, Tab::Light, "Luz");
                ui.selectable_value(&mut self.tab, Tab::Info, "Información");
            });
            ui.separator();
            // contenido con scroll que ocupa todo el alto: la ventana se puede estirar en vertical
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                change = match self.tab {
                    Tab::Target => self.target_tab(ui, info),
                    Tab::Keyboard => keyboard_tab(ui, info),
                    Tab::Light => self.light_tab(ui),
                    Tab::Info => {
                        info_tab(ui, info);
                        None
                    }
                };
            });
        });
        self.open = open;
        change
    }

    fn target_tab(&mut self, ui: &mut egui::Ui, info: &Info) -> Option<Change> {
        ui.label(egui::RichText::new("Sistema del servidor").strong());
        ui.label(egui::RichText::new("Si el clic no responde, cambia el sistema.").color(theme::GRIS));
        ui.add_space(6.0);
        let before = self.compat;
        ui.horizontal(|ui| {
            for o in TargetOs::ALL {
                if ui.selectable_label(self.compat.os == Some(o), o.label()).clicked() {
                    self.compat = Compat { os: Some(o), via_rel: o.abs_buttons_via_rel() };
                }
            }
        });
        ui.add_space(6.0);
        egui::CollapsingHeader::new("Avanzado").default_open(self.compat.os.is_none()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Clics por:");
                if ui.radio(!self.compat.via_rel, "absoluto").clicked() {
                    self.compat = Compat { os: None, via_rel: false };
                }
                if ui.radio(self.compat.via_rel, "relativo").clicked() {
                    self.compat = Compat { os: None, via_rel: true };
                }
            });
        });
        if !info.mouse_abs {
            let msg = if self.compat.os == Some(TargetOs::Macos) { "Ratón en rel: en macOS el clic no funciona, usa abs." } else { "Ratón en rel: este ajuste no se aplica." };
            ui.colored_label(theme::AMBAR, msg);
        } else if self.env_note.is_some() {
            ui.colored_label(theme::AMBAR, "RIOTKVM_ABS_BUTTONS fijó el arranque.");
        }
        if self.compat == before {
            return None;
        }
        self.compat.save();
        self.env_note = None;
        let opts = self.compat.opts();
        let who = self.compat.os.map(|o| o.label()).unwrap_or("manual");
        dlog!("compatibilidad destino -> {who}: abs_buttons_via_rel={}", opts.abs_buttons_via_rel);
        Some(Change::Opts(opts, format!("equipo controlado: {who} (clics por el {})", path_label(opts.abs_buttons_via_rel))))
    }
}

impl Settings {
    fn light_tab(&mut self, ui: &mut egui::Ui) -> Option<Change> {
        let l = &mut self.light;
        let before = l.led;
        let is_color = matches!(l.led, Led::Color { .. });
        ui.horizontal(|ui| {
            if ui.selectable_label(l.led == Led::Auto, "Automático").on_hover_text("Color según modo, BLE y SD").clicked() {
                l.led = Led::Auto;
            }
            if ui.selectable_label(is_color, "Color").clicked() && !is_color {
                l.led = Led::Color { rgb: l.rgb, bright: l.bright };
            }
            if ui.selectable_label(l.led == Led::Off, "Apagado").clicked() {
                l.led = Led::Off;
            }
        });
        // se decide DESPUÉS de los botones: si el usuario acaba de elegir Automático o Apagado, no se vuelve a Color
        if matches!(l.led, Led::Color { .. }) {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(&mut l.rgb);
                ui.add_space(6.0);
                for (c, name) in SWATCHES {
                    let b = egui::Button::new("").fill(c).min_size(egui::vec2(18.0, 18.0)).selected(l.rgb == [c.r(), c.g(), c.b()]);
                    if ui.add(b).on_hover_text(name).clicked() {
                        l.rgb = [c.r(), c.g(), c.b()];
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Brillo");
                ui.add(egui::Slider::new(&mut l.bright, 0..=100).suffix(" %"));
            });
            l.led = Led::Color { rgb: l.rgb, bright: l.bright };
        }
        if l.supported == Some(false) {
            ui.add_space(6.0);
            ui.colored_label(theme::AMBAR, "Firmware sin control de la luz: actualízalo.");
        }
        if l.led == before {
            return None;
        }
        let led = l.led;
        led.save();
        dlog!("luz -> {led}");
        Some(Change::Led(led))
    }
}

fn keyboard_tab(ui: &mut egui::Ui, info: &Info) -> Option<Change> {
    ui.label(egui::RichText::new("Idioma del teclado del servidor").strong());
    ui.add_space(6.0);
    let mut pick = None;
    for n in riotkvm_core::layouts::LAYOUT_NAMES {
        if ui.selectable_label(info.layout == n, riotkvm_core::layouts::layout_label(n)).clicked() && info.layout != n {
            pick = Some(Change::Layout(n));
        }
    }
    pick
}

fn info_tab(ui: &mut egui::Ui, info: &Info) {
    ui.label(format!("ratón: {}", if info.mouse_abs { "absoluto" } else { "relativo (BIOS)" }));
    ui.label(format!("soltar la captura: {}", hook::ESCAPE_HELP));
    ui.label(format!("pegar el portapapeles: {}", info.paste_chord));
    ui.separator();
    ui.label(egui::RichText::new(format!("RiotKVM {} · {}", env!("CARGO_PKG_VERSION"), info.transport)).color(theme::GRIS));
    repo_link(ui);
}

/// Enlace al repositorio (código, releases e incidencias).
pub fn repo_link(ui: &mut egui::Ui) {
    let url = format!("https://github.com/{}", riotkvm_flash::release::REPO);
    ui.hyperlink_to(format!("github.com/{}", riotkvm_flash::release::REPO), &url).on_hover_text(url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_round_trips_and_maps_os() {
        for o in TargetOs::ALL {
            let c = Compat { os: Some(o), via_rel: o.abs_buttons_via_rel() };
            assert_eq!(Compat::from_text(&c.to_text()), c);
        }
        assert!(!Compat::from_text("os=macos\n").opts().abs_buttons_via_rel);
        assert!(Compat::from_text("os=fedora\nclicks=abs\n").opts().abs_buttons_via_rel, "el SO manda sobre clicks");
        let m = Compat::from_text("os=manual\nclicks=abs\n");
        assert_eq!(m, Compat { os: None, via_rel: false });
        assert_eq!(Compat::from_text(&m.to_text()), m);
        // fichero vacío o raro: Windows (clics por el relativo, el comportamiento de siempre)
        assert_eq!(Compat::from_text(""), Compat::default());
        assert_eq!(Compat::from_text("os=beos"), Compat::default());
        assert!(Compat::default().opts().abs_buttons_via_rel);
    }

    #[test]
    fn light_remembers_color_for_the_color_button() {
        let mut l = Light::new(Led::Auto);
        assert_eq!((l.rgb, l.bright), ([0x00, 0xFF, 0x00], DEFAULT_BRIGHT), "defecto: verde puro al 35 %");
        l.remember(Led::Color { rgb: [1, 2, 3], bright: 9 });
        l.remember(Led::Off);
        assert_eq!((l.led, l.rgb, l.bright), (Led::Off, [1, 2, 3], 9), "volver a Color recupera el último color");
    }
}
