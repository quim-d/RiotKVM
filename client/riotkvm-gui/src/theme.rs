//! Identidad visual RiotKVM (docs/DESIGN.md): fondo negro, verde fósforo, acentos por modo. Sin animaciones.

use eframe::egui::{self, Color32, FontFamily, FontId, TextStyle};

pub const NEGRO: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
pub const VERDE: Color32 = Color32::from_rgb(0x00, 0xFF, 0x46);
pub const VERDE_OSC: Color32 = Color32::from_rgb(0x00, 0x5A, 0x1E);
pub const AMBAR: Color32 = Color32::from_rgb(0xFF, 0xB0, 0x00);
pub const CIAN: Color32 = Color32::from_rgb(0x00, 0xDC, 0xFF);
pub const MAGENTA: Color32 = Color32::from_rgb(0xFF, 0x00, 0xC8);
pub const ROJO: Color32 = Color32::from_rgb(0xFF, 0x28, 0x28);
pub const HUESO: Color32 = Color32::from_rgb(0xD2, 0xFF, 0xD7);
pub const GRIS: Color32 = Color32::from_rgb(0x6E, 0x78, 0x6E);

/// (nombre del modo, color) según la letra A/B/C del dongle.
pub fn mode_style(m: Option<&str>) -> (&'static str, Color32) {
    match m {
        Some("A") => ("CARGO", AMBAR),
        Some("B") => ("BOARDING", CIAN),
        Some("C") => ("FULL", VERDE),
        _ => ("---", GRIS),
    }
}

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.override_text_color = None;
    v.panel_fill = NEGRO;
    v.window_fill = NEGRO;
    v.extreme_bg_color = NEGRO;
    v.faint_bg_color = Color32::from_rgb(0x06, 0x12, 0x08);
    v.window_stroke = egui::Stroke::new(1.0, VERDE);
    v.selection.bg_fill = VERDE_OSC;
    v.selection.stroke = egui::Stroke::new(1.0, VERDE);
    v.hyperlink_color = CIAN;
    v.warn_fg_color = AMBAR;
    v.error_fg_color = ROJO;
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;
    v.window_corner_radius = egui::CornerRadius::ZERO;
    v.menu_corner_radius = egui::CornerRadius::ZERO;
    let w = &mut v.widgets;
    for (wv, stroke, fg) in [(&mut w.noninteractive, GRIS, GRIS), (&mut w.inactive, VERDE_OSC, VERDE), (&mut w.hovered, VERDE, HUESO), (&mut w.active, VERDE, NEGRO), (&mut w.open, VERDE, HUESO)] {
        wv.corner_radius = egui::CornerRadius::ZERO;
        wv.bg_stroke = egui::Stroke::new(1.0, stroke);
        wv.fg_stroke = egui::Stroke::new(1.0, fg);
        wv.expansion = 0.0;
    }
    w.noninteractive.bg_fill = NEGRO;
    w.inactive.bg_fill = NEGRO;
    w.inactive.weak_bg_fill = NEGRO;
    w.hovered.bg_fill = VERDE_OSC;
    w.hovered.weak_bg_fill = VERDE_OSC;
    w.active.bg_fill = VERDE;
    w.active.weak_bg_fill = VERDE;
    w.open.bg_fill = VERDE_OSC;
    w.open.weak_bg_fill = VERDE_OSC;
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);

    // tipografía monoespaciada (la incluida en egui: sin dependencias ni licencias extra)
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::new(16.0, FontFamily::Monospace)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Monospace)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(11.0, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 4.0);
        s.spacing.button_padding = egui::vec2(6.0, 2.0);
    });
}

/// `[ OK ]` verde, `[ .. ]` ámbar, `[FAIL]` rojo, `[ -- ]` gris.
pub fn tag(ok: Tag) -> (&'static str, Color32) {
    match ok {
        Tag::Ok => ("[ OK ]", VERDE),
        Tag::Wait => ("[ .. ]", AMBAR),
        Tag::Fail => ("[FAIL]", ROJO),
        Tag::Na => ("[ -- ]", GRIS),
    }
}

#[derive(Clone, Copy)]
pub enum Tag {
    Ok,
    Wait,
    Fail,
    Na,
}
