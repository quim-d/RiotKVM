//! Tablas por distribución del teclado del servidor: caracteres críticos (símbolos de programación, ñ, ç, ¿, ¡, acentos y
//! teclas muertas). Las posiciones salen del conocimiento de cada layout (no de un teclado real conectado): ver README.

use riotkvm_core::keys::{MOD_LSHIFT as S, MOD_RALT as A};
use riotkvm_core::layouts::{canonical, get_layout, LAYOUT_NAMES};

fn st(layout: &str, ch: char) -> Vec<(u16, u8)> {
    get_layout(layout).unwrap().strokes(ch).unwrap_or_else(|e| panic!("{layout} {ch:?}: {}", e.message()))
}

fn one(layout: &str, ch: char, usage: u16, mods: u8) {
    assert_eq!(st(layout, ch), vec![(usage, mods)], "{layout} {ch:?}");
}

#[test]
fn all_names_and_aliases_resolve() {
    for n in LAYOUT_NAMES {
        assert!(get_layout(n).is_ok(), "{n}");
        assert_eq!(canonical(n), Some(n));
    }
    for (a, c) in [("en-GB", "gb"), ("UK", "gb"), ("pt-PT", "pt"), ("es-419", "latam"), ("LA", "latam"), ("en-us", "us"), ("de_DE", "de")] {
        assert_eq!(canonical(a), Some(c), "{a}");
    }
    assert!(get_layout("klingon").is_err());
}

#[test]
fn es_critical_chars() {
    for (c, u, m) in [('@', 0x1F, A), ('#', 0x20, A), ('\\', 0x35, A), ('|', 0x1E, A), ('{', 0x34, A), ('}', 0x31, A), ('[', 0x2F, A), (']', 0x30, A), ('~', 0x21, A), ('ñ', 0x33, 0), ('Ñ', 0x33, S), ('ç', 0x31, 0), ('¿', 0x2E, S), ('¡', 0x2E, 0)] {
        one("es", c, u, m);
    }
    assert_eq!(st("es", 'á'), vec![(0x34, 0), (0x04, 0)]); // tecla muerta ´ + a
    assert_eq!(st("es", 'Ü'), vec![(0x34, S), (0x18, S)]);
}

#[test]
fn us_critical_chars() {
    for (c, u, m) in [('@', 0x1F, S), ('#', 0x20, S), ('\\', 0x31, 0), ('|', 0x31, S), ('{', 0x2F, S), ('}', 0x30, S), ('[', 0x2F, 0), (']', 0x30, 0), ('~', 0x35, S), ('`', 0x35, 0)] {
        one("us", c, u, m);
    }
    assert!(get_layout("us").unwrap().strokes('ñ').is_err());
}

#[test]
fn gb_critical_chars() {
    for (c, u, m) in [('@', 0x34, S), ('"', 0x1F, S), ('#', 0x31, 0), ('~', 0x31, S), ('\\', 0x64, 0), ('|', 0x64, S), ('£', 0x20, S), ('€', 0x21, A), ('é', 0x08, A), ('¬', 0x35, S)] {
        one("gb", c, u, m);
    }
}

#[test]
fn de_critical_chars() {
    for (c, u, m) in [('@', 0x14, A), ('\\', 0x2D, A), ('|', 0x64, A), ('{', 0x24, A), ('}', 0x27, A), ('[', 0x25, A), (']', 0x26, A), ('~', 0x30, A), ('ß', 0x2D, 0), ('ü', 0x2F, 0), ('Ö', 0x33, S), ('ä', 0x34, 0), ('€', 0x08, A), ('z', 0x1C, 0), ('y', 0x1D, 0), ('Z', 0x1C, S), ('#', 0x31, 0), ('"', 0x1F, S), ('/', 0x24, S), ('=', 0x27, S)] {
        one("de", c, u, m);
    }
    assert_eq!(st("de", '^'), vec![(0x35, 0), (0x2C, 0)]); // tecla muerta ^ + espacio
    assert_eq!(st("de", 'é'), vec![(0x2E, 0), (0x08, 0)]); // acento muerto + e
}

