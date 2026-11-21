//! Enumeración V4L2 directa (Linux): nodos con sus capacidades y los modos que ofrece cada uno.
//! Abrir un nodo para consultarlo NO inicia la captura (no enciende el piloto de una webcam).

use crate::pick::{fps_from_interval, NodeInfo, Offer};
use v4l::frameinterval::FrameIntervalEnum;
use v4l::framesize::FrameSizeEnum;
use v4l::video::Capture;
use v4l::{context, Device};

fn read_hex(path: &str) -> Option<u16> {
    u16::from_str_radix(std::fs::read_to_string(path).ok()?.trim(), 16).ok()
}

/// (idVendor, idProduct) del dispositivo USB que hay detrás del nodo, vía sysfs.
fn usb_ids(index: usize) -> Option<(u16, u16)> {
    let base = format!("/sys/class/video4linux/video{index}/device/..");
    Some((read_hex(&format!("{base}/idVendor"))?, read_hex(&format!("{base}/idProduct"))?))
}

/// Todos los nodos `/dev/video*` con sus `device_caps`.
pub fn nodes() -> Vec<NodeInfo> {
    context::enum_devices()
        .into_iter()
        .map(|n| {
            let mut ni = NodeInfo { index: n.index(), path: n.path().to_string_lossy().to_string(), ..Default::default() };
            match Device::with_path(n.path()) {
                Ok(d) => match d.query_caps() {
                    Ok(c) => {
                        ni.card = c.card;
                        ni.driver = c.driver;
                        ni.bus = c.bus;
                        ni.caps = c.capabilities.bits();
                    }
                    Err(e) => ni.open_err = Some(format!("VIDIOC_QUERYCAP: {e}")),
                },
                Err(e) => ni.open_err = Some(e.to_string()),
            }
            ni.usb = usb_ids(ni.index);
            ni
        })
        .collect()
}

/// Modos (formato, tamaño, fps) que ofrece un nodo.
pub fn offers(index: usize) -> Result<Vec<Offer>, String> {
    let dev = Device::new(index).map_err(|e| format!("/dev/video{index}: {e}"))?;
    let mut out: Vec<Offer> = Vec::new();
    for f in dev.enum_formats().map_err(|e| format!("VIDIOC_ENUM_FMT en /dev/video{index}: {e}"))? {
        let name = f.fourcc.str().unwrap_or("????").to_string();
        for fs in dev.enum_framesizes(f.fourcc).unwrap_or_default() {
            let dims: Vec<(u32, u32)> = match fs.size {
                FrameSizeEnum::Discrete(d) => vec![(d.width, d.height)],
                FrameSizeEnum::Stepwise(s) => vec![(s.min_width, s.min_height), (s.max_width, s.max_height)],
            };
            for (w, h) in dims {
                let mut got = false;
                for iv in dev.enum_frameintervals(f.fourcc, w, h).unwrap_or_default() {
                    let fr: Vec<(u32, u32)> = match iv.interval {
                        FrameIntervalEnum::Discrete(fr) => vec![(fr.numerator, fr.denominator)],
                        FrameIntervalEnum::Stepwise(s) => vec![(s.min.numerator, s.min.denominator), (s.max.numerator, s.max.denominator)],
                    };
                    for (n, d) in fr {
                        let fps = fps_from_interval(n, d);
                        if fps > 0 {
                            let o = Offer { fourcc: name.clone(), w, h, fps };
                            if !out.contains(&o) {
                                out.push(o);
                            }
                            got = true;
                        }
                    }
                }
                if !got {
                    let o = Offer { fourcc: name.clone(), w, h, fps: 30 };
                    if !out.contains(&o) {
                        out.push(o);
                    }
                }
            }
        }
    }
    Ok(out)
}
