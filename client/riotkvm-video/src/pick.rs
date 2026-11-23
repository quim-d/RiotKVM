//! Lógica pura (sin dispositivos) para elegir nodo y formato de captura; testeable con listas simuladas.
//!
//! Problemas que resuelve (Linux/V4L2, reportado en Fedora: "abrir cámara 3: could not get device camera format: Failed to fulfill"):
//! - Cada dispositivo UVC expone varios nodos `/dev/videoN` y SOLO algunos pueden capturar vídeo (el resto son nodos de
//!   metadatos): el selector y "Automática" solo deben usar nodos con `V4L2_CAP_VIDEO_CAPTURE`.
//! - El modo pedido (p. ej. MJPEG 1920x1080@30) puede no existir exactamente: se prueba una CASCADA de formatos
//!   (MJPEG pedido -> MJPEG cercano -> YUYV -> otro -> formato por defecto) y se informa de lo probado.

use crate::{classify, Backend, DeviceInfo};

pub const CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
pub const CAP_VIDEO_CAPTURE_MPLANE: u32 = 0x0000_1000;
pub const CAP_META_CAPTURE: u32 = 0x0080_0000;
pub const CAP_STREAMING: u32 = 0x0400_0000;

/// Nodo V4L2 tal como lo describe el sistema.
#[derive(Debug, Clone, Default)]
pub struct NodeInfo {
    pub index: usize,
    pub path: String,
    pub card: String,
    pub driver: String,
    pub bus: String,
    /// `device_caps` del NODO (no las del dispositivo completo).
    pub caps: u32,
    /// (idVendor, idProduct) USB si se conocen.
    pub usb: Option<(u16, u16)>,
    /// Si no se pudo abrir (permisos...), el motivo.
    pub open_err: Option<String>,
}

pub fn caps_text(c: u32) -> String {
    let names = [(CAP_VIDEO_CAPTURE, "VIDEO_CAPTURE"), (CAP_VIDEO_CAPTURE_MPLANE, "VIDEO_CAPTURE_MPLANE"), (CAP_META_CAPTURE, "META_CAPTURE"), (CAP_STREAMING, "STREAMING"), (0x0100_0000, "READWRITE"), (0x0002_0000, "AUDIO")];
    let v: Vec<&str> = names.iter().filter(|(b, _)| c & b != 0).map(|(_, n)| *n).collect();
    if v.is_empty() {
        format!("0x{c:08x}")
    } else {
        v.join("|")
    }
}

/// ¿Se puede capturar vídeo por este nodo? (los de metadatos o los que no se pueden abrir, no)
pub fn is_capture_node(n: &NodeInfo) -> bool {
    n.open_err.is_none() && n.caps & (CAP_VIDEO_CAPTURE | CAP_VIDEO_CAPTURE_MPLANE) != 0
}

/// Capturadoras conocidas por USB id (MacroSilicon MS2109/MS2130...).
pub fn known_capture_usb(id: Option<(u16, u16)>) -> bool {
    matches!(id, Some((0x534d, 0x2109)) | Some((0x534d, 0x2130)) | Some((0x345f, 0x2130)) | Some((0x534d, 0x0021)))
}

/// Dispositivos que ofrece el selector a partir de los nodos: solo nodos con captura, con nombre amigable y el nodo aparte.
pub fn devices_from_nodes(nodes: &[NodeInfo]) -> Vec<DeviceInfo> {
    nodes
        .iter()
        .filter(|n| is_capture_node(n))
        .map(|n| {
            let name = if n.card.is_empty() { n.path.clone() } else { n.card.clone() };
            let (virtual_cam, card) = classify(&name);
            DeviceInfo {
                backend: Backend::Native,
                index: n.index,
                capture_card: (card || known_capture_usb(n.usb)) && !virtual_cam,
                virtual_cam,
                name,
                node: n.path.clone(),
                caps: caps_text(n.caps),
            }
        })
        .collect()
}

/// Un modo que ofrece el dispositivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// FourCC de V4L2 ("MJPG", "YUYV", "NV12"...).
    pub fourcc: String,
    pub w: u32,
    pub h: u32,
    pub fps: u32,
}

/// Un intento de apertura; `fourcc == None` = formato por defecto del dispositivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub fourcc: Option<String>,
    pub w: u32,
    pub h: u32,
    pub fps: u32,
}

impl Attempt {
    pub fn label(&self) -> String {
        match &self.fourcc {
            Some(f) => format!("{f} {}x{}@{}", self.w, self.h, self.fps),
            None => "formato por defecto del dispositivo".into(),
        }
    }
}

