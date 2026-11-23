//! Tema QKVM de Ventoy (GRUB2) y `ventoy.json`.
//!
//! El tema vive en `packaging/ventoy-theme/` (lo genera `tools/gen_ventoy_theme.py`) y va embebido en el
//! binario (`build.rs`, ~200 kB). Al preparar la SD se crea dentro del exFAT:
//!
//! ```text
//! /ventoy/ventoy.json
//! /ventoy/theme/qkvm/theme_<WxH>_<n>.txt + bg_<WxH>_<n>.png, pixmaps, icons/
//!     (Ventoy se queda con los de la resolución activa —resolution_fit— y entre ellos elige uno al azar en cada
//!     arranque: así rota la cita del pie)
//! /ventoy/theme/fonts/jbmono-*.pf2, OFL.txt  (JetBrains Mono, SIL OFL 1.1)
//! /Tools/                                     (vacía: herramientas de rescate)
//! ```
//!
//! Las ISO las pone cada uno a mano: instaladores en la raíz y herramientas en `/Tools`. Menú en árbol para que
//! `/Tools` salga como carpeta. Sin `VTOY_DEFAULT_SEARCH_ROOT` (escondería las de la raíz). El tema es el mismo
//! en BIOS legacy y UEFI.

use serde_json::{json, Value};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/ventoy_theme.rs"));
}

/// Resoluciones para las que hay maqueta (la resolución va en el nombre del theme_*.txt: así la encuentra
/// `resolution_fit`).
pub const RESOLUTIONS: [&str; 2] = ["1920x1080", "1024x768"];
/// Primero 1920x1080 (lo que mejor captura el dongle); si la tarjeta no lo da (VBE de servidores viejos),
/// 1024x768; y si tampoco, lo que haya (sin coincidencia de resolución, Ventoy usa cualquiera de los temas).
pub const GFXMODE: &str = "1920x1080,1024x768,auto";
/// Carpeta para las herramientas de rescate.
pub const TOOLS_DIR: &str = "Tools";

/// Ficheros del tema con su ruta en la partición Ventoy (sin barra inicial).
pub fn theme_files() -> Vec<(String, &'static [u8])> {
    embedded::FILES.iter().filter(|(p, _)| !p.ends_with(".md")).map(|(p, d)| (format!("ventoy/theme/{p}"), *d)).collect()
}

pub fn theme_bytes() -> u64 {
    theme_files().iter().map(|(_, d)| d.len() as u64).sum()
}

/// Rutas (con barra inicial) de todos los theme_*.txt, 1920x1080 primero.
pub fn theme_txts() -> Vec<String> {
    let mut v: Vec<String> = theme_files().into_iter().map(|(p, _)| p).filter(|p| p.starts_with("ventoy/theme/qkvm/theme_") && p.ends_with(".txt")).map(|p| format!("/{p}")).collect();
    v.sort_by_key(|p| (RESOLUTIONS.iter().position(|r| p.contains(r)), p.clone()));
    v
}

fn fonts() -> Vec<String> {
    theme_files().into_iter().map(|(p, _)| p).filter(|p| p.ends_with(".pf2")).map(|p| format!("/{p}")).collect()
}

/// Sección `theme` de ventoy.json. La posición y el color de la etiqueta de versión de Ventoy van en la primera
/// línea de cada theme_*.txt (`ventoy_left_top_color`), distintas por resolución.
pub fn theme_section() -> Value {
    json!({
        "file": theme_txts(),
        "resolution_fit": 1,
        "gfxmode": GFXMODE,
        "display_mode": "GUI",
        "fonts": fonts(),
    })
}

/// ventoy.json: castellano, menú en árbol, fuera los `._*` que deja macOS en exFAT, icono de rescate para todo lo
/// que haya en `/Tools` y alias de esa carpeta. Con `with_theme`, el tema QKVM.
pub fn ventoy_json(with_theme: bool) -> Value {
    let mut v = json!({
        "control": [
            {"VTOY_MENU_LANGUAGE": "es_ES"},
            {"VTOY_DEFAULT_MENU_MODE": "1"},
            {"VTOY_FILT_DOT_UNDERSCORE_FILE": "1"}
        ]
    });
    if with_theme {
        v["theme"] = theme_section();
    }
    v["menu_class"] = json!([{"parent": format!("/{TOOLS_DIR}"), "class": "qkvm-rescate"}]);
    v["menu_alias"] = json!([{"dir": format!("/{TOOLS_DIR}"), "alias": "Tools — rescate y diagnóstico"}]);
    v
}

