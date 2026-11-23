//! HTTP mínimo (ureq + rustls): JSON/texto pequeño y descargas con avance, límite de tamaño y cancelación.

use crate::{Cancel, Error, Progress, Reporter, Result};
use std::io::Read;
use std::time::Duration;

const UA: &str = concat!("qkvm-client/", env!("CARGO_PKG_VERSION"));

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .user_agent(UA)
        .build()
        .into()
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    agent()
        .get(url)
        .header("Accept", "application/vnd.github+json, application/octet-stream, */*")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => Error::msg(format!("no existe: {url} (404)")),
            ureq::Error::StatusCode(403) => Error::msg(format!("GitHub rechazó la petición (403, ¿límite de peticiones sin token?): {url}")),
            e => Error::msg(format!("no se pudo descargar {url}: {e}")),
        })
}

/// Texto pequeño (JSON de la API, manifest, sumas). Máximo 2 MB.
pub fn get_text(url: &str) -> Result<String> {
    let mut r = get(url)?;
    r.body_mut().with_config().limit(2 << 20).read_to_string().map_err(|e| Error::msg(format!("respuesta ilegible de {url}: {e}")))
}

/// Descarga binaria con avance. Falla si pasa de `max` bytes. Comprueba la cancelación entre bloques.
pub fn get_bytes(url: &str, max: u64, label: &str, rep: &dyn Reporter, cancel: &Cancel) -> Result<Vec<u8>> {
    let mut r = get(url)?;
    let total = r.body().content_length().unwrap_or(0);
    if total > max {
        return Err(Error::msg(format!("{url}: {total} bytes, más de lo esperado ({max})")));
    }
    let mut rd = r.body_mut().as_reader();
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let n = rd.read(&mut buf).map_err(|e| Error::msg(format!("descarga interrumpida ({url}): {e}")))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() as u64 > max {
            return Err(Error::msg(format!("{url}: más de {max} bytes, se aborta")));
        }
        rep.progress(Progress { stage: label.to_string(), done: out.len() as u64, total });
    }
    Ok(out)
}
