//! Distribuciones de teclado del equipo DESTINO: carácter -> pulsaciones HID.
//!
//! Una pulsación es (uso_hid, máscara_modificadores). El dongle es "tonto": aquí se
//! decide qué tecla física produce cada carácter según el layout que tiene el servidor.

use crate::error::{pyrepr, usage, Result};
use crate::keys::{MOD_LSHIFT, MOD_RALT};
use std::collections::HashMap;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

pub type Stroke = (u16, u8);
const S: u8 = MOD_LSHIFT;
const A: u8 = MOD_RALT;
pub const ENTER: u16 = 0x28;
pub const TAB: u16 = 0x2B;
pub const SPACE: u16 = 0x2C;

type Table = HashMap<char, Stroke>;

fn common() -> Table {
    let mut d = Table::new();
    for (i, c) in "abcdefghijklmnopqrstuvwxyz".chars().enumerate() {
        d.insert(c, (0x04 + i as u16, 0));
        d.insert(c.to_ascii_uppercase(), (0x04 + i as u16, S));
    }
    d.insert(' ', (SPACE, 0));
    d.insert('\n', (ENTER, 0));
    d.insert('\t', (TAB, 0));
    for (i, c) in "1234567890".chars().enumerate() {
        d.insert(c, (0x1E + i as u16, 0));
    }
    d
}

fn us_table() -> Table {
    let mut d = common();
    for (i, c) in "!@#$%^&*()".chars().enumerate() {
        d.insert(c, (0x1E + i as u16, S));
    }
    let pairs: [(u16, char, char); 11] = [
        (0x2D, '-', '_'), (0x2E, '=', '+'), (0x2F, '[', '{'), (0x30, ']', '}'), (0x31, '\\', '|'),
        (0x33, ';', ':'), (0x34, '\'', '"'), (0x35, '`', '~'), (0x36, ',', '<'), (0x37, '.', '>'),
        (0x38, '/', '?'),
    ];
    for (u, a, b) in pairs {
        d.insert(a, (u, 0));
        d.insert(b, (u, S));
    }
    d
}

fn es_table() -> Table {
    let mut d = common();
    for (i, c) in "!\"·$%&/()=".chars().enumerate() {
        d.insert(c, (0x1E + i as u16, S));
    }
    for (c, u) in [('|', 0x1E), ('@', 0x1F), ('#', 0x20), ('~', 0x21), ('€', 0x22), ('¬', 0x23)] {
        d.insert(c, (u, A));
    }
    d.insert('º', (0x35, 0));
    d.insert('ª', (0x35, S));
    d.insert('\\', (0x35, A));
    d.insert('\'', (0x2D, 0));
    d.insert('?', (0x2D, S));
    d.insert('¡', (0x2E, 0));
    d.insert('¿', (0x2E, S));
    d.insert('[', (0x2F, A));
    d.insert('+', (0x30, 0));
    d.insert('*', (0x30, S));
    d.insert(']', (0x30, A));
    d.insert('ñ', (0x33, 0));
    d.insert('Ñ', (0x33, S));
    d.insert('{', (0x34, A));
    d.insert('ç', (0x31, 0));
    d.insert('Ç', (0x31, S));
    d.insert('}', (0x31, A));
    d.insert('<', (0x64, 0));
    d.insert('>', (0x64, S));
    d.insert(',', (0x36, 0));
    d.insert(';', (0x36, S));
    d.insert('.', (0x37, 0));
    d.insert(':', (0x37, S));
    d.insert('-', (0x38, 0));
    d.insert('_', (0x38, S));
    d
}

pub struct Layout {
    pub name: &'static str,
    table: Table,
    /// Marca combinante (U+0301...) -> tecla muerta que hay que pulsar antes de la base.
    dead_marks: HashMap<char, Stroke>,
    /// Carácter de acento suelto (´ ¨ ` ^) -> su marca combinante.
    dead_chars: HashMap<char, char>,
}

