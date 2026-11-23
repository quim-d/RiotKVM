//! Firmware publicado en la última release de GitHub.
//!
//! El CI (`.github/workflows/release.yml`) publica en cada release `qkvm-fw-X.Y.Z.factory.bin`, `SHA256SUMS.txt` y
//! `manifest.json` (`{schema:1, tag, client:{version}, firmware:{version, proto, asset, sha256, size, url}}`).
//! Orden de preferencia: manifest → asset `qkvm-fw-*.factory.bin` + `SHA256SUMS.txt` → mismo asset + `digest` de la
//! API de GitHub. Releases antiguas (≤ v0.2.1) no traen firmware: se informa sin error.

use crate::{http, Cancel, Error, Reporter, Result};
use dkvm_core::FwVersion;
use serde::Deserialize;
use std::path::PathBuf;

pub const REPO: &str = "quim-d/quim-dkvm";
pub const LATEST_API: &str = "https://api.github.com/repos/quim-d/quim-dkvm/releases/latest";
/// Tope de tamaño del factory.bin (la flash del T-Dongle-S3 es de 16 MB).
pub const MAX_FW_BYTES: u64 = 16 << 20;
const ASSET_PREFIX: &str = "qkvm-fw-";
const ASSET_SUFFIX: &str = ".factory.bin";

#[derive(Debug, Clone, Deserialize)]
pub struct GhRelease {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GhAsset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    pub browser_download_url: String,
    /// `sha256:<hex>` (GitHub lo rellena en assets recientes).
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub tag: String,
    pub firmware: ManifestFw,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFw {
    pub version: String,
    #[serde(default)]
    pub proto: Option<u32>,
    pub asset: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

/// De dónde sale la suma que se exige al descargar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaSource {
    Manifest,
    Sha256Sums,
    GithubDigest,
}

/// Firmware disponible y verificable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareOffer {
    pub tag: String,
    pub version: FwVersion,
    pub proto: Option<u32>,
    pub asset: String,
    pub url: String,
    pub size: Option<u64>,
    pub sha256: String,
    pub sha_source: ShaSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Offer(FirmwareOffer),
    /// La release existe pero no trae firmware (o no se puede verificar): motivo legible.
    None { tag: String, reason: String },
}

/// `SHA256SUMS.txt` (formato de `sha256sum`: `<hex>  <nombre>` o `<hex> *<nombre>`) -> pares (nombre, hex).
pub fn parse_sha256sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (h, rest) = l.split_once(char::is_whitespace)?;
            let name = rest.trim_start().trim_start_matches('*').trim();
            (h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit()) && !name.is_empty()).then(|| (name.to_string(), h.to_ascii_lowercase()))
        })
        .collect()
}

/// `qkvm-fw-1.2.3.factory.bin` -> 1.2.3.
pub fn version_from_asset_name(name: &str) -> Option<FwVersion> {
    name.strip_prefix(ASSET_PREFIX)?.strip_suffix(ASSET_SUFFIX)?.parse().ok()
}

