//! Embebe el tema de Ventoy (packaging/ventoy-theme, generado por tools/gen_ventoy_theme.py) en el binario:
//! genera `$OUT_DIR/ventoy_theme.rs` con la lista `(ruta en la SD, bytes)`.

use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())).map(|e| e.unwrap().path()).collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            walk(&p, out);
        } else if !p.file_name().unwrap().to_string_lossy().starts_with('.') {
            out.push(p);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let theme = manifest.join("../../packaging/ventoy-theme").canonicalize().expect("packaging/ventoy-theme");
    println!("cargo:rerun-if-changed={}", theme.display());
    let mut files = Vec::new();
    walk(&theme, &mut files);
    let mut src = String::from("/// Ficheros del tema RiotKVM: (ruta dentro de /ventoy/theme/, contenido).\npub static FILES: &[(&str, &[u8])] = &[\n");
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        let rel = f.strip_prefix(&theme).unwrap().to_string_lossy().replace('\\', "/");
        src += &format!("    ({rel:?}, include_bytes!({:?})),\n", f.display().to_string());
    }
    src += "];\n";
    std::fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ventoy_theme.rs"), src).unwrap();
}