fn is_mjpeg(f: &str) -> bool {
    f.eq_ignore_ascii_case("MJPG") || f.eq_ignore_ascii_case("MJPEG") || f.eq_ignore_ascii_case("JPEG")
}
fn is_yuyv(f: &str) -> bool {
    f.eq_ignore_ascii_case("YUYV") || f.eq_ignore_ascii_case("YUY2")
}
fn is_nv12(f: &str) -> bool {
    f.eq_ignore_ascii_case("NV12")
}

/// Los `n` modos de `offers` (de un mismo grupo) más cercanos al deseado, de mejor a peor.
fn closest(want: (u32, u32, u32), offers: &[&Offer], n: usize) -> Vec<Offer> {
    let mut pool: Vec<(u32, u32, u32)> = offers.iter().map(|o| (o.w, o.h, o.fps)).collect();
    pool.dedup();
    let mut out = Vec::new();
    let mut left = pool.clone();
    for _ in 0..n {
        let Some(m) = crate::pick_mode(want, &left) else { break };
        left.retain(|x| *x != m);
        if let Some(o) = offers.iter().find(|o| (o.w, o.h, o.fps) == m) {
            out.push((*o).clone());
        }
    }
    out
}

/// Cascada de intentos: MJPEG exacto -> MJPEG cercano (hasta 3) -> YUYV (hasta 2) -> NV12 -> formato por defecto.
pub fn plan_attempts(want: (u32, u32, u32), offers: &[Offer]) -> Vec<Attempt> {
    let mut out: Vec<Attempt> = Vec::new();
    let mut push = |o: &Offer| {
        let a = Attempt { fourcc: Some(o.fourcc.clone()), w: o.w, h: o.h, fps: o.fps };
        if !out.contains(&a) {
            out.push(a);
        }
    };
    let mj: Vec<&Offer> = offers.iter().filter(|o| is_mjpeg(&o.fourcc)).collect();
    let yu: Vec<&Offer> = offers.iter().filter(|o| is_yuyv(&o.fourcc)).collect();
    let nv: Vec<&Offer> = offers.iter().filter(|o| is_nv12(&o.fourcc)).collect();
    for o in closest(want, &mj, 3) {
        push(&o);
    }
    for o in closest(want, &yu, 2) {
        push(&o);
    }
    for o in closest(want, &nv, 1) {
        push(&o);
    }
    out.push(Attempt { fourcc: None, w: 0, h: 0, fps: 0 });
    out
}

/// Mensaje de error cuando fallan todos los intentos (qué se probó y el último error), sin repetir ruido.
pub fn describe_failure(dev: &str, tried: &[(String, String)]) -> String {
    let mut s = format!("no se pudo abrir {dev}: ningún formato funcionó");
    for (a, e) in tried {
        s.push_str(&format!("\n  - {a}: {e}"));
    }
    s
}

