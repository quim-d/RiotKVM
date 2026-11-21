//! Backend nativo con `nokhwa` (Media Foundation en Windows, V4L2 en Linux, AVFoundation en macOS).

use crate::{Backend, DeviceInfo, Frame, Latest, Mode};
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};
use nokhwa::Camera;
use std::time::{Duration, Instant};

pub fn list() -> Vec<DeviceInfo> {
    nokhwa::nokhwa_initialize(|_| {});
    let Ok(cams) = nokhwa::query(ApiBackend::Auto) else { return vec![] };
    cams.iter()
        .enumerate()
        .map(|(i, c)| {
            let name = c.human_name();
            let (virtual_cam, capture_card) = crate::classify(&name);
            let index = c.index().as_index().map(|v| v as usize).unwrap_or(i);
            DeviceInfo { backend: Backend::Native, index, name, virtual_cam, capture_card }
        })
        .collect()
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

pub fn run(name: Option<&str>, index: Option<usize>, mode: &Mode, latest: &Latest) -> Result<(), String> {
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
    let want = CameraFormat::new(Resolution::new(mode.w, mode.h), frame_format(mode), mode.fps);
    let req = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(want));
    let mut cam = Camera::new(CameraIndex::Index(idx as u32), req).map_err(|e| format!("abrir cámara {idx}: {e}"))?;
    cam.open_stream().map_err(|e| format!("iniciar captura: {e}"))?;
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
