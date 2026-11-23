//! riotkvm-gui: vídeo de la capturadora HDMI + teclado/ratón hacia el dongle.
//!
//!   riotkvm-gui                       autodetecta dongle (serie) y capturadora
//!   riotkvm-gui --port mock           dongle simulado (sin hardware)
//!   riotkvm-gui --video pattern       patrón de prueba en vez de capturadora
//!   riotkvm-gui --video ffmpeg:"USB Video"   backend ffmpeg/dshow
//!
//! Escape de la captura: Ctrl+Shift+F12 (o el botón Liberar, o perder el foco).

mod app;
mod clip;
mod dlog;
mod hook;
mod settings;
mod theme;
mod tools;

use app::{App, MouseMode, Options};
use clap::Parser;
use riotkvm_core::session::{Connector, Session};
use riotkvm_core::transport::select::{self, Choice};
use riotkvm_core::transport::{MockTransport, SerialTransport, Transport};
use riotkvm_video::{Mode, Source};
use std::time::Duration;

#[derive(Parser)]
#[command(version, about = "KVM de bolsillo: vídeo + teclado/ratón", after_help = "Captura: clic en el vídeo. Liberar: Ctrl+Shift+F12.\nCtrl+Alt+Supr y Win+L no se pueden interceptar en Windows: usa el menú Enviar.")]
struct Args {
    /// 'auto' (BLE, se reconecta solo; si no hay adaptador BLE, serie), 'ble', COM7 o /dev/ttyACM0 (serie), 'mock' o 'ble-mock' (dongle BLE simulado)
    #[arg(short, long, env = "RIOTKVM_PORT", default_value = "auto")]
    port: String,
    /// Distribución del teclado del servidor (es, us, gb, de, fr, it, pt, latam). Por defecto la guardada (selector de la GUI), si no `es`
    #[arg(short, long, env = "RIOTKVM_LAYOUT")]
    layout: Option<String>,
    /// Atajo local para teclear el portapapeles en el servidor: ctrl+shift+v (defecto) | ctrl+alt+v | ctrl+shift+insert | off
    #[arg(long, default_value = "ctrl+shift+v")]
    paste_key: String,
    /// auto | pattern | native[:NOMBRE] | ffmpeg:NOMBRE_DSHOW
    #[arg(long, default_value = "auto")]
    video: String,
    /// Fuente de vídeo: nombre (p. ej. "USB Video"), índice, `auto` o `none`. Manda sobre la guardada en %APPDATA%\riotkvm\video_source.txt
    #[arg(long, value_name = "NOMBRE|ÍNDICE|auto|none")]
    video_source: Option<String>,
    /// Resolución pedida a la cámara (por defecto la última elegida en el desplegable; si no hay, 1280x720)
    #[arg(long)]
    res: Option<String>,
    /// Cuadros por segundo (por defecto el último elegido; si no hay, 60)
    #[arg(long)]
    fps: Option<u32>,
    /// (pruebas) ventana fuera de pantalla y sin robar el foco
    #[arg(long)]
    offscreen: bool,
    /// mjpeg | nv12 | yuyv
    #[arg(long, default_value = "mjpeg")]
    format: String,
    /// Modo de ratón inicial: abs | rel
    #[arg(long, default_value = "abs")]
    mouse: String,
    /// No instalar el hook de teclado de Windows (se usan los eventos de la ventana)
    #[arg(long)]
    no_hook: bool,
    /// (pruebas) guarda una captura de la ventana tras N segundos en FICHERO.png
    #[arg(long, value_names = ["SEGUNDOS", "FICHERO"], num_args = 2)]
    shot: Option<Vec<String>>,
    /// (pruebas) con --port mock: vuelca cada orden enviada a este fichero
    #[arg(long)]
    echo: Option<std::path::PathBuf>,
    /// (pruebas) arranca ya capturado
    #[arg(long)]
    start_captured: bool,
    /// Registro de diagnóstico (captura, teclas, botones, errores del dongle) en este fichero
    #[arg(long, env = "RIOTKVM_GUI_LOG")]
    log: Option<std::path::PathBuf>,
    /// Vuelve a vsync y cola de 2 cuadros de egui (más suave, ~1-2 cuadros más de latencia de presentación)
    #[arg(long)]
    vsync: bool,
    /// Diagnóstico del hook de teclado: lo instala N segundos SIN abrir ventana y cuenta las teclas que ve (escribe en cualquier sitio)
    #[arg(long, value_name = "SEGUNDOS")]
    hook_test: Option<u64>,
    /// Imprime los dispositivos de vídeo (nodo, nombre, capacidades y modos) y sale: para pegar en un informe de fallo
    #[arg(long)]
    video_list: bool,
    /// (pruebas) cierra tras N segundos
    #[arg(long)]
    exit_after: Option<f64>,
}

