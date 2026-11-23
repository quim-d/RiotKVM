//! Elección de formato y frecuencia en macOS (lógica pura, testeable en cualquier SO).
//!
//! AVFoundation anuncia frecuencias no enteras (la capturadora MS2109 ofrece 30.00003 y 60.00024 fps) y solo acepta
//! fijar la duración EXACTA de un rango anunciado: pedir 1/30 lanza una NSInvalidArgumentException. Por eso aquí se
//! elige por CERCANÍA entre lo que ofrece el dispositivo y luego se usa la duración del propio rango.

/// Una combinación (formato, rango de fps) que ofrece el dispositivo.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub fmt_idx: usize,
    pub range_idx: usize,
    pub w: u32,
    pub h: u32,
    pub fps: f64,
    /// FourCC de AVFoundation ("420v", "420f", "yuvs", "BGRA", "jpeg"...).
    pub fourcc: String,
}

/// Preferencia de formato de origen: los planares 4:2:0 (NV12) son lo que da la MS2109 a 60 fps; YUYV solo a 5-10 fps.
fn rank(fourcc: &str) -> u32 {
    match fourcc {
        "420v" | "420f" | "BGRA" | "2vuy" | "yuvs" => 0,
        _ => 1,
    }
}

/// Ofertas ordenadas de mejor a peor para `want` = (ancho, alto, fps); la primera es la que se intenta antes, el resto
/// es la cascada si AVFoundation rechaza alguna. Orden: resolución más cercana, fps más cercano (por cercanía, no por
/// igualdad: 30 casa con 30.00003), formato preferido, y por último fps exacto.
pub fn plan(want: (u32, u32, u32), offers: &[Offer]) -> Vec<Offer> {
    let area = |w: u32, h: u32| w as i64 * h as i64;
    let wa = area(want.0, want.1);
    let mut v: Vec<Offer> = offers.iter().filter(|o| o.w > 0 && o.h > 0 && o.fps > 0.0).cloned().collect();
    v.sort_by(|a, b| {
        let key = |o: &Offer| {
            let d = (o.fps - want.2 as f64).abs();
            ((area(o.w, o.h) - wa).abs(), d.round() as u32, rank(&o.fourcc), (d * 1000.0) as u64)
        };
        key(a).cmp(&key(b))
    });
    v
}

/// Modos (ancho, alto, fps redondeado) para la GUI, de mayor a menor; solo >= 5 fps.
pub fn modes(offers: &[Offer]) -> Vec<(u32, u32, u32)> {
    let mut out: Vec<(u32, u32, u32)> = offers.iter().filter(|o| o.fps >= 5.0 && o.w > 0 && o.h > 0).map(|o| (o.w, o.h, o.fps.round() as u32)).collect();
    out.sort_by_key(|m| std::cmp::Reverse((m.0 * m.1, m.2)));
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(f: usize, r: usize, w: u32, h: u32, fps: f64, cc: &str) -> Offer {
        Offer { fmt_idx: f, range_idx: r, w, h, fps, fourcc: cc.into() }
    }

    /// Lo que anuncia la MS2109 en un Mac real (medido por el coordinador).
    fn ms2109() -> Vec<Offer> {
        let mut v = Vec::new();
        let mut i = 0;
        for (w, h) in [(1920, 1080), (1280, 720)] {
            for (r, fps) in [60.00024, 30.00003, 25.0, 20.0, 10.0].into_iter().enumerate() {
                v.push(o(i, r, w, h, fps, "420v"));
            }
            i += 1;
            v.push(o(i, 0, w, h, if h == 1080 { 5.0 } else { 10.0 }, "yuvs"));
            i += 1;
        }
        v
    }

    #[test]
    fn thirty_matches_30_00003_not_the_yuyv() {
        let p = plan((1280, 720, 30), &ms2109());
        assert_eq!((p[0].w, p[0].h), (1280, 720));
        assert!((p[0].fps - 30.00003).abs() < 1e-9 && p[0].fourcc == "420v", "{:?}", p[0]);
    }

    #[test]
    fn sixty_matches_60_00024() {
        let p = plan((1920, 1080, 60), &ms2109());
        assert_eq!((p[0].w, p[0].h), (1920, 1080));
        assert!((p[0].fps - 60.00024).abs() < 1e-9);
    }

    #[test]
    fn unknown_resolution_falls_back_to_closest_and_keeps_a_cascade() {
        let p = plan((1600, 900, 30), &ms2109());
        assert!(p.len() > 3);
        assert!(p[0].w == 1280 || p[0].w == 1920);
    }

    #[test]
    fn modes_are_rounded_and_unique() {
        let m = modes(&ms2109());
        assert!(m.contains(&(1920, 1080, 60)) && m.contains(&(1280, 720, 30)));
        assert_eq!(m.iter().filter(|x| **x == (1280, 720, 10)).count(), 1);
        assert_eq!(m[0], (1920, 1080, 60));
    }
}
