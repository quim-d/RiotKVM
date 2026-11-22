//! Backend nativo con `nokhwa` (Media Foundation en Windows, V4L2 en Linux, AVFoundation en macOS).

#[cfg(not(target_os = "linux"))]
use crate::Backend;
use crate::{DeviceInfo, Frame, Latest, Mode};
use nokhwa::pixel_format::RgbAFormat;
#[cfg(not(target_os = "linux"))]
use nokhwa::utils::ApiBackend;
use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};
use nokhwa::Camera;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod mac;

/// Ejecuta una llamada al backend nativo aislando los pánicos. En macOS, nokhwa convierte las `NSException` de
/// AVFoundation en pánicos de Rust (p. ej. `set_all` rechazando el formato de la capturadora); sin este aislamiento
/// tumban el proceso. Aquí se convierten en un error de texto para mostrarlo en la interfaz.
pub fn guard<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        let m = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "causa desconocida".into());
        format!("{what}: el backend de vídeo del sistema falló ({m})")
    })
}

pub fn list() -> Vec<DeviceInfo> {
    guard("listar cámaras", list_inner).unwrap_or_default()
}

fn list_inner() -> Vec<DeviceInfo> {
    nokhwa::nokhwa_initialize(|_| {});
    // Linux: solo nodos con capacidad de captura (los de metadatos de un mismo dispositivo UVC no sirven)
    #[cfg(target_os = "linux")]
    {
        return crate::pick::devices_from_nodes(&crate::linux::nodes());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Ok(cams) = nokhwa::query(ApiBackend::Auto) else { return vec![] };
        cams.iter()
            .enumerate()
            .map(|(i, c)| {
                let name = c.human_name();
                let (virtual_cam, capture_card) = crate::classify(&name);
                let index = c.index().as_index().map(|v| v as usize).unwrap_or(i);
                DeviceInfo { backend: Backend::Native, index, name, node: String::new(), caps: String::new(), virtual_cam, capture_card }
            })
            .collect()
    }
}

pub fn frame_format(mode: &Mode) -> FrameFormat {
    if mode.format == "yuyv" {
        FrameFormat::YUYV
    } else if mode.format == "nv12" {
        FrameFormat::NV12
    } else {
        FrameFormat::MJPEG
    }
}

/// Formatos que ofrece el dispositivo (para `vidbench formats`).
pub fn formats(index: usize) -> Result<Vec<String>, String> {
    guard("leer formatos", || formats_inner(index))?
}

fn formats_inner(index: usize) -> Result<Vec<String>, String> {
    #[cfg(target_os = "macos")]
    {
        return mac::formats(index);
    }
    #[cfg(not(target_os = "macos"))]
    {
    nokhwa::nokhwa_initialize(|_| {});
    let req = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::None);
    let mut cam = Camera::new(CameraIndex::Index(index as u32), req).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for ff in cam.compatible_fourcc().map_err(|e| e.to_string())? {
        let by = cam.compatible_list_by_resolution(ff).map_err(|e| e.to_string())?;
        let mut items: Vec<_> = by.into_iter().collect();
        items.sort_by_key(|(r, _)| std::cmp::Reverse(r.width() * r.height()));
        for (res, fps) in items {
            out.push(format!("{ff:?} {}x{} fps={fps:?}", res.width(), res.height()));
        }
    }
    Ok(out)
    }
}

/// Captura hasta `latest.stop()`. Cualquier pánico del backend (NSException en macOS) vuelve como `Err`.
pub fn run(name: Option<&str>, index: Option<usize>, mode: &Mode, latest: &Latest) -> Result<(), String> {
    guard("captura de vídeo", || run_inner(name, index, mode, latest))?
}