/// Fracción V4L2 (intervalo en segundos) -> fps redondeados.
pub fn fps_from_interval(num: u32, den: u32) -> u32 {
    if num == 0 {
        0
    } else {
        ((den as f64) / (num as f64)).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(i: usize, card: &str, caps: u32, usb: Option<(u16, u16)>) -> NodeInfo {
        NodeInfo { index: i, path: format!("/dev/video{i}"), card: card.into(), driver: "uvcvideo".into(), bus: "usb".into(), caps, usb, open_err: None }
    }

    const CAP: u32 = CAP_VIDEO_CAPTURE | CAP_STREAMING;
    const META: u32 = CAP_META_CAPTURE | CAP_STREAMING;

    /// Reproduce el caso del Fedora: webcam (captura + metadatos) y capturadora MS2109 (captura + metadatos).
    fn fedora_nodes() -> Vec<NodeInfo> {
        vec![
            node(0, "Integrated Camera: Integrated C", CAP, Some((0x0bda, 0x5634))),
            node(1, "Integrated Camera: Integrated C", META, Some((0x0bda, 0x5634))),
            node(2, "Integrated Camera: Integrated I", CAP, Some((0x0bda, 0x5634))), // cámara IR
            node(3, "USB Video: USB Video", META, Some((0x534d, 0x2109))),            // <- el índice 3 de Quim
            node(4, "USB Video: USB Video", CAP, Some((0x534d, 0x2109))),
        ]
    }

    #[test]
    fn metadata_nodes_are_not_offered() {
        let d = devices_from_nodes(&fedora_nodes());
        let idx: Vec<usize> = d.iter().map(|x| x.index).collect();
        assert_eq!(idx, vec![0, 2, 4], "los nodos de metadatos (1 y 3) no deben salir");
        assert!(d.iter().all(|x| x.node.starts_with("/dev/video")));
        assert!(!d.iter().any(|x| x.index == 3));
    }

    #[test]
    fn unopenable_nodes_are_not_offered() {
        let mut n = fedora_nodes();
        n[4].open_err = Some("Permission denied".into());
        assert!(devices_from_nodes(&n).iter().all(|x| x.index != 4));
    }

    #[test]
    fn auto_prefers_the_capture_card_over_integrated_webcams() {
        let d = devices_from_nodes(&fedora_nodes());
        let pick = crate::choose_auto(&d).unwrap();
        assert_eq!(pick.index, 4);
        assert!(pick.capture_card);
        // capturadora reconocida por USB id aunque el nombre no diga nada
        let n = vec![node(0, "Integrated Camera: Integrated C", CAP, None), node(6, "Cosa rara", CAP, Some((0x534d, 0x2109)))];
        let d = devices_from_nodes(&n);
        assert_eq!(crate::choose_auto(&d).unwrap().index, 6);
    }

    fn ms2109_without_exact_1080p30() -> Vec<Offer> {
        let o = |f: &str, w, h, fps| Offer { fourcc: f.into(), w, h, fps };
        vec![o("MJPG", 1920, 1080, 20), o("MJPG", 1920, 1080, 10), o("MJPG", 1280, 720, 60), o("MJPG", 1280, 720, 30), o("MJPG", 640, 480, 30), o("YUYV", 1280, 720, 10), o("YUYV", 640, 480, 30)]
    }

    #[test]
    fn cascade_when_the_exact_mode_does_not_exist() {
        let offers = ms2109_without_exact_1080p30();
        let plan = plan_attempts((1920, 1080, 30), &offers);
        // 1.º MJPEG lo más cercano al pedido (mismo tamaño, 20 fps), luego otros MJPEG, luego YUYV, y al final el defecto
        assert_eq!(plan[0], Attempt { fourcc: Some("MJPG".into()), w: 1920, h: 1080, fps: 20 });
        assert!(plan.iter().filter(|a| a.fourcc.as_deref() == Some("MJPG")).count() <= 3);
        let first_yuyv = plan.iter().position(|a| a.fourcc.as_deref() == Some("YUYV")).expect("hay YUYV");
        let last_mjpg = plan.iter().rposition(|a| a.fourcc.as_deref() == Some("MJPG")).unwrap();
        assert!(last_mjpg < first_yuyv, "MJPEG antes que YUYV");
        assert_eq!(plan.last().unwrap().fourcc, None, "último recurso: formato por defecto");
        // sin repetidos
        for (i, a) in plan.iter().enumerate() {
            assert!(!plan[..i].contains(a));
        }
    }

    #[test]
    fn exact_mode_goes_first_and_720p60_is_preferred_for_odd_requests() {
        let offers = ms2109_without_exact_1080p30();
        assert_eq!(plan_attempts((1280, 720, 60), &offers)[0], Attempt { fourcc: Some("MJPG".into()), w: 1280, h: 720, fps: 60 });
        assert_eq!(plan_attempts((1600, 900, 60), &offers)[0].w, 1280);
    }

    #[test]
    fn device_with_only_yuyv_and_device_with_nothing() {
        let yu = vec![Offer { fourcc: "YUYV".into(), w: 640, h: 480, fps: 30 }];
        let p = plan_attempts((1280, 720, 60), &yu);
        assert_eq!(p[0].fourcc.as_deref(), Some("YUYV"));
        assert_eq!(p.len(), 2);
        // nodo sin formatos (p. ej. metadatos): solo queda el defecto; el error final lo explica
        let p = plan_attempts((1280, 720, 60), &[]);
        assert_eq!(p, vec![Attempt { fourcc: None, w: 0, h: 0, fps: 0 }]);
    }

    #[test]
    fn failure_message_lists_every_attempt() {
        let m = describe_failure("USB Video (/dev/video3)", &[("MJPG 1920x1080@20".into(), "Failed to fulfill".into()), ("formato por defecto del dispositivo".into(), "no hay formatos".into())]);
        assert!(m.contains("/dev/video3") && m.contains("MJPG 1920x1080@20") && m.contains("Failed to fulfill") && m.contains("no hay formatos"), "{m}");
    }

    #[test]
    fn interval_to_fps() {
        assert_eq!(fps_from_interval(1, 30), 30);
        assert_eq!(fps_from_interval(1000, 60000), 60);
        assert_eq!(fps_from_interval(333333, 10000000), 30);
        assert_eq!(fps_from_interval(0, 30), 0);
    }
}