fn pretty(v: &Value) -> Vec<u8> {
    let mut s = serde_json::to_vec_pretty(v).unwrap_or_default();
    s.push(b'\n');
    s
}

/// Lo que se crea en el exFAT al preparar la SD: la carpeta `Tools/` (ruta acabada en `/` = directorio vacío),
/// ventoy.json y, con `with_theme`, el tema.
pub fn sd_files(with_theme: bool) -> Vec<(String, Vec<u8>)> {
    let mut v = vec![(format!("{TOOLS_DIR}/"), Vec::new()), ("ventoy/ventoy.json".to_string(), pretty(&ventoy_json(with_theme)))];
    if with_theme {
        v.extend(theme_files().into_iter().map(|(p, d)| (p, d.to_vec())));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_theme_is_complete_and_small() {
        let f = theme_files();
        let has = |p: &str| f.iter().any(|(x, _)| x == p);
        let txts = theme_txts();
        for r in RESOLUTIONS {
            assert!(txts.iter().filter(|t| t.contains(r)).count() >= 2, "{r}: varias para que rote el pie");
        }
        for t in &txts {
            let body = String::from_utf8(f.iter().find(|(p, _)| format!("/{p}") == *t).unwrap().1.to_vec()).unwrap();
            assert!(body.starts_with("ventoy_left_top_color: \"@"), "{t}: la versión de Ventoy se coloca en la 1ª línea");
            let bg = body.lines().find_map(|l| l.strip_prefix("desktop-image: \"")).unwrap().trim_end_matches('"');
            assert!(has(&format!("ventoy/theme/qkvm/{bg}")), "{t}: {bg}");
            // cada fuente que nombra el tema está embebida
            for name in body.lines().filter_map(|l| l.split("font = \"").nth(1).or_else(|| l.split("font: \"").nth(1))).map(|r| r.split('"').next().unwrap()) {
                let size = name.rsplit(' ').next().unwrap();
                let style = if name.contains("Bold") { "bold" } else { "regular" };
                assert!(has(&format!("ventoy/theme/fonts/jbmono-{style}-{size}.pf2")), "{name}");
            }
        }
        for n in ["menu_c.png", "select_c.png", "terminal_box_c.png", "thumb_c.png", "icons/qkvm-rescate.png"] {
            assert!(has(&format!("ventoy/theme/qkvm/{n}")), "{n}");
        }
        assert!(has("ventoy/theme/fonts/OFL.txt"), "licencia de la fuente");
        assert!(theme_bytes() < 1 << 20, "el tema embebido debe ser pequeño: {}", theme_bytes());
    }

    #[test]
    fn json_tree_mode_tools_and_theme() {
        let v = ventoy_json(true);
        assert_eq!(v["theme"]["resolution_fit"], 1);
        assert_eq!(v["theme"]["file"][0], "/ventoy/theme/qkvm/theme_1920x1080_1.txt");
        assert!(v["theme"]["gfxmode"].as_str().unwrap().starts_with("1920x1080"));
        assert_eq!(v["theme"]["fonts"].as_array().unwrap().len(), 4);
        assert!(v["control"].as_array().unwrap().iter().any(|c| c["VTOY_DEFAULT_MENU_MODE"] == "1"));
        assert!(!v["control"].as_array().unwrap().iter().any(|c| c.get("VTOY_DEFAULT_SEARCH_ROOT").is_some()));
        assert_eq!(v["menu_alias"], json!([{"dir": "/Tools", "alias": "Tools — rescate y diagnóstico"}]));
        assert!(ventoy_json(false).get("theme").is_none());
        let files = sd_files(true);
        assert_eq!(files[0], ("Tools/".to_string(), vec![]));
        assert!(serde_json::from_slice::<Value>(&files[1].1).is_ok());
        assert_eq!(sd_files(false).len(), 2);
    }
}
