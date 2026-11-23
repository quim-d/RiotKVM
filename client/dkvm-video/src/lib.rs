//! dkvm-video: fuentes de vídeo del KVM con el mínimo búfer posible.
//!
//! Diseño: cada fuente corre en su hilo y publica SOLO el último cuadro en un `Latest`
//! (sustituye al anterior: si la GUI va lenta se descartan cuadros, nunca se acumulan).
//! Backends: `nokhwa` (Media Foundation / V4L2 / AVFoundation, en proceso), `ffmpeg` (tubería
//! rawvideo con `-fflags nobuffer -flags low_delay`) y un patrón de prueba generado.

pub mod ffmpeg;
#[cfg(feature = "nokhwa-backend")]
#[cfg(target_os = "linux")]
pub mod linux;
pub mod macsel;
pub mod pick;
pub mod native;
pub mod pattern;
pub mod stats;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Cuadro RGBA8 (`rgba.len() == w*h*4`).
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub seq: u64,
    /// Instante en que el cuadro llegó a nuestro proceso (ya leído/decodificado).
    pub arrived: Instant,
    /// Tiempo de decodificación/conversión en este proceso (0 si lo hace ffmpeg fuera).
    pub decode: Duration,
}

/// Ranura de "último cuadro": el productor sobrescribe, el consumidor se lleva el más reciente.
#[derive(Default)]
pub struct Latest {
    slot: Mutex<Option<Frame>>,
    cv: Condvar,
    pub dropped: AtomicU64,
    stop: AtomicBool,
    err: Mutex<Option<String>>,
    info: Mutex<String>,
    child_cpu: Mutex<f64>,
    notify: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Latest {
    pub fn new() -> Arc<Latest> {
        Arc::new(Latest::default())
    }
    pub fn publish(&self, f: Frame) {
        let mut g = self.slot.lock().unwrap();
        if let Ok(mut e) = self.err.try_lock() {
            *e = None; // hay cuadros: el error anterior (reintento) ya no aplica
        }
        if g.replace(f).is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        self.cv.notify_one();
        drop(g);
        if let Some(n) = self.notify.lock().unwrap().as_ref() {
            n();
        }
    }
    /// Se llama tras cada cuadro publicado (la GUI pide repintado).
    pub fn set_notify(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.notify.lock().unwrap() = Some(Box::new(f));
    }
    /// Toma el cuadro pendiente sin esperar (GUI).
    pub fn take(&self) -> Option<Frame> {
        self.slot.lock().unwrap().take()
    }
    /// Espera un cuadro hasta `timeout` (bancos de prueba).
    pub fn wait(&self, timeout: Duration) -> Option<Frame> {
        let mut g = self.slot.lock().unwrap();
        if g.is_none() {
            g = self.cv.wait_timeout(g, timeout).unwrap().0;
        }
        g.take()
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.cv.notify_all();
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    pub fn set_error(&self, e: impl Into<String>) {
        *self.err.lock().unwrap() = Some(e.into());
    }
    pub fn error(&self) -> Option<String> {
        self.err.lock().unwrap().clone()
    }
    /// Descripción del modo realmente negociado (para la barra de estado).
    pub fn set_info(&self, s: impl Into<String>) {
        *self.info.lock().unwrap() = s.into();
    }
    pub fn set_child_cpu(&self, s: f64) {
        *self.child_cpu.lock().unwrap() = s;
    }
    pub fn child_cpu(&self) -> f64 {
        *self.child_cpu.lock().unwrap()
    }
    pub fn info(&self) -> String {
        self.info.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Native,
    Ffmpeg,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub backend: Backend,
    pub index: usize,
    pub name: String,
    /// Nodo del sistema (Linux: `/dev/video4`); vacío si no aplica.
    pub node: String,
    /// Capacidades legibles (Linux: `VIDEO_CAPTURE|STREAMING`); vacío si no aplica.
    pub caps: String,
    /// Cámara virtual (OBS, móvil como webcam...): nunca se elige sola.
    pub virtual_cam: bool,
    pub capture_card: bool,
}

/// (es_virtual, es_capturadora) a partir del nombre.
pub fn classify(name: &str) -> (bool, bool) {
    let n = name.to_lowercase();
    let virt = ["virtual", "obs", "droidcam", "ndi", "snap camera"].iter().any(|k| n.contains(k));
    let card = ["usb video", "capture", "hdmi", "cam link", "ms2109", "ms2130", "uvc", "grabber"].iter().any(|k| n.contains(k));
    (virt, card && !virt)
}

/// Qué fuente abrir.
#[derive(Debug, Clone)]
pub enum Source {
    /// Sin vídeo (no se abre ningún dispositivo).
    None,
    /// Capturadora si hay; si no, cámara real; si no, patrón de prueba.
    Auto,
    Pattern,
    /// Backend nativo (nokhwa), por índice y/o nombre.
    Native { name: Option<String>, index: Option<usize> },
    /// Nombre exacto de dispositivo dshow (Windows) con una tubería ffmpeg.
    Ffmpeg { name: String },
}

#[derive(Debug, Clone)]
pub struct Mode {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
    /// Formato de la cámara: "mjpeg" (por defecto) o "yuyv".
    pub format: String,
}

impl Default for Mode {
    fn default() -> Self {
        // El MS2109 anuncia 1080p60 MJPEG pero entrega 30 (medido): 1080p30 es el modo por defecto.
        Mode { w: 1920, h: 1080, fps: 30, format: "mjpeg".into() }
    }
}

/// Dispositivos de vídeo visibles.
pub fn list_devices() -> Vec<DeviceInfo> {
    #[cfg(feature = "nokhwa-backend")]
    {
        native::list()
    }
    #[cfg(not(feature = "nokhwa-backend"))]
    {
        ffmpeg::list()
    }
}

/// Elección automática: capturadora (MS2109 "USB Video"...) > cámara real > nada (patrón). Nunca una cámara virtual.
pub fn choose_auto(devs: &[DeviceInfo]) -> Option<&DeviceInfo> {
    devs.iter().find(|d| d.capture_card).or_else(|| devs.iter().find(|d| !d.virtual_cam))
}

/// Interpreta una preferencia de fuente (`--video-source`, fichero guardado): `none`, `auto`, un índice, o un nombre
/// (exacto, sin distinguir mayúsculas; si no, el primero que lo contenga). El nombre manda sobre el índice.
/// `idx_hint` es el índice guardado junto al nombre (respaldo si el nombre ya no está y el índice sigue existiendo).
/// Devuelve la fuente y, si no se pudo cumplir, un aviso.
pub fn source_from_pref(pref: &str, idx_hint: Option<usize>, devs: &[DeviceInfo]) -> (Source, Option<String>) {
    let p = pref.trim();
    let low = p.to_lowercase();
    if low.is_empty() || low == "auto" {
        return (Source::Auto, None);
    }
    if low == "none" || low == "ninguna" || low == "off" {
        return (Source::None, None);
    }
    let native = |d: &DeviceInfo| Source::Native { name: Some(d.name.clone()), index: Some(d.index) };
    if let Ok(i) = p.parse::<usize>() {
        return match devs.iter().find(|d| d.index == i) {
            Some(d) => (native(d), None),
            None => (Source::Auto, Some(format!("no hay ninguna cámara con índice {i}: se usa la automática"))),
        };
    }
    if let Some(d) = devs.iter().find(|d| d.name.to_lowercase() == low).or_else(|| devs.iter().find(|d| d.name.to_lowercase().contains(&low))) {
        return (native(d), None);
    }
    if let Some(d) = idx_hint.and_then(|i| devs.iter().find(|d| d.index == i && !d.virtual_cam)) {
        return (native(d), Some(format!("«{p}» ya no aparece: se usa el dispositivo #{} ({})", d.index, d.name)));
    }
    (Source::Auto, Some(format!("«{p}» no está conectada: se usa la fuente automática")))
}

/// Elige el modo disponible más cercano al deseado. Si no hay ninguno parecido, prefiere MJPEG 720p60.
/// `avail`: (ancho, alto, fps). Devuelve `None` si la lista está vacía.
pub fn pick_mode(want: (u32, u32, u32), avail: &[(u32, u32, u32)]) -> Option<(u32, u32, u32)> {
    if avail.contains(&want) {
        return Some(want);
    }
    let score = |m: &(u32, u32, u32)| -> i64 {
        let area = (m.0 as i64 * m.1 as i64 - want.0 as i64 * want.1 as i64).abs() / 1000;
        let fps = (m.2 as i64 - want.2 as i64).abs() * 50; // 10 fps de diferencia ~ 0,5 Mpx: se prefiere conservar la resolución pedida
        // de los parecidos, un pequeño sesgo hacia 720p60 (baja latencia)
        let pref = if (m.0, m.1, m.2) == (1280, 720, 60) { -1 } else { 0 };
        area + fps + pref
    };
    avail.iter().min_by_key(|m| score(m)).copied()
}

/// Resuelve `Auto` a una fuente concreta y la describe (para la barra de estado).
pub fn resolve(src: &Source) -> (Source, String) {
    match src {
        Source::None => (Source::None, "sin vídeo (fuente: ninguna)".into()),
        Source::Auto => {
            let devs = list_devices();
            let pick = choose_auto(&devs);
            match pick {
                Some(d) => (
                    Source::Native { name: Some(d.name.clone()), index: Some(d.index) },
                    format!("{}{}", d.name, if d.capture_card { " (capturadora)" } else { " (cámara)" }),
                ),
                None => (Source::Pattern, "patrón de prueba (no hay capturadora ni cámara real)".into()),
            }
        }
        Source::Pattern => (Source::Pattern, "patrón de prueba".into()),
        Source::Native { name, index } => (src.clone(), format!("nativo {}", name.clone().unwrap_or_else(|| format!("#{}", index.unwrap_or(0))))),
        Source::Ffmpeg { name } => (src.clone(), format!("ffmpeg {name}")),
    }
}

/// Intentos de apertura seguidos antes de dejar de reintentar (cada 2 s) y mostrar el último error.
pub const MAX_OPEN_ATTEMPTS: u32 = 5;

/// Texto de diagnóstico (para pegar en un informe): nodos, capacidades y modos de cada dispositivo de vídeo.
pub fn diagnostic_report() -> String {
    #[cfg(all(feature = "nokhwa-backend", target_os = "linux"))]
    {
        let mut s = String::from("Dispositivos de vídeo (V4L2)\n");
        for n in linux::nodes() {
            let cap = pick::is_capture_node(&n);
            s.push_str(&format!(
                "{}  card=\"{}\" driver={} bus={} usb={} caps=[{}]  captura={}\n",
                n.path,
                n.card,
                n.driver,
                n.bus,
                n.usb.map(|(v, p)| format!("{v:04x}:{p:04x}")).unwrap_or_else(|| "-".into()),
                pick::caps_text(n.caps),
                match (&n.open_err, cap) {
                    (Some(e), _) => format!("NO ({e})"),
                    (None, true) => "sí".to_string(),
                    (None, false) => "no (nodo de metadatos u otra función)".to_string(),
                }
            ));
            if cap {
                match linux::offers(n.index) {
                    Ok(offers) => {
                        let mut fmts: Vec<String> = Vec::new();
                        for o in &offers {
                            if !fmts.contains(&o.fourcc) {
                                fmts.push(o.fourcc.clone());
                            }
                        }
                        for f in fmts {
                            let mut sizes: Vec<(u32, u32)> = Vec::new();
                            for o in offers.iter().filter(|o| o.fourcc == f) {
                                if !sizes.contains(&(o.w, o.h)) {
                                    sizes.push((o.w, o.h));
                                }
                            }
                            let parts: Vec<String> = sizes
                                .iter()
                                .map(|(w, h)| {
                                    let fps: Vec<String> = offers.iter().filter(|o| o.fourcc == f && o.w == *w && o.h == *h).map(|o| o.fps.to_string()).collect();
                                    format!("{w}x{h}@{}", fps.join("/"))
                                })
                                .collect();
                            s.push_str(&format!("    {f}: {}\n", parts.join("  ")));
                        }
                    }
                    Err(e) => s.push_str(&format!("    (no se pudieron leer los modos: {e})\n")),
                }
            }
        }
        s
    }
    #[cfg(all(feature = "nokhwa-backend", not(target_os = "linux")))]
    {
        let mut s = String::from("Dispositivos de vídeo (nokhwa)\n");
        for d in list_devices() {
            s.push_str(&format!("#{}  {}{}{}\n", d.index, d.name, if d.virtual_cam { " [virtual]" } else { "" }, if d.capture_card { " [capturadora]" } else { "" }));
            match native::formats(d.index) {
                Ok(v) => v.iter().for_each(|l| s.push_str(&format!("    {l}\n"))),
                Err(e) => s.push_str(&format!("    (no se pudieron leer los modos: {e})\n")),
            }
        }
        s
    }
    #[cfg(not(feature = "nokhwa-backend"))]
    {
        "compilado sin backend nativo".to_string()
    }
}

/// Arranca la fuente en un hilo; los cuadros llegan a `latest`.
pub fn start(src: &Source, mode: &Mode, latest: Arc<Latest>) -> std::thread::JoinHandle<()> {
    let (src, mode) = (src.clone(), mode.clone());
    let retry = std::env::var_os("DKVM_VIDEO_NORETRY").is_none();
    std::thread::Builder::new()
        .name("dkvm-video".into())
        .spawn(move || {
            // Si la capturadora se reenchufa o falla al abrir (Media Foundation tarda unos segundos en
            // volver a ofrecerla tras reenumerar), se reintenta cada 2 s hasta que `stop()`.
            let mut fails = 0u32;
            while !latest.stopped() {
                let r = match &src {
                    Source::Auto => {
                        // `Auto` se vuelve a resolver cada 2 s: si la capturadora aparece (o vuelve tras
                        // reenchufar) se pasa del patrón de prueba al vídeo real sin reiniciar la GUI.
                        match resolve(&Source::Auto).0 {
                            Source::None => Ok(()),
                            Source::Pattern => pattern::run_for(&mode, &latest, Duration::from_secs(2)),
                            #[cfg(feature = "nokhwa-backend")]
                            Source::Native { name, index } => native::run(name.as_deref(), index, &mode, &latest),
                            other => Err(format!("fuente inesperada: {other:?}")),
                        }
                    }
                    Source::None => {
                        latest.set_info("sin vídeo (fuente: ninguna)".to_string());
                        while !latest.stopped() {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Ok(())
                    }
                    Source::Pattern => pattern::run(&mode, &latest),
                    #[cfg(feature = "nokhwa-backend")]
                    Source::Native { name, index } => native::run(name.as_deref(), *index, &mode, &latest),
                    #[cfg(not(feature = "nokhwa-backend"))]
                    Source::Native { .. } => Err("compilado sin backend nativo".to_string()),
                    Source::Ffmpeg { name } => ffmpeg::run(name, &mode, &latest),
                };
                match r {
                    Ok(()) if matches!(src, Source::Auto) && !latest.stopped() => {}
                    Ok(()) => break,
                    Err(e) => {
                        fails += 1;
                        if !retry || fails >= MAX_OPEN_ATTEMPTS {
                            // sin bucle silencioso: se deja el último error a la vista y se para
                            latest.set_error(format!("{e}\n(se detienen los reintentos tras {fails} intentos: cambia de fuente o pulsa Actualizar)"));
                            break;
                        }
                        latest.set_error(format!("{e}\n(reintentando, intento {fails}/{MAX_OPEN_ATTEMPTS})"));
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
            }
        })
        .expect("hilo de vídeo")
}

/// BGRA (filas de `stride` bytes, `stride >= w*4`) -> RGBA compacto (`w*h*4`) EN EL MISMO búfer, sin reservar otro:
/// cada fila se desplaza a su sitio (el destino nunca adelanta al origen) y se intercambian R y B con alfa 255.
/// Medido a 1080p sin relleno: 2,5-2,7 ms -> 1,6-1,7 ms por cuadro (copia del callback + conversión + paso a la GUI).
pub fn bgra_to_rgba_in_place(mut data: Vec<u8>, w: usize, h: usize, stride: usize) -> Vec<u8> {
    let row = w * 4;
    assert!(stride >= row && data.len() >= stride * h.saturating_sub(1) + row, "búfer BGRA demasiado corto");
    let swizzle = |px: &mut [u8]| {
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
    };
    if stride == row {
        swizzle(&mut data[..row * h]);
    } else {
        for y in 0..h {
            data.copy_within(y * stride..y * stride + row, y * row);
            swizzle(&mut data[y * row..(y + 1) * row]);
        }
    }
    data.truncate(row * h);
    data
}

#[cfg(test)]
mod convert_tests {
    #[test]
    fn bgra_to_rgba_with_and_without_row_padding() {
        let (w, h) = (3usize, 2usize);
        for pad in [0usize, 4, 8] {
            let stride = w * 4 + pad;
            let mut src = vec![0xEEu8; stride * h];
            for y in 0..h {
                for x in 0..w {
                    let i = y * stride + x * 4;
                    let v = (y * w + x) as u8 * 10;
                    src[i..i + 4].copy_from_slice(&[v, v + 1, v + 2, 7]); // B G R A
                }
            }
            let out = super::bgra_to_rgba_in_place(src, w, h, stride);
            assert_eq!(out.len(), w * h * 4);
            for (k, px) in out.chunks_exact(4).enumerate() {
                let v = k as u8 * 10;
                assert_eq!(px, [v + 2, v + 1, v, 255], "píxel {k} con relleno {pad}");
            }
        }
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    fn dev(i: usize, name: &str) -> DeviceInfo {
        let (virtual_cam, capture_card) = classify(name);
        DeviceInfo { backend: Backend::Native, index: i, name: name.into(), node: String::new(), caps: String::new(), virtual_cam, capture_card }
    }

    fn rig() -> Vec<DeviceInfo> {
        vec![dev(0, "Integrated Camera"), dev(1, "Phone (Windows Virtual Camera)"), dev(2, "USB Video")]
    }

    #[test]
    fn auto_prefers_the_capture_card_over_the_webcam_and_never_a_virtual_camera() {
        let d = rig();
        assert_eq!(choose_auto(&d).unwrap().name, "USB Video");
        let only_webcam_and_virtual = vec![dev(0, "OBS Virtual Camera"), dev(1, "Integrated Camera")];
        assert_eq!(choose_auto(&only_webcam_and_virtual).unwrap().name, "Integrated Camera");
        assert!(choose_auto(&[dev(0, "OBS Virtual Camera")]).is_none());
    }

    #[test]
    fn source_preferences_name_beats_index_and_fall_back_sanely() {
        let d = rig();
        let nm = |s: &Source| match s {
            Source::Native { name, index } => (name.clone().unwrap(), index.unwrap()),
            other => panic!("{other:?}"),
        };
        // por nombre (sin mayúsculas / parcial), aunque el índice guardado sea otro
        assert_eq!(nm(&source_from_pref("usb video", Some(0), &d).0), ("USB Video".into(), 2));
        assert_eq!(nm(&source_from_pref("Integrated", None, &d).0), ("Integrated Camera".into(), 0));
        // por índice
        assert_eq!(nm(&source_from_pref("2", None, &d).0), ("USB Video".into(), 2));
        // none / auto
        assert!(matches!(source_from_pref("none", None, &d).0, Source::None));
        assert!(matches!(source_from_pref("auto", None, &d).0, Source::Auto));
        // el nombre ya no está: respaldo por índice (avisa); sin respaldo: automática (avisa)
        let (s, warn) = source_from_pref("Capturadora vieja", Some(2), &d);
        assert_eq!(nm(&s).1, 2);
        assert!(warn.is_some());
        let (s, warn) = source_from_pref("Capturadora vieja", None, &d);
        assert!(matches!(s, Source::Auto) && warn.is_some());
        // índice inexistente
        assert!(source_from_pref("9", None, &d).1.is_some());
        // el índice de respaldo no puede ser una cámara virtual
        assert!(matches!(source_from_pref("x", Some(1), &d).0, Source::Auto));
    }

    #[test]
    fn closest_mode_is_chosen_and_720p60_is_preferred() {
        let usb = [(1920, 1080, 30), (1280, 720, 60), (1280, 720, 30), (640, 480, 30)];
        assert_eq!(pick_mode((1280, 720, 60), &usb), Some((1280, 720, 60)));
        // la webcam integrada solo da 30 fps
        let webcam = [(1280, 720, 30), (640, 480, 30), (1920, 1080, 30)];
        assert_eq!(pick_mode((1280, 720, 60), &webcam), Some((1280, 720, 30)));
        assert_eq!(pick_mode((1920, 1080, 60), &webcam), Some((1920, 1080, 30)));
        // un modo raro: el más parecido
        assert_eq!(pick_mode((1600, 900, 60), &usb), Some((1280, 720, 60)));
        assert_eq!(pick_mode((1280, 720, 60), &[]), None);
    }
}
