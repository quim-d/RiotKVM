//! Paquete oficial de Ventoy para Linux (versión y SHA-256 fijados): descarga, caché y extracción de los tres
//! ficheros que hacen falta (`boot/boot.img`, `boot/core.img.xz`, `ventoy/ventoy.disk.img.xz`).

use crate::{http, Cancel, Error, Reporter, Result};
use std::io::Read;
use std::path::PathBuf;

pub const VERSION: &str = "1.1.18";
/// SHA-256 de `ventoy-1.1.18-linux.tar.gz` (coincide con `sha256.txt` y el `digest` de la release oficial).
pub const SHA256: &str = "d86ff9de63d94c8b8f1f6b14d23c55af4b88e8207bf2893890892a6a97b1ad54";
pub const SIZE: u64 = 20_320_548;

pub fn tarball_name() -> String {
    format!("ventoy-{VERSION}-linux.tar.gz")
}

pub fn url() -> String {
    format!("https://github.com/ventoy/Ventoy/releases/download/v{VERSION}/{}", tarball_name())
}

pub fn cache_path() -> Result<PathBuf> {
    Ok(crate::state_dir()?.join("ventoy").join(tarball_name()))
}

#[derive(Clone)]
pub struct Package {
    pub boot_img: Vec<u8>,
    pub core_img: Vec<u8>,
    pub disk_img: Vec<u8>,
}

impl std::fmt::Debug for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Package {{ boot {} B, core {} B, disk {} B }}", self.boot_img.len(), self.core_img.len(), self.disk_img.len())
    }
}

fn xz(data: &[u8], what: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(data), &mut out).map_err(|e| Error::msg(format!("{what}: xz no válido: {e:?}")))?;
    Ok(out)
}

impl Package {
    /// Extrae y valida los tres ficheros del tar.gz (ya verificado).
    pub fn from_tarball(tgz: &[u8]) -> Result<Package> {
        let prefix = format!("ventoy-{VERSION}/");
        let (mut boot, mut core, mut disk) = (None, None, None);
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
        for e in ar.entries().map_err(|e| Error::msg(format!("tar: {e}")))? {
            let mut e = e.map_err(|e| Error::msg(format!("tar: {e}")))?;
            let path = e.path().map_err(|e| Error::msg(format!("tar: {e}")))?.to_string_lossy().into_owned();
            // el tar oficial guarda `./ventoy-X/...`
            let slot = match path.trim_start_matches("./").strip_prefix(&prefix) {
                Some("boot/boot.img") => &mut boot,
                Some("boot/core.img.xz") => &mut core,
                Some("ventoy/ventoy.disk.img.xz") => &mut disk,
                _ => continue,
            };
            let mut v = Vec::new();
            e.read_to_end(&mut v).map_err(|e| Error::msg(format!("tar {path}: {e}")))?;
            *slot = Some(v);
        }
        let missing = |n: &str| Error::msg(format!("el paquete de Ventoy no trae {n}"));
        let p = Package {
            boot_img: boot.ok_or_else(|| missing("boot/boot.img"))?,
            core_img: xz(&core.ok_or_else(|| missing("boot/core.img.xz"))?, "core.img.xz")?,
            disk_img: xz(&disk.ok_or_else(|| missing("ventoy/ventoy.disk.img.xz"))?, "ventoy.disk.img.xz")?,
        };
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<()> {
        if self.boot_img.len() != 512 {
            return Err(Error::msg("boot.img no mide 512 bytes"));
        }
        if self.core_img.is_empty() || self.core_img.len() as u64 > super::CORE_MAX_SECTORS * 512 {
            return Err(Error::msg("core.img no cabe en los sectores 1-2047"));
        }
        if self.disk_img.len() as u64 != super::PART2_SECTORS * 512 {
            return Err(Error::msg("ventoy.disk.img no mide 32 MiB"));
        }
        Ok(())
    }

    /// Paquete de mentira con los tamaños reales (pruebas).
    #[cfg(test)]
    pub fn fake() -> Package {
        let pat = |n: usize, k: u8| (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(k)).collect::<Vec<u8>>();
        Package { boot_img: pat(512, 1), core_img: pat(1_048_064, 2), disk_img: pat(32 << 20, 3) }
    }
}

/// Toma el tar.gz de la caché o lo descarga; en ambos casos verifica el SHA-256 fijado antes de usarlo.
pub fn load(rep: &dyn Reporter, cancel: &Cancel) -> Result<Package> {
    let path = cache_path()?;
    if let Ok(d) = std::fs::read(&path) {
        if crate::sha256_hex(&d) == SHA256 {
            rep.log(&format!("Ventoy {VERSION} desde la caché ({})", path.display()));
            return Package::from_tarball(&d);
        }
        rep.log("la copia en caché de Ventoy no cuadra con el SHA-256: se descarga otra vez");
    }
    let d = http::get_bytes(&url(), SIZE + (1 << 20), &format!("descargando Ventoy {VERSION}"), rep, cancel)?;
    let got = crate::sha256_hex(&d);
    if got != SHA256 {
        return Err(Error::msg(format!("{}: SHA-256 no coincide (esperado {SHA256}, obtenido {got})", tarball_name())));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, &d);
    }
    rep.log(&format!("descargado {} (SHA-256 verificado)", tarball_name()));
    Package::from_tarball(&d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (n, d) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(d.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, n, *d).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn missing_files_are_reported() {
        let t = tgz(&[("ventoy-1.1.18/boot/boot.img", &[0u8; 512])]);
        let e = Package::from_tarball(&t).unwrap_err().to_string();
        assert!(e.contains("core.img.xz"), "{e}");
        let t = tgz(&[("./ventoy-1.1.18/boot/boot.img", &[0u8; 512])]);
        assert!(Package::from_tarball(&t).unwrap_err().to_string().contains("core.img.xz"), "con ./ delante también");
        let t = tgz(&[("otra-version/boot/boot.img", &[0u8; 512])]);
        assert!(Package::from_tarball(&t).is_err());
    }

    /// Con el tar.gz real en `DKVM_VENTOY_TGZ` (no se descarga en los tests).
    #[test]
    #[ignore]
    fn real_tarball() {
        let p = std::env::var("DKVM_VENTOY_TGZ").expect("DKVM_VENTOY_TGZ");
        let d = std::fs::read(p).unwrap();
        assert_eq!(crate::sha256_hex(&d), SHA256);
        let pkg = Package::from_tarball(&d).unwrap();
        assert_eq!(pkg.core_img.len(), 1_048_064);
    }
}
