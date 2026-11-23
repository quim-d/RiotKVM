//! Captura en macOS con AVFoundation directo (sin `Camera` de nokhwa).
//!
//! Motivos (leídos en nokhwa 0.10.11 / nokhwa-bindings-macos 0.2.4, medidos por el coordinador en un Mac real):
//! - `set_all` elige el ULTIMO formato con la resolución pedida (p. ej. YUYV a 10 fps) y le aplica la duración de
//!   cuadro de OTRO rango, y redondea los fps a entero: AVFoundation lanza `setActiveVideoMinFrameDuration: Not supported`.
//!   Aquí se elige (formato, rango) con `macsel::plan` y se aplica la duración EXACTA de ese rango.
//! - AVFoundation no ofrece MJPEG con la MS2109: entrega NV12 (420v) descomprimido. nokhwa leía el búfer planar con
//!   `CVPixelBufferGetBaseAddress`, que no es válido en planares. Aquí se pide a la salida BGRA (la conversión la hace
//!   AVFoundation) y el búfer es contiguo.
//! Todo fallo (incluidas las NSException, que `objc` 0.2 convierte en pánico) vuelve como `Err` gracias a `native::guard`.
//! NO verificado en un Mac real: ver docs/handoff/gui-dev.md.

// `msg_send!` de objc 0.2 expande `cfg(feature = "cargo-clippy")`, que rustc ya no conoce (56 avisos sin valor).
#![allow(unexpected_cfgs)]

use crate::macsel::{self, Offer};
use crate::native::guard;
use crate::{Frame, Latest, Mode};
use nokhwa::utils::{CameraIndex, FrameFormat};
use nokhwa_bindings_macos::{AVCaptureDevice, AVCaptureVideoCallback};
use objc::runtime::{Object, BOOL, NO, YES};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;
use std::time::{Duration, Instant};

type Msg = (Vec<u8>, FrameFormat, Option<Duration>);

#[repr(C)]
struct Dim {
    width: i32,
    height: i32,
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionGetDimensions(desc: *mut Object) -> Dim;
    fn CMFormatDescriptionGetMediaSubType(desc: *mut Object) -> u32;
}

const BGRA: u32 = 0x4247_5241; // kCVPixelFormatType_32BGRA

unsafe fn nsstr(s: &str) -> *mut Object {
    let c = CString::new(s).unwrap_or_default();
    msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()]
}