/// El asset de firmware de la release (si hubiera varios, el de versión más alta).
pub fn select_firmware_asset(assets: &[GhAsset]) -> Option<(&GhAsset, FwVersion)> {
    assets.iter().filter_map(|a| version_from_asset_name(&a.name).map(|v| (a, v))).max_by_key(|(_, v)| *v)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Decide qué firmware ofrece una release. `fetch` baja textos (manifest/sumas): inyectable en pruebas.
pub fn resolve(rel: &GhRelease, fetch: &dyn Fn(&str) -> Result<String>) -> Result<Availability> {
    let tag = rel.tag_name.clone();
    let none = |reason: String| Ok(Availability::None { tag: tag.clone(), reason });
    if let Some(m) = rel.assets.iter().find(|a| a.name == "manifest.json") {
        let txt = fetch(&m.browser_download_url)?;
        let man: Manifest = serde_json::from_str(&txt).map_err(|e| Error::msg(format!("manifest.json de {tag} ilegible: {e}")))?;
        if man.schema != 1 {
            return none(format!("manifest.json con schema {} (este cliente entiende el 1): actualiza el cliente", man.schema));
        }
        let fw = &man.firmware;
        let version: FwVersion = fw.version.parse().map_err(|e| Error::msg(format!("manifest.json: {e}")))?;
        if !is_hex64(&fw.sha256) {
            return Err(Error::msg("manifest.json: sha256 del firmware no válido"));
        }
        if version_from_asset_name(&fw.asset) != Some(version) {
            return Err(Error::msg(format!("manifest.json: el asset {} no corresponde a la versión {version}", fw.asset)));
        }
        // la URL buena es la del asset publicado; la del manifest solo si el asset no aparece en la lista
        let listed = rel.assets.iter().find(|a| a.name == fw.asset);
        let url = match (listed, &fw.url) {
            (Some(a), _) => a.browser_download_url.clone(),
            (None, Some(u)) if u.starts_with("https://github.com/") => u.clone(),
            _ => return none(format!("manifest.json cita {} pero la release no lo trae", fw.asset)),
        };
        let size = fw.size.or(listed.map(|a| a.size));
        return Ok(Availability::Offer(FirmwareOffer {
            tag: tag.clone(),
            version,
            proto: fw.proto,
            asset: fw.asset.clone(),
            url,
            size,
            sha256: fw.sha256.to_ascii_lowercase(),
            sha_source: ShaSource::Manifest,
        }));
    }
    let Some((asset, version)) = select_firmware_asset(&rel.assets) else {
        return none(format!("la release {tag} no trae firmware (ni manifest.json ni qkvm-fw-*.factory.bin)"));
    };
    let offer = |sha256: String, src| {
        Ok(Availability::Offer(FirmwareOffer {
            tag: tag.clone(),
            version,
            proto: None,
            asset: asset.name.clone(),
            url: asset.browser_download_url.clone(),
            size: Some(asset.size).filter(|s| *s > 0),
            sha256,
            sha_source: src,
        }))
    };
    if let Some(s) = rel.assets.iter().find(|a| a.name == "SHA256SUMS.txt") {
        let sums = parse_sha256sums(&fetch(&s.browser_download_url)?);
        if let Some((_, h)) = sums.iter().find(|(n, _)| *n == asset.name) {
            return offer(h.clone(), ShaSource::Sha256Sums);
        }
    }
    if let Some(h) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).filter(|h| is_hex64(h)) {
        return offer(h.to_ascii_lowercase(), ShaSource::GithubDigest);
    }
    none(format!("{} no tiene suma SHA-256 publicada: no se puede verificar", asset.name))
}

/// Consulta la última release (sin token: el repo es público).
pub fn check_latest() -> Result<Availability> {
    let txt = http::get_text(LATEST_API)?;
    let rel: GhRelease = serde_json::from_str(&txt).map_err(|e| Error::msg(format!("respuesta de GitHub ilegible: {e}")))?;
    resolve(&rel, &|u| http::get_text(u))
}

/// Comprueba tamaño y SHA-256 de un firmware descargado.
pub fn verify(offer: &FirmwareOffer, data: &[u8]) -> Result<()> {
    if let Some(s) = offer.size {
        if s != data.len() as u64 {
            return Err(Error::msg(format!("{}: {} bytes, se esperaban {s}", offer.asset, data.len())));
        }
    }
    let got = crate::sha256_hex(data);
    if got != offer.sha256 {
        return Err(Error::msg(format!("{}: SHA-256 no coincide (esperado {}, obtenido {got})", offer.asset, offer.sha256)));
    }
    Ok(())
}

fn cache_path(offer: &FirmwareOffer) -> Result<PathBuf> {
    Ok(crate::state_dir()?.join("firmware").join(&offer.asset))
}

/// Descarga (o toma de la caché) el firmware y lo verifica. Nunca devuelve bytes sin verificar.
pub fn fetch_firmware(offer: &FirmwareOffer, rep: &dyn Reporter, cancel: &Cancel) -> Result<Vec<u8>> {
    let path = cache_path(offer)?;
    if let Ok(d) = std::fs::read(&path) {
        if verify(offer, &d).is_ok() {
            rep.log(&format!("firmware {} desde la caché ({})", offer.version, path.display()));
            return Ok(d);
        }
    }
    let d = http::get_bytes(&offer.url, MAX_FW_BYTES, &format!("descargando {}", offer.asset), rep, cancel)?;
    verify(offer, &d)?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, &d);
    }
    rep.log(&format!("descargado {} ({} bytes, SHA-256 verificado por {:?})", offer.asset, d.len(), offer.sha_source));
    Ok(d)
}

/// Relación entre la versión instalada y la disponible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No se conoce la instalada (sin conexión o firmware anterior al versionado).
    Unknown,
    Upgrade,
    Same,
    Downgrade,
}