#[test]
fn fr_critical_chars() {
    for (c, u, m) in [('a', 0x14, 0), ('q', 0x04, 0), ('w', 0x1D, 0), ('z', 0x1A, 0), ('m', 0x33, 0), (',', 0x10, 0), (';', 0x36, 0), (':', 0x37, 0), ('!', 0x38, 0), ('1', 0x1E, S), ('0', 0x27, S), ('@', 0x27, A), ('#', 0x20, A), ('~', 0x1F, A), ('{', 0x21, A), ('}', 0x2E, A), ('[', 0x22, A), (']', 0x2D, A), ('|', 0x23, A), ('\\', 0x25, A), ('^', 0x26, A), ('é', 0x1F, 0), ('è', 0x24, 0), ('ç', 0x26, 0), ('à', 0x27, 0), ('ù', 0x34, 0), ('€', 0x08, A), ('$', 0x30, 0), ('&', 0x1E, 0)] {
        one("fr", c, u, m);
    }
    assert_eq!(st("fr", 'ê'), vec![(0x2F, 0), (0x08, 0)]); // circunflejo muerto + e
    assert_eq!(st("fr", 'ë'), vec![(0x2F, S), (0x08, 0)]);
}

#[test]
fn it_critical_chars() {
    for (c, u, m) in [('@', 0x33, A), ('#', 0x34, A), ('[', 0x2F, A), (']', 0x30, A), ('{', 0x2F, A | S), ('}', 0x30, A | S), ('\\', 0x35, 0), ('|', 0x35, S), ('è', 0x2F, 0), ('é', 0x2F, S), ('ò', 0x33, 0), ('à', 0x34, 0), ('ù', 0x31, 0), ('ì', 0x2E, 0), ('~', 0x2E, A), ('€', 0x21, A), ('£', 0x20, S)] {
        one("it", c, u, m);
    }
}

#[test]
fn pt_critical_chars() {
    for (c, u, m) in [('@', 0x1F, A), ('€', 0x22, A), ('{', 0x24, A), ('}', 0x27, A), ('[', 0x25, A), (']', 0x26, A), ('\\', 0x35, 0), ('|', 0x35, S), ('ç', 0x33, 0), ('Ç', 0x33, S), ('«', 0x2E, 0), ('º', 0x34, 0), ('£', 0x20, A)] {
        one("pt", c, u, m);
    }
    assert_eq!(st("pt", '~'), vec![(0x31, 0), (0x2C, 0)]); // tecla muerta ~ + espacio
    assert_eq!(st("pt", 'ã'), vec![(0x31, 0), (0x04, 0)]);
    assert_eq!(st("pt", 'á'), vec![(0x30, 0), (0x04, 0)]);
}

#[test]
fn latam_critical_chars() {
    for (c, u, m) in [('@', 0x1F, A), ('|', 0x35, 0), ('\\', 0x2D, A), ('{', 0x34, 0), ('}', 0x31, 0), ('[', 0x34, S), (']', 0x31, S), ('~', 0x30, A), ('ñ', 0x33, 0), ('Ñ', 0x33, S), ('¿', 0x2E, 0), ('¡', 0x2E, S), ('"', 0x1F, S), ('#', 0x20, S), ('€', 0x08, A)] {
        one("latam", c, u, m);
    }
    assert_eq!(st("latam", 'á'), vec![(0x2F, 0), (0x04, 0)]);
}

#[test]
fn unsupported_chars_are_reported_not_skipped_silently() {
    let l = get_layout("us").unwrap();
    let (strokes, bad) = l.text_strokes_lossy("añb漢c\r\nd");
    assert_eq!(bad, vec!['ñ', '漢']);
    // a, b, c, Enter, d (CRLF = un solo Enter)
    assert_eq!(strokes.len(), 5);
    // latam no puede escribir ^ ni ` (no se ha modelado su tecla muerta): se avisa
    let (_, bad) = get_layout("latam").unwrap().text_strokes_lossy("a^b`c");
    assert_eq!(bad, vec!['^', '`']);
    // fr: mayúsculas acentuadas necesitan Bloq Mayús, no soportado
    let (_, bad) = get_layout("fr").unwrap().text_strokes_lossy("É");
    assert_eq!(bad, vec!['É']);
}

#[test]
fn every_ascii_printable_is_typeable_in_the_main_layouts() {
    // en us/es/gb/de/it/pt/latam/fr todo el ASCII imprimible debe poder escribirse (latam: sin ^ ni `)
    for n in LAYOUT_NAMES {
        let l = get_layout(n).unwrap();
        let (_, bad) = l.text_strokes_lossy(&(32u8..127).map(|b| b as char).collect::<String>());
        let allowed: &[char] = if n == "latam" { &['^', '`'] } else { &[] };
        let unexpected: Vec<char> = bad.into_iter().filter(|c| !allowed.contains(c)).collect();
        assert!(unexpected.is_empty(), "{n}: no representables {unexpected:?}");
    }
}

#[test]
fn text_strokes_converts_crlf_and_tabs() {
    let l = get_layout("es").unwrap();
    assert_eq!(l.text_strokes("a\r\nb\tc").unwrap().len(), 5);
}
