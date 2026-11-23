//! Scancodes de teclado físico (set 1 de Windows / PS/2) -> usos HID (página 0x07).
//!
//! La GUI reenvía la TECLA FÍSICA, no el carácter: el sistema del servidor aplica su propio
//! layout, igual que un teclado USB real. `ext` = prefijo E0 (flag "extended" de Windows).

pub fn hid_from_scancode(sc: u16, ext: bool) -> Option<u8> {
    Some(if !ext {
        match sc {
            0x01 => 0x29,
            0x02..=0x0A => 0x1E + (sc - 0x02) as u8, // 1..9
            0x0B => 0x27,                             // 0
            0x0C => 0x2D,
            0x0D => 0x2E,
            0x0E => 0x2A,
            0x0F => 0x2B,
            0x10 => 0x14, // q
            0x11 => 0x1A, // w
            0x12 => 0x08, // e
            0x13 => 0x15, // r
            0x14 => 0x17, // t
            0x15 => 0x1C, // y
            0x16 => 0x18, // u
            0x17 => 0x0C, // i
            0x18 => 0x12, // o
            0x19 => 0x13, // p
            0x1A => 0x2F,
            0x1B => 0x30,
            0x1C => 0x28,
            0x1D => 0xE0,
            0x1E => 0x04, // a
            0x1F => 0x16, // s
            0x20 => 0x07, // d
            0x21 => 0x09, // f
            0x22 => 0x0A, // g
            0x23 => 0x0B, // h
            0x24 => 0x0D, // j
            0x25 => 0x0E, // k
            0x26 => 0x0F, // l
            0x27 => 0x33,
            0x28 => 0x34,
            0x29 => 0x35,
            0x2A => 0xE1,
            0x2B => 0x31,
            0x2C => 0x1D, // z
            0x2D => 0x1B, // x
            0x2E => 0x06, // c
            0x2F => 0x19, // v
            0x30 => 0x05, // b
            0x31 => 0x11, // n
            0x32 => 0x10, // m
            0x33 => 0x36,
            0x34 => 0x37,
            0x35 => 0x38,
            0x36 => 0xE5,
            0x37 => 0x55, // kp *
            0x38 => 0xE2,
            0x39 => 0x2C,
            0x3A => 0x39,
            0x3B..=0x44 => 0x3A + (sc - 0x3B) as u8, // F1..F10
            0x45 => 0x53,
            0x46 => 0x47,
            0x47 => 0x5F,
            0x48 => 0x60,
            0x49 => 0x61,
            0x4A => 0x56,
            0x4B => 0x5C,
            0x4C => 0x5D,
            0x4D => 0x5E,
            0x4E => 0x57,
            0x4F => 0x59,
            0x50 => 0x5A,
            0x51 => 0x5B,
            0x52 => 0x62,
            0x53 => 0x63,
            0x56 => 0x64, // <> ISO
            0x57 => 0x44, // F11
            0x58 => 0x45, // F12
            0x7D => 0x89, // yen (JIS)
            _ => return None,
        }
    } else {
        match sc {
            0x1C => 0x58,
            0x1D => 0xE4,
            0x35 => 0x54,
            0x37 => 0x46,
            0x38 => 0xE6,
            0x45 => 0x53,
            0x47 => 0x4A,
            0x48 => 0x52,
            0x49 => 0x4B,
            0x4B => 0x50,
            0x4D => 0x4F,
            0x4F => 0x4D,
            0x50 => 0x51,
            0x51 => 0x4E,
            0x52 => 0x49,
            0x53 => 0x4C,
            0x5B => 0xE3,
            0x5C => 0xE7,
            0x5D => 0x65,
            _ => return None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::lookup;

    #[test]
    fn matches_named_keys() {
        // letras y números contra la tabla de nombres
        let letters = "qwertyuiop";
        for (i, c) in letters.chars().enumerate() {
            assert_eq!(hid_from_scancode(0x10 + i as u16, false), lookup(&c.to_string()).map(|u| u as u8), "{c}");
        }
        for (i, c) in "asdfghjkl".chars().enumerate() {
            assert_eq!(hid_from_scancode(0x1E + i as u16, false), lookup(&c.to_string()).map(|u| u as u8), "{c}");
        }
        for (i, c) in "zxcvbnm".chars().enumerate() {
            assert_eq!(hid_from_scancode(0x2C + i as u16, false), lookup(&c.to_string()).map(|u| u as u8), "{c}");
        }
        for (i, c) in "1234567890".chars().enumerate() {
            assert_eq!(hid_from_scancode(0x02 + i as u16, false), lookup(&c.to_string()).map(|u| u as u8), "{c}");
        }
        for i in 1..=10u16 {
            assert_eq!(hid_from_scancode(0x3A + i, false), lookup(&format!("f{i}")).map(|u| u as u8));
        }
        assert_eq!(hid_from_scancode(0x57, false), lookup("f11").map(|u| u as u8));
        assert_eq!(hid_from_scancode(0x58, false), lookup("f12").map(|u| u as u8));
    }

    #[test]
    fn specials() {
        assert_eq!(hid_from_scancode(0x1D, false), Some(0xE0)); // LCtrl
        assert_eq!(hid_from_scancode(0x1D, true), Some(0xE4)); // RCtrl
        assert_eq!(hid_from_scancode(0x38, true), Some(0xE6)); // RAlt / AltGr
        assert_eq!(hid_from_scancode(0x5B, true), Some(0xE3)); // LWin
        assert_eq!(hid_from_scancode(0x53, true), Some(0x4C)); // Supr
        assert_eq!(hid_from_scancode(0x53, false), Some(0x63)); // kp .
        assert_eq!(hid_from_scancode(0x1C, true), Some(0x58)); // intro del teclado numérico
        assert_eq!(hid_from_scancode(0x56, false), Some(0x64)); // <> ISO (ES)
        assert_eq!(hid_from_scancode(0x21D, false), None); // LCtrl falso de AltGr
        assert_eq!(hid_from_scancode(0x2A, true), None); // LShift falso (Windows)
    }
}