unsafe fn to_string(ns: *mut Object) -> String {
    if ns.is_null() {
        return String::new();
    }
    let p: *const c_char = msg_send![ns, UTF8String];
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

unsafe fn error_text(err: *mut Object) -> String {
    if err.is_null() {
        return String::new();
    }
    let d: *mut Object = msg_send![err, localizedDescription];
    to_string(d)
}

fn fourcc_str(v: u32) -> String {
    v.to_be_bytes().iter().map(|b| if (32..127).contains(b) { *b as char } else { '?' }).collect()
}

fn open_device(index: usize) -> Result<AVCaptureDevice, String> {
    AVCaptureDevice::new(&CameraIndex::Index(index as u32)).map_err(|e| e.to_string())
}

/// Todas las combinaciones (formato, rango) en SOLO LECTURA (no abre ni configura nada).
pub fn offers(index: usize) -> Result<Vec<Offer>, String> {
    guard("leer formatos (AVFoundation)", || {
        let dev = open_device(index)?;
        let mut out = Vec::new();
        unsafe {
            let fmts: *mut Object = msg_send![dev.inner(), formats];
            if fmts.is_null() {
                return Err("el dispositivo no devuelve formatos".to_string());
            }
            let n: usize = msg_send![fmts, count];
            for fi in 0..n {
                let f: *mut Object = msg_send![fmts, objectAtIndex: fi];
                let desc: *mut Object = msg_send![f, formatDescription];
                if desc.is_null() {
                    continue;
                }
                let d = CMVideoFormatDescriptionGetDimensions(desc);
                let cc = fourcc_str(CMFormatDescriptionGetMediaSubType(desc));
                let ranges: *mut Object = msg_send![f, videoSupportedFrameRateRanges];
                if ranges.is_null() {
                    continue;
                }
                let rn: usize = msg_send![ranges, count];
                for ri in 0..rn {
                    let r: *mut Object = msg_send![ranges, objectAtIndex: ri];
                    let fps: f64 = msg_send![r, maxFrameRate];
                    out.push(Offer { fmt_idx: fi, range_idx: ri, w: d.width.max(0) as u32, h: d.height.max(0) as u32, fps, fourcc: cc.clone() });
                }
            }
        }
        Ok(out)
    })?
}

pub fn modes(index: usize) -> Vec<(u32, u32, u32)> {
    macsel::modes(&offers(index).unwrap_or_default())
}

pub fn formats(index: usize) -> Result<Vec<String>, String> {
    let mut v = offers(index)?;
    v.sort_by(|a, b| (b.w * b.h).cmp(&(a.w * a.h)).then(b.fps.partial_cmp(&a.fps).unwrap_or(std::cmp::Ordering::Equal)));
    Ok(v.iter().map(|o| format!("{} {}x{} fps={}", o.fourcc, o.w, o.h, o.fps)).collect())
}

struct Cam {
    dev: AVCaptureDevice,
    session: *mut Object,
    input: *mut Object,
    output: *mut Object,
    _cb: AVCaptureVideoCallback,
    _tx: Arc<flume::Sender<Msg>>,
    rx: flume::Receiver<Msg>,
    w: u32,
    h: u32,
    desc: String,
}

impl Drop for Cam {
    fn drop(&mut self) {
        // un fallo al cerrar no debe propagarse
        let (s, i, o) = (self.session, self.input, self.output);
        let _ = guard("cerrar captura", || unsafe {
            let _: () = msg_send![s, stopRunning];
            let _: () = msg_send![s, removeOutput: o];
            let _: () = msg_send![s, removeInput: i];
        });
        let _ = &self.dev;
    }
}

/// Fija formato y duración de cuadro EXACTOS del rango elegido (con el dispositivo bloqueado para configuración).
unsafe fn apply(dev: *mut Object, o: &Offer) -> Result<(), String> {
    let fmts: *mut Object = msg_send![dev, formats];
    let f: *mut Object = msg_send![fmts, objectAtIndex: o.fmt_idx];
    let ranges: *mut Object = msg_send![f, videoSupportedFrameRateRanges];
    let r: *mut Object = msg_send![ranges, objectAtIndex: o.range_idx];
    let dur: *mut Object = msg_send![r, valueForKey: nsstr("minFrameDuration")];
    if dur.is_null() {
        return Err("el rango no tiene duración de cuadro".into());
    }
    let mut err: *mut Object = std::ptr::null_mut();
    let locked: BOOL = msg_send![dev, lockForConfiguration: &mut err as *mut *mut Object];
    if locked == NO {
        return Err(format!("lockForConfiguration rechazado: {}", error_text(err)));
    }
    // si algo lanza una excepción dentro, el bloqueo se libera igualmente
    let res = guard("fijar formato", || {
        let _: () = msg_send![dev, setValue: f forKey: nsstr("activeFormat")];
        let _: () = msg_send![dev, setValue: dur forKey: nsstr("activeVideoMinFrameDuration")];
        let _: () = msg_send![dev, setValue: dur forKey: nsstr("activeVideoMaxFrameDuration")];
    });
    let _ = guard("liberar bloqueo", || {
        let _: () = msg_send![dev, unlockForConfiguration];
    });
    res
}

/// `videoSettings` de la salida: siempre BGRA; con `w`/`h` > 0 fija además el tamaño de los cuadros.
unsafe fn output_settings(w: u32, h: u32) -> *mut Object {
    let d: *mut Object = msg_send![class!(NSMutableDictionary), dictionary];
    let num: *mut Object = msg_send![class!(NSNumber), numberWithUnsignedInt: BGRA];
    let _: () = msg_send![d, setObject: num forKey: nsstr("PixelFormatType")]; // kCVPixelBufferPixelFormatTypeKey
    if w > 0 && h > 0 {
        let nw: *mut Object = msg_send![class!(NSNumber), numberWithUnsignedInt: w];
        let nh: *mut Object = msg_send![class!(NSNumber), numberWithUnsignedInt: h];
        let _: () = msg_send![d, setObject: nw forKey: nsstr("Width")]; // kCVPixelBufferWidthKey
        let _: () = msg_send![d, setObject: nh forKey: nsstr("Height")]; // kCVPixelBufferHeightKey
    }
    d
}

fn open(index: usize, want: (u32, u32, u32)) -> Result<Cam, String> {
    guard("abrir cámara (AVFoundation)", || open_inner(index, want))?
}

fn open_inner(index: usize, want: (u32, u32, u32)) -> Result<Cam, String> {
    let all = offers(index)?;
    let plan = macsel::plan(want, &all);
    if plan.is_empty() {
        return Err("el dispositivo no ofrece ningún formato".into());
    }
    let dev = open_device(index)?;
    let (tx, rx) = flume::unbounded::<Msg>();
    let tx = Arc::new(tx);
    let label = CString::new(format!("dkvm-cam-{index}")).unwrap_or_default();
    let cb = AVCaptureVideoCallback::new(&label, &tx).map_err(|e| e.to_string())?;
    unsafe {
        let mut err: *mut Object = std::ptr::null_mut();
        let input: *mut Object = {
            let a: *mut Object = msg_send![class!(AVCaptureDeviceInput), alloc];
            msg_send![a, initWithDevice: dev.inner() error: &mut err as *mut *mut Object]
        };
        if input.is_null() {
            return Err(format!("no se pudo abrir el dispositivo ({}); comprueba el permiso de cámara en Ajustes del Sistema > Privacidad y seguridad > Cámara", error_text(err)));
        }
        let session: *mut Object = {
            let a: *mut Object = msg_send![class!(AVCaptureSession), alloc];
            msg_send![a, init]
        };
        let _: () = msg_send![session, beginConfiguration];
        // NO se fija `AVCaptureSessionPresetInputPriority` a mano: `setSessionPreset:` lo rechaza con NSException
        // ("is not a supported preset"). La sesión pasa sola a ese preset cuando `apply` fija `activeFormat`, y por eso
        // `apply` va DESPUÉS de `addInput` (si fuera antes, al añadir la entrada el preset por defecto pisaría el formato).
        let can: BOOL = msg_send![session, canAddInput: input];
        if can == NO {
            return Err("la sesión rechaza la entrada (¿cámara en uso por otra aplicación?)".into());
        }
        let _: () = msg_send![session, addInput: input];
        let output: *mut Object = msg_send![class!(AVCaptureVideoDataOutput), new];
        let _: () = msg_send![output, setVideoSettings: output_settings(0, 0)];
        let _: () = msg_send![output, setAlwaysDiscardsLateVideoFrames: YES]; // baja latencia: nunca cola
        let _: () = msg_send![output, setSampleBufferDelegate: cb.inner() queue: cb.queue().0];
        let can: BOOL = msg_send![session, canAddOutput: output];
        if can == NO {
            return Err("la sesión rechaza la salida de vídeo".into());
        }
        let _: () = msg_send![session, addOutput: output];

        // cascada: se prueba cada oferta, de la más cercana a la menos, hasta que AVFoundation acepte una
        let mut tried: Vec<String> = Vec::new();
        let mut chosen: Option<Offer> = None;
        for o in plan.iter().take(8) {
            match apply(dev.inner(), o) {
                Ok(()) => {
                    chosen = Some(o.clone());
                    break;
                }
                Err(e) => tried.push(format!("{} {}x{}@{}: {e}", o.fourcc, o.w, o.h, o.fps)),
            }
        }
        let Some(o) = chosen else {
            let _: () = msg_send![session, commitConfiguration];
            return Err(format!("AVFoundation rechazó todos los formatos probados: {}", tried.join(" | ")));
        };
        // Medido con la MS2109: con `activeFormat` en 1280x720 la salida seguía entregando 1920x1080 (8294400 bytes).
        // Se fija el tamaño también en la salida para que los cuadros midan lo que `run` espera.
        let _: () = msg_send![output, setVideoSettings: output_settings(o.w, o.h)];
        let _: () = msg_send![session, commitConfiguration];
        let _: () = msg_send![session, startRunning];
        Ok(Cam { dev, session, input, output, _cb: cb, _tx: tx, rx, w: o.w, h: o.h, desc: format!("{} {}x{}@{:.5}", o.fourcc, o.w, o.h, o.fps) })
    }
}

/// Captura hasta `latest.stop()`; cada cuadro BGRA se publica como RGBA.
pub fn run(index: usize, mode: &Mode, latest: &Latest) -> Result<(), String> {
    let cam = open(index, (mode.w, mode.h, mode.fps))?;
    latest.set_info(format!("AVFoundation {} (salida BGRA)", cam.desc));
    let started = Instant::now();
    let mut seq = 0u64;
    let mut bad = 0u32;
    while !latest.stopped() {
        let mut last = match cam.rx.recv_timeout(Duration::from_millis(250)) {
            Ok(m) => m,
            Err(flume::RecvTimeoutError::Timeout) => {
                if seq == 0 && started.elapsed() > Duration::from_secs(6) {
                    return Err("el dispositivo no entrega cuadros (¿permiso de cámara denegado, o cámara en uso por otra aplicación?)".into());
                }
                continue;
            }
            Err(_) => return Err("la captura se interrumpió".into()),
        };
        while let Ok(n) = cam.rx.try_recv() {
            last = n; // solo el último cuadro
        }
        let arrived = Instant::now();
        let (w, h) = (cam.w as usize, cam.h as usize);
        let data = last.0;
        let stride = if h > 0 { data.len() / h } else { 0 };
        // Además de que quepa, el relleno por fila tiene que ser pequeño: un cuadro de otro tamaño (p. ej. 1080p leído
        // como 720p) daría un `stride` enorme y una imagen con filas desplazadas y colores mezclados.
        if stride < w * 4 || stride >= w * 4 + 256 {
            bad += 1;
            latest.set_info(format!("AVFoundation: cuadro de {} bytes no cabe en {w}x{h} ({bad} descartados)", data.len()));
            if bad > 30 && seq == 0 {
                return Err(format!("los cuadros no tienen el tamaño esperado ({} bytes para {w}x{h})", data.len()));
            }
            continue;
        }
        let t = Instant::now();
        let rgba = crate::bgra_to_rgba_in_place(data, w, h, stride);
        let decode = t.elapsed();
        latest.publish(Frame { w: cam.w, h: cam.h, rgba, seq, arrived, decode });
        seq += 1;
    }
    Ok(())
}