impl Layout {
    pub fn strokes(&self, ch: char) -> Result<Vec<Stroke>> {
        if let Some(mark) = self.dead_chars.get(&ch) {
            // la tecla muerta sola: pulsarla y luego espacio
            return Ok(vec![self.dead_marks[mark], (SPACE, 0)]);
        }
        if let Some(s) = self.table.get(&ch) {
            return Ok(vec![*s]);
        }
        if !self.dead_marks.is_empty() {
            // vocal acentuada: tecla muerta + base
            let dec: Vec<char> = std::iter::once(ch).nfd().collect();
            if dec.len() == 2 {
                if let (Some(dead), Some(base)) = (self.dead_marks.get(&dec[1]), self.table.get(&dec[0])) {
                    return Ok(vec![*dead, *base]);
                }
            }
        }
        usage(format!(
            "el carácter {} no se puede escribir con el layout {}",
            pyrepr(&ch.to_string()),
            self.name
        ))
    }

    /// Como `text_strokes` pero sin fallar: pulsaciones de lo representable y lista de caracteres que este layout
    /// NO puede escribir (en orden de aparición, con repeticiones).
    pub fn text_strokes_lossy(&self, text: &str) -> (Vec<Stroke>, Vec<char>) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (mut out, mut bad) = (Vec::new(), Vec::new());
        for ch in text.chars() {
            match self.strokes(ch) {
                Ok(st) => out.extend(st),
                Err(_) => bad.push(ch),
            }
        }
        (out, bad)
    }

    pub fn text_strokes(&self, text: &str) -> Result<Vec<Stroke>> {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut out = Vec::new();
        for ch in text.chars() {
            out.extend(self.strokes(ch)?);
        }
        Ok(out)
    }
}

fn build_es() -> Layout {
    let dead_marks = HashMap::from([
        ('\u{0301}', (0x34, 0)), // acento agudo  ´
        ('\u{0308}', (0x34, S)), // diéresis      ¨
        ('\u{0300}', (0x2F, 0)), // acento grave  `
        ('\u{0302}', (0x2F, S)), // circunflejo   ^
    ]);
    let dead_chars = HashMap::from([
        ('´', '\u{0301}'),
        ('¨', '\u{0308}'),
        ('`', '\u{0300}'),
        ('^', '\u{0302}'),
    ]);
    Layout { name: "es", table: es_table(), dead_marks, dead_chars }
}

/// Nombres canónicos (con sus alias en `canonical`).
pub const LAYOUT_NAMES: [&str; 8] = ["es", "us", "gb", "de", "fr", "it", "pt", "latam"];

/// Descripción para el selector de la GUI.
pub fn layout_label(name: &str) -> &'static str {
    match name {
        "es" => "es · Español (España)",
        "us" => "us · English (US)",
        "gb" => "gb · English (UK)",
        "de" => "de · Deutsch",
        "fr" => "fr · Français (AZERTY)",
        "it" => "it · Italiano",
        "pt" => "pt · Português (PT)",
        "latam" => "latam · Español (Latinoamérica)",
        _ => "?",
    }
}

/// Alias aceptados -> nombre canónico.
pub fn canonical(name: &str) -> Option<&'static str> {
    Some(match name.trim().to_lowercase().replace('_', "-").as_str() {
        "es" | "es-es" => "es",
        "us" | "en" | "en-us" => "us",
        "gb" | "uk" | "en-gb" => "gb",
        "de" | "de-de" => "de",
        "fr" | "fr-fr" => "fr",
        "it" | "it-it" => "it",
        "pt" | "pt-pt" => "pt",
        "latam" | "la" | "es-la" | "es-latam" | "es-419" => "latam",
        _ => return None,
    })
}

const N: char = '\0';
/// (uso, sin modificador, Shift, AltGr, AltGr+Shift); '\0' = no hay carácter.
type Key = (u16, char, char, char, char);