fn main() -> eframe::Result {
    riotkvm_core::config::adopt_legacy_env(); // DKVM_PORT, DKVM_LAYOUT... (nombre anterior) siguen valiendo
    let a = Args::parse();
    if a.video_list {
        print!("{}", riotkvm_video::diagnostic_report());
        return Ok(());
    }
    if riotkvm_core::config::env_os("ALLOW_MULTI").is_none() && !single_instance() {
        eprintln!("riotkvm-gui: ya hay otra ventana de riotkvm-gui abierta (comparten capturadora y enlace BLE: un segundo cliente desplaza al primero). Ciérrala primero.");
        std::process::exit(2);
    }
    if let Some(secs) = a.hook_test {
        std::env::set_var("RIOTKVM_HOOK_DEBUG", "1");
        let (tx, _rx) = std::sync::mpsc::channel();
        let ok = hook::install(tx, || {});
        println!("hook instalado: {ok}. Escribe en cualquier ventana durante {secs} s...");
        std::thread::sleep(Duration::from_secs(secs));
        println!("el hook vio {} eventos de teclado", dlog::HOOK_SEEN.load(std::sync::atomic::Ordering::Relaxed));
        return Ok(());
    }
    if let Some(p) = &a.log {
        dlog::init(p);
    }
    riotkvm_core::rt::timer_resolution_1ms(); // temporizadores de 1 ms (sleep/esperas de los hilos de entrada y BLE)
    let saved = app::load_video_mode();
    let (sw, sh, sfps) = saved.unwrap_or((1280, 720, 60));
    let (w, h) = a.res.as_deref().and_then(|r| r.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).unwrap_or((sw, sh));
    let mode = Mode { w, h, fps: a.fps.unwrap_or(sfps), format: a.format.clone() };
    // fuente de vídeo: --video (heredado, explícito) > --video-source > guardada > automática (capturadora)
    let devices = riotkvm_video::list_devices();
    let mut video_note: Option<String> = None;
    let mut src_pref = "auto".to_string();
    let source = match a.video.as_str() {
        "auto" => {
            let (pref, hint) = match &a.video_source {
                Some(p) => (p.clone(), None),
                None => match riotkvm_core::config::load("video_source.txt") {
                    Some(t) => {
                        let mut l = t.lines();
                        (l.next().unwrap_or("auto").to_string(), l.next().and_then(|x| x.trim().parse::<usize>().ok()))
                    }
                    None => ("auto".to_string(), None),
                },
            };
            let (src, warn) = riotkvm_video::source_from_pref(&pref, hint, &devices);
            video_note = warn;
            src_pref = match &src {
                Source::Native { name: Some(n), .. } => n.clone(),
                Source::None => "none".into(),
                _ => "auto".into(),
            };
            src
        }
        "pattern" => Source::Pattern,
        "native" => Source::Native { name: None, index: None },
        s if s.starts_with("native:") => Source::Native { name: Some(s[7..].into()), index: s[7..].parse().ok() },
        s if s.starts_with("ffmpeg:") => Source::Ffmpeg { name: s[7..].into() },
        other => {
            eprintln!("--video {other:?} no válido (auto, pattern, native[:N], ffmpeg:NOMBRE)");
            std::process::exit(2)
        }
    };
    let (resolved_src, video_desc) = riotkvm_video::resolve(&source);
    // modos de la fuente elegida (sin encender nada más) y el más parecido al pedido
    let mut modes: Vec<(u32, u32, u32)> = Vec::new();
    let mut mode = mode;
    if let Source::Native { index: Some(i), .. } = &resolved_src {
        modes = riotkvm_video::native::modes(*i);
        if let Some(m) = riotkvm_video::pick_mode((mode.w, mode.h, mode.fps), &modes) {
            if (m.0, m.1, m.2) != (mode.w, mode.h, mode.fps) {
                eprintln!("riotkvm-gui: el modo {}x{}@{} no existe en la fuente: se usa {}x{}@{}", mode.w, mode.h, mode.fps, m.0, m.1, m.2);
                mode = Mode { w: m.0, h: m.1, fps: m.2, format: mode.format.clone() };
            }
        }
    }

    let port = a.port.clone();
    let echo = a.echo.clone();
    let choice = Choice::parse(Some(port.as_str()), false);
    let transport_desc = match &choice {
        Choice::Auto | Choice::Ble => "BLE".to_string(),
        Choice::BleMock => "BLE simulado".to_string(),
        Choice::Mock => "mock".to_string(),
        Choice::Serial(p) => p.clone().unwrap_or_else(|| "serie".into()),
    };
    // BLE se reconecta solo dentro del transporte: el conector se llama una vez. Si no hay adaptador BLE
    // (y hay un dongle por serie), cae a serie para que "abrir y funcionar" siga valiendo.
    let connector: Connector = Box::new(move || -> riotkvm_core::Result<Box<dyn Transport>> {
        match &choice {
            Choice::Mock => {
                let m = MockTransport::new();
                Ok(Box::new(match &echo {
                    Some(path) => {
                        let f = std::sync::Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| riotkvm_core::Error::Transport(e.to_string()))?);
                        m.with_echo(move |l| {
                            use std::io::Write;
                            let _ = writeln!(f.lock().unwrap(), "{l}");
                        })
                    }
                    None => m,
                }))
            }
            Choice::Auto => match select::spawn_ble() {
                Ok(t) => Ok(Box::new(t)),
                Err(e) => {
                    eprintln!("riotkvm-gui: BLE no disponible ({e}); probando serie");
                    Ok(Box::new(SerialTransport::open(None)?))
                }
            },
            Choice::Ble => Ok(Box::new(select::spawn_ble()?)),
            other => select::open(other, Duration::from_secs(1)),
        }
    });

    let paste_chord = hook::PasteChord::parse(&a.paste_key).unwrap_or_else(|| {
        eprintln!("--paste-key {:?} no válido (ctrl+shift+v, ctrl+alt+v, ctrl+shift+insert, off)", a.paste_key);
        std::process::exit(2)
    });
    let opts = Options {
        modes,
        src_pref,
        video_note,
        layout: riotkvm_core::config::effective_layout(a.layout.as_deref()),
        paste_chord,
        mouse: if a.mouse.starts_with("rel") { MouseMode::Relative } else { MouseMode::Absolute },
        hook: !a.no_hook,
        transport_desc,
        shot: a.shot.as_ref().map(|v| (Duration::from_secs_f64(v[0].parse().unwrap_or(2.0)), v[1].clone().into())),
        exit_after: a.exit_after.map(Duration::from_secs_f64),
        start_captured: a.start_captured,
    };
    let layout = riotkvm_core::config::effective_layout(a.layout.as_deref());
    let layout = riotkvm_core::layouts::canonical(&layout).unwrap_or("es").to_string();
    let mut vb = eframe::egui::ViewportBuilder::default().with_title("RiotKVM - KVM de bolsillo").with_inner_size([1100.0, 700.0]);
    if let Ok(img) = image::load_from_memory(include_bytes!("../assets/skull_64.png")) {
        let img = img.to_rgba8();
        let (w, h) = img.dimensions();
        vb = vb.with_icon(eframe::egui::IconData { rgba: img.into_raw(), width: w, height: h });
    }
    if a.offscreen {
        vb = vb.with_position([-32000.0, -32000.0]).with_active(false).with_taskbar(false);
    }
    let mut native = eframe::NativeOptions { viewport: vb, ..Default::default() };
    // presentación de baja latencia: 1 cuadro en cola y sin esperar al vsync (puede haber tearing); --vsync lo revierte
    if !a.vsync {
        native.wgpu_options.surface = eframe::egui_wgpu::SurfaceConfig {
            present_mode: eframe::egui_wgpu::wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: Some(1),
        };
    }
    eframe::run_native(
        "riotkvm-gui",
        native,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            theme::apply(&ctx);
            let session = Session::spawn_with(connector, &layout, Some(Box::new({
                let c = ctx.clone();
                move || c.request_repaint()
            })), settings::initial_session_opts())?;
            let video = app::VideoCtl::start(source, video_desc, mode, ctx);
            Ok(Box::new(App::new(cc, session, video, opts)))
        }),
    )
}

/// Una sola ventana por usuario: la capturadora no se puede abrir dos veces y el dongle solo admite un cliente BLE
/// (el segundo desplazaría al primero). Windows: mutex con nombre; si el proceso muere, Windows lo libera solo.
#[cfg(windows)]
fn single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = r"Local\riotkvm-gui-single-instance".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        let dup = !h.is_null() && GetLastError() == ERROR_ALREADY_EXISTS;
        !dup // el manejador se queda abierto hasta que el proceso termina
    }
}
#[cfg(not(windows))]
fn single_instance() -> bool {
    true
}
