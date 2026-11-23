//! Nombres de teclas -> usos HID (página Keyboard/Keypad 0x07) y combinaciones `ctrl+alt+del`.

use crate::error::{pyrepr, usage, Result};
use crate::layouts::{Layout, Stroke};
use std::collections::HashMap;
use std::sync::OnceLock;

pub const MOD_LCTRL: u8 = 0x01;
pub const MOD_LSHIFT: u8 = 0x02;
pub const MOD_LALT: u8 = 0x04;
pub const MOD_LGUI: u8 = 0x08;
pub const MOD_RCTRL: u8 = 0x10;
pub const MOD_RSHIFT: u8 = 0x20;
pub const MOD_RALT: u8 = 0x40;
pub const MOD_RGUI: u8 = 0x80;

struct Names {
    ordered: Vec<(String, u16)>,
    map: HashMap<String, u16>,
}

fn names() -> &'static Names {
    static N: OnceLock<Names> = OnceLock::new();
    N.get_or_init(|| {
        let mut v: Vec<(String, u16)> = Vec::new();
        let fixed: &[(&str, u16)] = &[
            ("enter", 0x28), ("return", 0x28), ("esc", 0x29), ("escape", 0x29), ("backspace", 0x2A), ("bksp", 0x2A),
            ("tab", 0x2B), ("space", 0x2C), ("capslock", 0x39), ("caps", 0x39),
            ("printscreen", 0x46), ("prtsc", 0x46), ("scrolllock", 0x47), ("pause", 0x48), ("insert", 0x49), ("ins", 0x49),
            ("home", 0x4A), ("pageup", 0x4B), ("pgup", 0x4B), ("delete", 0x4C), ("del", 0x4C), ("end", 0x4D),
            ("pagedown", 0x4E), ("pgdn", 0x4E), ("right", 0x4F), ("left", 0x50), ("down", 0x51), ("up", 0x52),
            ("numlock", 0x53), ("menu", 0x65), ("app", 0x65), ("sysrq", 0x46),
            ("ctrl", 0xE0), ("lctrl", 0xE0), ("control", 0xE0), ("shift", 0xE1), ("lshift", 0xE1),
            ("alt", 0xE2), ("lalt", 0xE2), ("win", 0xE3), ("gui", 0xE3), ("super", 0xE3), ("meta", 0xE3),
            ("cmd", 0xE3), ("lwin", 0xE3),
            ("rctrl", 0xE4), ("rshift", 0xE5), ("ralt", 0xE6), ("altgr", 0xE6), ("rwin", 0xE7), ("rgui", 0xE7),
        ];
        for (n, u) in fixed {
            v.push((n.to_string(), *u));
        }
        for (i, c) in "abcdefghijklmnopqrstuvwxyz".chars().enumerate() {
            v.push((c.to_string(), 0x04 + i as u16));
        }
        for (i, c) in "1234567890".chars().enumerate() {
            v.push((c.to_string(), 0x1E + i as u16));
        }
        for i in 1..=12u16 {
            v.push((format!("f{i}"), 0x3A + i - 1));
        }
        for i in 13..=24u16 {
            v.push((format!("f{i}"), 0x68 + i - 13));
        }
        let map = v.iter().cloned().collect();
        Names { ordered: v, map }
    })
}

pub fn lookup(name: &str) -> Option<u16> {
    names().map.get(name).copied()
}

pub fn usage_name(usage: u16) -> String {
    names()
        .ordered
        .iter()
        .find(|(_, u)| *u == usage)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| format!("0x{usage:02X}"))
}

pub fn is_modifier(usage: u16) -> bool {
    (0xE0..=0xE7).contains(&usage)
}

/// Máscara de modificadores -> usos 0xE0..0xE7 (de menor a mayor bit).
pub fn mods_to_usages(mask: u8) -> Vec<u16> {
    (0..8u16).filter(|i| mask & (1 << i) != 0).map(|i| 0xE0 + i).collect()
}

/// `ctrl+alt+del` -> [(uso, mods_extra), ...] en orden de pulsación.
///
/// Cada elemento es un nombre de tecla o un único carácter (resuelto con el layout,
/// que puede añadir Shift/AltGr). `plus` = `+`. Un `+` final (`ctrl++`) también vale.
pub fn parse_combo(text: &str, layout: &Layout) -> Result<Vec<Stroke>> {
    let text = text.trim();
    if text.is_empty() {
        return usage("combinación vacía");
    }
    let parts: Vec<String> = if let Some(head) = text.strip_suffix("++") {
        if head.is_empty() {
            vec!["plus".into()]
        } else {
            head.split('+').map(String::from).chain(std::iter::once("plus".to_string())).collect()
        }
    } else {
        text.split('+').map(String::from).collect()
    };
    let mut out = Vec::new();
    for p in &parts {
        let p = p.trim();
        if p.is_empty() {
            return usage(format!("combinación mal formada: {}", pyrepr(text)));
        }
        let low = p.to_lowercase();
        if let Some(u) = lookup(&low) {
            out.push((u, 0));
        } else if let Some(hex) = low.strip_prefix("0x") {
            match u16::from_str_radix(hex, 16) {
                Ok(u) => out.push((u, 0)),
                Err(_) => return usage(format!("uso HID inválido: {}", pyrepr(p))),
            }
        } else {
            let ch = if low == "plus" { "+" } else { p };
            if ch.chars().count() != 1 {
                return usage(format!("tecla desconocida: {}", pyrepr(p)));
            }
            let strokes = layout.strokes(ch.chars().next().unwrap())?;
            if strokes.len() != 1 {
                return usage(format!("{} no es una tecla simple en el layout {}", pyrepr(p), layout.name));
            }
            out.push(strokes[0]);
        }
    }
    Ok(out)
}