fn run_inner(name: Option<&str>, index: Option<usize>, mode: &Mode, latest: &Latest) -> Result<(), String> {
    nokhwa::nokhwa_initialize(|_| {});
    // el NOMBRE manda sobre el índice (los índices cambian al reenchufar); el índice es el respaldo
    let idx = match (name, index) {
        (Some(n), idx) => {
            let nl = n.to_lowercase();
            let devs = list();
            match devs.iter().find(|d| d.name.to_lowercase() == nl).or_else(|| devs.iter().find(|d| d.name.to_lowercase().contains(&nl))) {
                Some(d) => d.index,
                None => match idx {
                    Some(i) if devs.iter().any(|d| d.index == i && !d.virtual_cam) => i,
                    _ => return Err(format!("no hay ninguna cámara llamada {n:?}")),
                },
            }
        }
        (None, Some(i)) => i,
        (None, None) => list().into_iter().find(|d| d.capture_card).or_else(|| list().into_iter().find(|d| !d.virtual_cam)).ok_or("no hay cámara")?.index,
    };
    // macOS: captura AVFoundation propia (ver mac.rs); el `Camera` de nokhwa no sirve con la MS2109
    #[cfg(target_os = "macos")]
    {
        return mac::run(idx, mode, latest);
    }
    #[allow(unreachable_code)]
    let mut cam = open_camera(idx, mode)?;
    let f = cam.camera_format();
    latest.set_info(format!("nokhwa {:?} {}x{}@{}", f.format(), f.width(), f.height(), f.frame_rate()));
    let mut seq = 0u64;
    let mut bad = 0u32;
    let fast = std::env::var_os("DKVM_DECODE_HQ").is_none(); // rápida por defecto; DKVM_DECODE_HQ=1 = calidad (upsampling fancy)
    while !latest.stopped() {
        let buf = match cam.frame() {
            Ok(b) => b,
            Err(e) => {
                bad += 1;
                if bad > 30 {
                    return Err(format!("leer cuadro: {e}"));
                }
                continue;
            }
        };
        let arrived = Instant::now();
        let res = buf.resolution();
        let (mut w, mut h) = (res.width(), res.height());
        let t = Instant::now();
        // el MS2109 a veces entrega cuadros vacíos/corruptos (p. ej. al pedir 1080p60): se saltan
        let rgba = if buf.source_frame_format() == FrameFormat::MJPEG {
            match decode_mjpeg(buf.buffer(), fast) {
                Ok((px, dw, dh)) => {
                    w = dw;
                    h = dh;
                    px
                }
                Err(e) => {
                    bad += 1;
                    latest.set_info(format!("nokhwa: {bad} cuadros corruptos descartados ({e})"));
                    if bad > 30 && seq == 0 {
                        return Err(format!("el dispositivo no entrega cuadros válidos en este modo: {e}"));
                    }
                    continue;
                }
            }
        } else {
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            if let Err(e) = buf.decode_image_to_buffer::<RgbAFormat>(&mut rgba) {
                bad += 1;
                latest.set_info(format!("nokhwa: {bad} cuadros corruptos descartados ({e})"));
                if bad > 30 && seq == 0 {
                    return Err(format!("el dispositivo no entrega cuadros válidos en este modo: {e}"));
                }
                continue;
            }
            rgba
        };
        let decode = t.elapsed();
        latest.publish(Frame { w, h, rgba, seq, arrived, decode });
        seq += 1;
    }
    let _ = cam.stop_stream();
    let _ = Duration::ZERO;
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn frame_format_of(fourcc: &str) -> Option<FrameFormat> {
    match fourcc.to_ascii_uppercase().as_str() {
        "MJPG" | "MJPEG" | "JPEG" => Some(FrameFormat::MJPEG),
        "YUYV" | "YUY2" => Some(FrameFormat::YUYV),
        "NV12" => Some(FrameFormat::NV12),
        _ => None,
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum Att {
    Exact(CameraFormat),
    Closest(CameraFormat),
    Default,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
/// Abre el dispositivo probando una cascada de formatos (ver `pick::plan_attempts`); si todo falla, el error dice qué se probó.
fn open_camera(idx: usize, mode: &Mode) -> Result<Camera, String> {
    let want = (mode.w, mode.h, mode.fps);
    let dev = list().into_iter().find(|d| d.index == idx);
    let label = match &dev {
        Some(d) if !d.node.is_empty() => format!("{} [{}]", d.name, d.node),
        Some(d) => d.name.clone(),
        None => format!("cámara #{idx}"),
    };
    #[cfg(target_os = "linux")]
    if dev.is_none() {
        return Err(format!("el nodo /dev/video{idx} no es un nodo de captura (metadatos u otra función) o no se puede abrir: elige otra fuente (`dkvm-gui --video-list` muestra los nodos)"));
    }
    let cf = |ff: FrameFormat, w: u32, h: u32, f: u32| CameraFormat::new(Resolution::new(w, h), ff, f);
    let mut attempts: Vec<(String, Att)> = Vec::new();
    #[cfg(target_os = "linux")]
    match crate::linux::offers(idx) {
        Ok(offers) => {
            for a in crate::pick::plan_attempts(want, &offers) {
                match a.fourcc.as_deref().and_then(frame_format_of) {
                    Some(ff) => attempts.push((a.label(), Att::Exact(cf(ff, a.w, a.h, a.fps)))),
                    None if a.fourcc.is_none() => attempts.push((a.label(), Att::Default)),
                    None => {}
                }
            }
        }
        Err(e) => attempts.push((format!("lectura de modos: {e}"), Att::Default)),
    }
    if attempts.is_empty() {
        for (n, ff) in [("MJPEG", FrameFormat::MJPEG), ("YUYV", FrameFormat::YUYV), ("NV12", FrameFormat::NV12)] {
            attempts.push((format!("{n} más cercano a {}x{}@{}", want.0, want.1, want.2), Att::Closest(cf(ff, want.0, want.1, want.2))));
        }
        attempts.push(("formato por defecto del dispositivo".into(), Att::Default));
    }
    let mut tried: Vec<(String, String)> = Vec::new();
    for (name, att) in attempts {
        let req = match att {
            Att::Exact(c) => RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Exact(c)),
            Att::Closest(c) => RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(c)),
            Att::Default => RequestedFormat::new::<RgbAFormat>(RequestedFormatType::None),
        };
        // cada intento aislado: si AVFoundation lanza una excepción con un formato, se prueba el siguiente
        let opened = guard("abrir cámara", || match Camera::new(CameraIndex::Index(idx as u32), req) {
            Ok(mut cam) => match cam.open_stream() {
                Ok(()) => Ok(cam),
                Err(e) => Err(format!("iniciar captura: {e}")),
            },
            Err(e) => Err(e.to_string()),
        });
        match opened {
            Ok(Ok(cam)) => return Ok(cam),
            Ok(Err(e)) | Err(e) => tried.push((name, e)),
        }
    }
    Err(crate::pick::describe_failure(&label, &tried))
}

/// MJPEG -> RGBA directo con libjpeg-turbo (mozjpeg): sin pasar por RGB888 ni la conversión de nokhwa.
/// `fast`: DCT rápida y sin "fancy upsampling" (algo de color en los bordes a cambio de ~15-20 % de CPU).
pub fn decode_mjpeg(data: &[u8], fast: bool) -> Result<(Vec<u8>, u32, u32), String> {
    let mut d = mozjpeg::Decompress::new_mem(data).map_err(|e| e.to_string())?;
    if fast {
        d.dct_method(mozjpeg::DctMethod::IntegerFast);
        d.do_fancy_upsampling(false);
        d.do_block_smoothing(false);
    }
    let (w, h) = (d.width(), d.height());
    let mut st = d.rgba().map_err(|e| e.to_string())?;
    let mut out = vec![0u8; w * h * 4];
    st.read_scanlines_into::<u8>(&mut out).map_err(|e| e.to_string())?;
    st.finish().map_err(|e| e.to_string())?;
    Ok((out, w as u32, h as u32))
}

/// Modos MJPEG (ancho, alto, fps) que ofrece el dispositivo. Abre el dispositivo SIN iniciar la captura (no enciende el
/// piloto de una webcam); solo se llama para la fuente elegida, nunca al enumerar.
pub fn modes(index: usize) -> Vec<(u32, u32, u32)> {
    guard("leer modos", || modes_inner(index)).unwrap_or_default()
}

fn modes_inner(index: usize) -> Vec<(u32, u32, u32)> {
    // macOS: lectura de AVCaptureDevice.formats sin abrir la cámara (abrirla con nokhwa fija un formato y AVFoundation
    // puede lanzar una NSException con el MS2109)
    #[cfg(target_os = "macos")]
    {
        return mac::modes(index);
    }
    #[cfg(target_os = "linux")]
    {
        let mut out: Vec<(u32, u32, u32)> = crate::linux::offers(index).unwrap_or_default().into_iter().filter(|o| o.fourcc.eq_ignore_ascii_case("MJPG")).map(|o| (o.w, o.h, o.fps)).filter(|m| m.2 >= 5).collect();
        out.sort_by_key(|m| std::cmp::Reverse((m.0 * m.1, m.2)));
        out.dedup();
        return out;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
    nokhwa::nokhwa_initialize(|_| {});
    let req = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::None);
    let Ok(mut cam) = Camera::new(CameraIndex::Index(index as u32), req) else { return vec![] };
    let mut out: Vec<(u32, u32, u32)> = Vec::new();
    let Ok(fourccs) = cam.compatible_fourcc() else { return out };
    for ff in fourccs {
        if ff != FrameFormat::MJPEG {
            continue;
        }
        if let Ok(by) = cam.compatible_list_by_resolution(ff) {
            for (res, fps_list) in by {
                for f in fps_list {
                    if f >= 5 {
                        out.push((res.width(), res.height(), f));
                    }
                }
            }
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse((m.0 * m.1, m.2)));
    out.dedup();
    out
    }
}

#[cfg(test)]
mod guard_tests {
    use super::guard;

    #[test]
    fn panic_becomes_error() {
        let r = guard("probar", || -> u32 { panic!("Uncaught exception <NSException>") });
        let e = r.unwrap_err();
        assert!(e.contains("probar") && e.contains("NSException"), "{e}");
    }

    #[test]
    fn ok_passes_through() {
        assert_eq!(guard("x", || 7).unwrap(), 7);
    }
}
