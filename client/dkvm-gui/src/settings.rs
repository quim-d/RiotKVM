//! Ventana de Ajustes (pestañas): equipo controlado (compatibilidad de los clics del ratón), teclado del servidor e
//! información. No toca la sesión directamente: devuelve un `Change` y `App` lo aplica (`Cmd::SetOpts`, `Cmd::SetLayout`).
//!
//! Compatibilidad con el SO destino (comprobado con hardware): Fedora no reacciona a los botones del informe ABSOLUTO;
//! el macOS destino solo reacciona a ellos; Windows funciona con los botones por el ratón relativo (el defecto).
//! Se guarda en `target.txt` (directorio de estado de `dkvm_core::config`); `DKVM_ABS_BUTTONS` manda al arrancar.

use crate::{dlog, hook, theme};
use dkvm_core::session::{abs_buttons_env_override, SessionOpts, TargetOs};
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
        dkvm_core::config::load(FILE).map(|t| Compat::from_text(&t)).unwrap_or_default()
    }

    fn save(&self) {
        dkvm_core::config::save(FILE, &self.to_text());
    }
}

/// Opciones con las que arranca la sesión: `DKVM_ABS_BUTTONS` > lo guardado > Windows (clics por el relativo).
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

fn os_hint(o: TargetOs) -> &'static str {
    match o {
        TargetOs::Windows => "clics por el ratón relativo (comprobado)",
        TargetOs::Macos => "clics por el informe absoluto (comprobado)",
        TargetOs::Linux => "clics por el ratón relativo (como Fedora)",
        TargetOs::Fedora => "clics por el ratón relativo (comprobado)",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Target,
    Keyboard,
    Info,
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
}

pub struct Settings {
    pub open: bool,
    tab: Tab,
    compat: Compat,
    /// `DKVM_ABS_BUTTONS` fijó el arranque (se muestra el aviso hasta que se elija otra cosa aquí).
    env_note: Option<bool>,
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
        Settings { open: false, tab: Tab::Target, compat, env_note: env }
    }

    pub fn show(&mut self, ctx: &egui::Context, info: &Info) -> Option<Change> {
        if !self.open {
            return None;
        }
        let mut open = true;
        let mut change = None;
        egui::Window::new("Ajustes").open(&mut open).default_pos([60.0, 80.0]).default_width(460.0).resizable(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Target, "Equipo controlado");
                ui.selectable_value(&mut self.tab, Tab::Keyboard, "Teclado");
                ui.selectable_value(&mut self.tab, Tab::Info, "Información");
            });
            ui.separator();
            change = match self.tab {
                Tab::Target => self.target_tab(ui, info),
                Tab::Keyboard => keyboard_tab(ui, info),
                Tab::Info => {
                    info_tab(ui, info);
                    None
                }
            };
        });
        self.open = open;
        change
    }

    fn target_tab(&mut self, ui: &mut egui::Ui, info: &Info) -> Option<Change> {
        ui.label(egui::RichText::new("Sistema del equipo controlado").strong());
        ui.label(
            "Cambia cómo se envían los clics y la rueda del ratón absoluto al equipo controlado (por el informe absoluto o \
             por el ratón relativo). Elige el SO del servidor si el clic no responde. Se aplica al momento, sin reconectar.",
        );
        ui.add_space(4.0);
        let before = self.compat;
        for o in TargetOs::ALL {
            ui.horizontal(|ui| {
                if ui.radio(self.compat.os == Some(o), o.label()).clicked() {
                    self.compat = Compat { os: Some(o), via_rel: o.abs_buttons_via_rel() };
                }
                ui.label(egui::RichText::new(os_hint(o)).color(theme::GRIS));
            });
        }
        if ui.radio(self.compat.os.is_none(), "Manual (avanzado)").clicked() {
            self.compat.os = None;
        }
        ui.add_enabled_ui(self.compat.os.is_none(), |ui| {
            ui.indent("manual", |ui| {
                ui.horizontal(|ui| {
                    ui.label("clics y rueda:");
                    ui.radio_value(&mut self.compat.via_rel, false, "informe absoluto");
                    ui.radio_value(&mut self.compat.via_rel, true, "ratón relativo");
                });
            });
        });
        ui.add_space(4.0);
        let via_rel = self.compat.opts().abs_buttons_via_rel;
        ui.label(egui::RichText::new(format!("Ahora: clics y rueda por el {}", path_label(via_rel))).color(theme::VERDE));
        if let Some(v) = self.env_note {
            ui.colored_label(theme::AMBAR, format!("DKVM_ABS_BUTTONS fijó el arranque (clics por el {}); lo que elijas aquí manda desde ahora.", path_label(v)));
        }
        if !info.mouse_abs {
            ui.colored_label(theme::AMBAR, "El ratón está en modo relativo (rel): ahí los clics van siempre por el ratón relativo y este ajuste no influye.");
        }
        ui.label(
            egui::RichText::new("Nota: con un Mac como destino el clic en modo relativo (rel) no funciona; usa el modo absoluto (abs).")
                .color(theme::GRIS),
        );
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

fn keyboard_tab(ui: &mut egui::Ui, info: &Info) -> Option<Change> {
    ui.label(egui::RichText::new("Idioma del teclado del servidor").strong());
    ui.label("Distribución con la que el equipo controlado interpreta las teclas (pegar y escribir texto la usan). También en la barra: Teclado.");
    ui.add_space(4.0);
    let mut pick = None;
    for n in dkvm_core::layouts::LAYOUT_NAMES {
        if ui.selectable_label(info.layout == n, dkvm_core::layouts::layout_label(n)).clicked() && info.layout != n {
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
    ui.label(egui::RichText::new(format!("QKVM {} · {}", env!("CARGO_PKG_VERSION"), info.transport)).color(theme::GRIS));
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
}