pub fn compare(current: Option<FwVersion>, available: FwVersion) -> Verdict {
    match current {
        None => Verdict::Unknown,
        Some(c) if available > c => Verdict::Upgrade,
        Some(c) if available == c => Verdict::Same,
        Some(_) => Verdict::Downgrade,
    }
}

/// Bajar de versión solo con permiso explícito; reinstalar la misma o actualizar, siempre.
pub fn may_flash(v: Verdict, allow_downgrade: bool) -> Result<()> {
    if v == Verdict::Downgrade && !allow_downgrade {
        return Err(Error::msg("la versión elegida es más antigua que la instalada: marca «permitir bajar de versión» (--allow-downgrade) si es lo que quieres"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn asset(name: &str, digest: Option<&str>) -> GhAsset {
        GhAsset { name: name.into(), size: 1234, browser_download_url: format!("https://github.com/{REPO}/releases/download/v1/{name}"), digest: digest.map(String::from) }
    }

    fn fetcher(map: HashMap<String, String>) -> impl Fn(&str) -> Result<String> {
        move |u: &str| map.iter().find(|(k, _)| u.ends_with(k.as_str())).map(|(_, v)| v.clone()).ok_or_else(|| Error::msg(format!("404 {u}")))
    }

    const H1: &str = "d86ff9de63d94c8b8f1f6b14d23c55af4b88e8207bf2893890892a6a97b1ad54";
    const H2: &str = "082c478e8c432e1ac653899fcf50e3a4077a2851bc6112b5ddd30545962164a9";

    #[test]
    fn sums_parse_both_formats() {
        let t = format!("{H1}  qkvm-fw-1.1.0.factory.bin\n{H2} *manifest.json\nbasura\n\n{}  corto\n", &H1[..10]);
        let s = parse_sha256sums(&t);
        assert_eq!(s, vec![("qkvm-fw-1.1.0.factory.bin".into(), H1.into()), ("manifest.json".into(), H2.into())]);
    }

    #[test]
    fn asset_names() {
        assert_eq!(version_from_asset_name("qkvm-fw-1.1.0.factory.bin"), Some(FwVersion::new(1, 1, 0)));
        assert_eq!(version_from_asset_name("qkvm-fw-1.1.factory.bin"), Some(FwVersion::new(1, 1, 0)));
        assert_eq!(version_from_asset_name("qkvm-fw-x.factory.bin"), None);
        assert_eq!(version_from_asset_name("quim-dkvm-0.2.1-linux-x86_64.tar.gz"), None);
        let a = [asset("qkvm-fw-1.1.0.factory.bin", None), asset("qkvm-fw-1.10.0.factory.bin", None), asset("qkvm-fw-1.9.0.factory.bin", None), asset("SHA256SUMS.txt", None)];
        assert_eq!(select_firmware_asset(&a).unwrap().1, FwVersion::new(1, 10, 0));
        assert!(select_firmware_asset(&a[3..]).is_none());
    }

    #[test]
    fn old_release_without_firmware() {
        // como v0.2.1: paquetes del cliente y SHA256SUMS, sin firmware ni manifest
        let rel = GhRelease { tag_name: "v0.2.1".into(), html_url: String::new(), assets: vec![asset("quim-dkvm-0.2.1-macos-arm64.dmg", Some(&format!("sha256:{H1}"))), asset("SHA256SUMS.txt", None)] };
        match resolve(&rel, &fetcher(HashMap::new())).unwrap() {
            Availability::None { tag, reason } => {
                assert_eq!(tag, "v0.2.1");
                assert!(reason.contains("no trae firmware"), "{reason}");
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn manifest_wins() {
        let man = format!(
            r#"{{"schema":1,"tag":"v0.3.0","client":{{"version":"0.3.0"}},"firmware":{{"version":"1.1.0","proto":1,"asset":"qkvm-fw-1.1.0.factory.bin","sha256":"{}","size":1234,"url":"https://github.com/x/y"}}}}"#,
            H1.to_uppercase()
        );
        let rel = GhRelease { tag_name: "v0.3.0".into(), html_url: String::new(), assets: vec![asset("qkvm-fw-1.1.0.factory.bin", None), asset("manifest.json", None), asset("SHA256SUMS.txt", None)] };
        let f = fetcher(HashMap::from([("manifest.json".to_string(), man)]));
        let Availability::Offer(o) = resolve(&rel, &f).unwrap() else { panic!() };
        assert_eq!(o.version, FwVersion::new(1, 1, 0));
        assert_eq!(o.proto, Some(1));
        assert_eq!(o.sha256, H1);
        assert_eq!(o.sha_source, ShaSource::Manifest);
        assert!(o.url.ends_with("/qkvm-fw-1.1.0.factory.bin"), "usa la URL del asset listado: {}", o.url);
    }

    #[test]
    fn manifest_inconsistent_or_newer_schema() {
        let bad = format!(r#"{{"schema":1,"tag":"v","firmware":{{"version":"1.2.0","asset":"qkvm-fw-1.1.0.factory.bin","sha256":"{H1}"}}}}"#);
        let rel = GhRelease { tag_name: "v".into(), html_url: String::new(), assets: vec![asset("qkvm-fw-1.1.0.factory.bin", None), asset("manifest.json", None)] };
        assert!(resolve(&rel, &fetcher(HashMap::from([("manifest.json".to_string(), bad)]))).is_err());
        let v2 = format!(r#"{{"schema":2,"tag":"v","firmware":{{"version":"1.1.0","asset":"qkvm-fw-1.1.0.factory.bin","sha256":"{H1}"}}}}"#);
        assert!(matches!(resolve(&rel, &fetcher(HashMap::from([("manifest.json".to_string(), v2)]))).unwrap(), Availability::None { .. }));
        let short = r#"{"schema":1,"tag":"v","firmware":{"version":"1.1.0","asset":"qkvm-fw-1.1.0.factory.bin","sha256":"abc"}}"#.to_string();
        assert!(resolve(&rel, &fetcher(HashMap::from([("manifest.json".to_string(), short)]))).is_err());
    }

    #[test]
    fn fallback_sums_then_digest() {
        let rel = GhRelease { tag_name: "v0.3.0".into(), html_url: String::new(), assets: vec![asset("qkvm-fw-1.1.0.factory.bin", Some(&format!("sha256:{H2}"))), asset("SHA256SUMS.txt", None)] };
        let sums = format!("{H1}  qkvm-fw-1.1.0.factory.bin\n");
        let Availability::Offer(o) = resolve(&rel, &fetcher(HashMap::from([("SHA256SUMS.txt".to_string(), sums)]))).unwrap() else { panic!() };
        assert_eq!((o.sha256.as_str(), o.sha_source), (H1, ShaSource::Sha256Sums));
        // sumas sin la línea del firmware: digest de GitHub
        let Availability::Offer(o) = resolve(&rel, &fetcher(HashMap::from([("SHA256SUMS.txt".to_string(), String::new())]))).unwrap() else { panic!() };
        assert_eq!((o.sha256.as_str(), o.sha_source), (H2, ShaSource::GithubDigest));
        // sin nada: no se ofrece
        let rel = GhRelease { tag_name: "v".into(), html_url: String::new(), assets: vec![asset("qkvm-fw-1.1.0.factory.bin", None)] };
        assert!(matches!(resolve(&rel, &fetcher(HashMap::new())).unwrap(), Availability::None { .. }));
    }

    #[test]
    fn verify_size_and_hash() {
        let data = b"hola".to_vec();
        let mut o = FirmwareOffer { tag: "v".into(), version: FwVersion::new(1, 1, 0), proto: None, asset: "a".into(), url: String::new(), size: Some(4), sha256: crate::sha256_hex(&data), sha_source: ShaSource::Manifest };
        assert!(verify(&o, &data).is_ok());
        assert!(verify(&o, b"hola!").is_err());
        o.size = None;
        assert!(verify(&o, b"holA").is_err());
    }

    #[test]
    fn verdicts() {
        let v = |s: &str| s.parse::<FwVersion>().unwrap();
        assert_eq!(compare(None, v("1.1.0")), Verdict::Unknown);
        assert_eq!(compare(Some(v("1.0.0")), v("1.1.0")), Verdict::Upgrade);
        assert_eq!(compare(Some(v("1.1.0")), v("1.1.0")), Verdict::Same);
        assert_eq!(compare(Some(v("1.2.0")), v("1.1.9")), Verdict::Downgrade);
        assert!(may_flash(Verdict::Downgrade, false).is_err());
        assert!(may_flash(Verdict::Downgrade, true).is_ok());
        for x in [Verdict::Unknown, Verdict::Upgrade, Verdict::Same] {
            assert!(may_flash(x, false).is_ok());
        }
    }
}