/// `letters`: letras que cambian de posición (uso -> letra); `skip`: usos de letra que no llevan letra.
fn from_spec(letters: &[(u16, char)], skip: &[u16], keys: &[Key]) -> Table {
    let mut d = Table::new();
    d.insert(' ', (SPACE, 0));
    d.insert('\n', (ENTER, 0));
    d.insert('\t', (TAB, 0));
    for (i, c) in "abcdefghijklmnopqrstuvwxyz".chars().enumerate() {
        let u = 0x04 + i as u16;
        if skip.contains(&u) {
            continue;
        }
        let c = letters.iter().find(|(lu, _)| *lu == u).map(|(_, l)| *l).unwrap_or(c);
        d.insert(c, (u, 0));
        d.insert(c.to_ascii_uppercase(), (u, S));
    }
    for (u, b, s, a, asft) in keys {
        for (c, m) in [(*b, 0), (*s, S), (*a, A), (*asft, A | S)] {
            if c != N {
                d.insert(c, (*u, m));
            }
        }
    }
    d
}

/// Caracteres con AltGr sobre una tecla de letra (usos US: a=0x04 e=0x08 i=0x0C o=0x12 u=0x18 q=0x14 m=0x10).
fn altgr_letters(d: &mut Table, items: &[(u16, char, char)]) {
    for (u, lo, up) in items {
        d.entry(*lo).or_insert((*u, A));
        if up != lo {
            d.entry(*up).or_insert((*u, A | S));
        }
    }
}

fn marks(items: &[(char, u16, u8)]) -> HashMap<char, Stroke> {
    items.iter().map(|(m, u, md)| (*m, (*u, *md))).collect()
}

const ACUTE: char = '\u{0301}';
const GRAVE: char = '\u{0300}';
const CIRC: char = '\u{0302}';
const DIAER: char = '\u{0308}';
const TILDE: char = '\u{0303}';

fn build_gb() -> Layout {
    let keys: &[Key] = &[
        (0x1E, '1', '!', N, N), (0x1F, '2', '"', N, N), (0x20, '3', '£', N, N), (0x21, '4', '$', '€', N), (0x22, '5', '%', N, N),
        (0x23, '6', '^', N, N), (0x24, '7', '&', N, N), (0x25, '8', '*', N, N), (0x26, '9', '(', N, N), (0x27, '0', ')', N, N),
        (0x2D, '-', '_', N, N), (0x2E, '=', '+', N, N), (0x2F, '[', '{', N, N), (0x30, ']', '}', N, N), (0x31, '#', '~', N, N),
        (0x33, ';', ':', N, N), (0x34, '\'', '@', N, N), (0x35, '`', '¬', '¦', N), (0x36, ',', '<', N, N), (0x37, '.', '>', N, N),
        (0x38, '/', '?', N, N), (0x64, '\\', '|', N, N),
    ];
    let mut t = from_spec(&[], &[], keys);
    altgr_letters(&mut t, &[(0x04, 'á', 'Á'), (0x08, 'é', 'É'), (0x0C, 'í', 'Í'), (0x12, 'ó', 'Ó'), (0x18, 'ú', 'Ú')]);
    Layout { name: "gb", table: t, dead_marks: HashMap::new(), dead_chars: HashMap::new() }
}

fn build_de() -> Layout {
    let keys: &[Key] = &[
        (0x1E, '1', '!', N, N), (0x1F, '2', '"', '²', N), (0x20, '3', '§', '³', N), (0x21, '4', '$', N, N), (0x22, '5', '%', N, N),
        (0x23, '6', '&', N, N), (0x24, '7', '/', '{', N), (0x25, '8', '(', '[', N), (0x26, '9', ')', ']', N), (0x27, '0', '=', '}', N),
        (0x2D, 'ß', '?', '\\', N), (0x2F, 'ü', 'Ü', N, N), (0x30, '+', '*', '~', N), (0x31, '#', '\'', N, N), (0x33, 'ö', 'Ö', N, N),
        (0x34, 'ä', 'Ä', N, N), (0x35, N, '°', N, N), (0x36, ',', ';', N, N), (0x37, '.', ':', N, N), (0x38, '-', '_', N, N),
        (0x64, '<', '>', '|', N),
    ];
    // QWERTZ: la tecla Y de EE. UU. (0x1C) escribe z y la Z (0x1D) escribe y
    let mut t = from_spec(&[(0x1C, 'z'), (0x1D, 'y')], &[], keys);
    altgr_letters(&mut t, &[(0x14, '@', '@'), (0x08, '€', '€'), (0x10, 'µ', 'µ')]);
    let dead_marks = marks(&[(ACUTE, 0x2E, 0), (GRAVE, 0x2E, S), (CIRC, 0x35, 0)]);
    let dead_chars = HashMap::from([('´', ACUTE), ('`', GRAVE), ('^', CIRC)]);
    Layout { name: "de", table: t, dead_marks, dead_chars }
}

fn build_fr() -> Layout {
    // AZERTY: los dígitos son la capa Shift de la fila de números
    let keys: &[Key] = &[
        (0x1E, '&', '1', N, N), (0x1F, 'é', '2', '~', N), (0x20, '"', '3', '#', N), (0x21, '\'', '4', '{', N), (0x22, '(', '5', '[', N),
        (0x23, '-', '6', '|', N), (0x24, 'è', '7', '`', N), (0x25, '_', '8', '\\', N), (0x26, 'ç', '9', '^', N), (0x27, 'à', '0', '@', N),
        (0x2D, ')', '°', ']', N), (0x2E, '=', '+', '}', N), (0x30, '$', '£', '¤', N), (0x31, '*', 'µ', N, N), (0x33, 'm', 'M', N, N),
        (0x34, 'ù', '%', N, N), (0x35, '²', N, N, N), (0x36, ';', '.', N, N), (0x37, ':', '/', N, N), (0x38, '!', '§', N, N),
        (0x10, ',', '?', N, N), (0x64, '<', '>', N, N),
    ];
    let mut t = from_spec(&[(0x04, 'q'), (0x14, 'a'), (0x1A, 'z'), (0x1D, 'w')], &[0x10], keys);
    altgr_letters(&mut t, &[(0x08, '€', '€')]);
    let dead_marks = marks(&[(CIRC, 0x2F, 0), (DIAER, 0x2F, S)]);
    let dead_chars = HashMap::from([('¨', DIAER)]);
    Layout { name: "fr", table: t, dead_marks, dead_chars }
}

fn build_it() -> Layout {
    let keys: &[Key] = &[
        (0x1E, '1', '!', N, N), (0x1F, '2', '"', N, N), (0x20, '3', '£', N, N), (0x21, '4', '$', '€', N), (0x22, '5', '%', N, N),
        (0x23, '6', '&', N, N), (0x24, '7', '/', N, N), (0x25, '8', '(', N, N), (0x26, '9', ')', N, N), (0x27, '0', '=', N, N),
        (0x2D, '\'', '?', '`', N), (0x2E, 'ì', '^', '~', N), (0x2F, 'è', 'é', '[', '{'), (0x30, '+', '*', ']', '}'), (0x31, 'ù', '§', N, N),
        (0x33, 'ò', 'ç', '@', N), (0x34, 'à', '°', '#', N), (0x35, '\\', '|', N, N), (0x36, ',', ';', N, N), (0x37, '.', ':', N, N),
        (0x38, '-', '_', N, N), (0x64, '<', '>', N, N),
    ];
    let mut t = from_spec(&[], &[], keys);
    altgr_letters(&mut t, &[(0x08, '€', '€')]);
    Layout { name: "it", table: t, dead_marks: HashMap::new(), dead_chars: HashMap::new() }
}

fn build_pt() -> Layout {
    let keys: &[Key] = &[
        (0x1E, '1', '!', N, N), (0x1F, '2', '"', '@', N), (0x20, '3', '#', '£', N), (0x21, '4', '$', '§', N), (0x22, '5', '%', '€', N),
        (0x23, '6', '&', N, N), (0x24, '7', '/', '{', N), (0x25, '8', '(', '[', N), (0x26, '9', ')', ']', N), (0x27, '0', '=', '}', N),
        (0x2D, '\'', '?', N, N), (0x2E, '«', '»', N, N), (0x2F, '+', '*', '¨', N), (0x33, 'ç', 'Ç', N, N), (0x34, 'º', 'ª', N, N),
        (0x35, '\\', '|', N, N), (0x36, ',', ';', N, N), (0x37, '.', ':', N, N), (0x38, '-', '_', N, N), (0x64, '<', '>', N, N),
    ];
    let mut t = from_spec(&[], &[], keys);
    altgr_letters(&mut t, &[(0x08, '€', '€')]);
    let dead_marks = marks(&[(ACUTE, 0x30, 0), (GRAVE, 0x30, S), (TILDE, 0x31, 0), (CIRC, 0x31, S)]);
    let dead_chars = HashMap::from([('´', ACUTE), ('`', GRAVE), ('~', TILDE), ('^', CIRC)]);
    Layout { name: "pt", table: t, dead_marks, dead_chars }
}

fn build_latam() -> Layout {
    let keys: &[Key] = &[
        (0x35, '|', '°', '¬', N), (0x1E, '1', '!', N, N), (0x1F, '2', '"', '@', N), (0x20, '3', '#', N, N), (0x21, '4', '$', N, N),
        (0x22, '5', '%', N, N), (0x23, '6', '&', N, N), (0x24, '7', '/', N, N), (0x25, '8', '(', N, N), (0x26, '9', ')', N, N),
        (0x27, '0', '=', N, N), (0x2D, '\'', '?', '\\', N), (0x2E, '¿', '¡', N, N), (0x30, '+', '*', '~', N), (0x31, '}', ']', N, N),
        (0x33, 'ñ', 'Ñ', N, N), (0x34, '{', '[', N, N), (0x36, ',', ';', N, N), (0x37, '.', ':', N, N), (0x38, '-', '_', N, N),
        (0x64, '<', '>', N, N),
    ];
    let mut t = from_spec(&[], &[], keys);
    altgr_letters(&mut t, &[(0x08, '€', '€')]);
    let dead_marks = marks(&[(ACUTE, 0x2F, 0), (DIAER, 0x2F, S)]);
    let dead_chars = HashMap::from([('´', ACUTE), ('¨', DIAER)]);
    Layout { name: "latam", table: t, dead_marks, dead_chars }
}

pub fn get_layout(name: &str) -> Result<&'static Layout> {
    static US: OnceLock<Layout> = OnceLock::new();
    static ES: OnceLock<Layout> = OnceLock::new();
    static GB: OnceLock<Layout> = OnceLock::new();
    static DE: OnceLock<Layout> = OnceLock::new();
    static FR: OnceLock<Layout> = OnceLock::new();
    static IT: OnceLock<Layout> = OnceLock::new();
    static PT: OnceLock<Layout> = OnceLock::new();
    static LA: OnceLock<Layout> = OnceLock::new();
    match canonical(name) {
        Some("us") => Ok(US.get_or_init(|| Layout { name: "us", table: us_table(), dead_marks: HashMap::new(), dead_chars: HashMap::new() })),
        Some("es") => Ok(ES.get_or_init(build_es)),
        Some("gb") => Ok(GB.get_or_init(build_gb)),
        Some("de") => Ok(DE.get_or_init(build_de)),
        Some("fr") => Ok(FR.get_or_init(build_fr)),
        Some("it") => Ok(IT.get_or_init(build_it)),
        Some("pt") => Ok(PT.get_or_init(build_pt)),
        Some("latam") => Ok(LA.get_or_init(build_latam)),
        _ => usage(format!("layout desconocido {}; disponibles: {}", pyrepr(name), LAYOUT_NAMES.join(", "))),
    }
}
