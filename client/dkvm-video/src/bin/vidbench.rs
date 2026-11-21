//! Banco de pruebas de vídeo reproducible.
//!
//! vidbench list                         dispositivos (nativo y dshow)
//! vidbench formats <idx>                formatos que ofrece (nokhwa)
//! vidbench native  [--idx N] [--format mjpeg|yuyv] [--res WxH] [--fps F] [--secs S]
//! vidbench ffmpeg  --name "USB Video" [--format ..] [--res ..] [--fps ..] [--secs S]
//! vidbench e2e [--idx N --res WxH --fps F --key win --n 6 --thr 3 --ble-timeout 15]  latencia ENTRADA->PANTALLA con el BLE ya conectado
//! vidbench lavfi   [--res WxH] [--fps F] [--secs S]    fuente sintética por ffmpeg (sin dispositivo)
//!
//! Métricas por cuadro (todas en este proceso; NO es latencia "de cristal a cristal"):
//!  - intervalo entre llegadas (ms): regularidad de la entrega y fps reales;
//!  - retraso sobre el suelo: llegada_n - (mínima llegada_n - n/fps)... ver `lag`;
//!  - decodificación en el proceso (nativo: MJPEG->RGBA; ffmpeg: 0, lo hace el hijo);
//!  - CPU: segundos de CPU de este proceso (+ hijo ffmpeg) / segundos reales.
//! Salida JSON con --json.

use dkvm_video::{ffmpeg, stats, Latest, Mode, Source};
use std::time::{Duration, Instant};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let json = args.iter().any(|a| a == "--json");
    let mut mode = Mode::default();
    if let Some(r) = arg(&args, "--res") {
        if let Some((w, h)) = r.split_once('x') {
            mode.w = w.parse().unwrap_or(mode.w);
            mode.h = h.parse().unwrap_or(mode.h);
        }
    }
    if let Some(f) = arg(&args, "--fps") {
        mode.fps = f.parse().unwrap_or(mode.fps);
    }
    if let Some(f) = arg(&args, "--format") {
        mode.format = f;
    }
    let secs: f64 = arg(&args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(8.0);
    match cmd {
        "list" => {
            for d in dkvm_video::list_devices() {
                println!("nativo #{}: {} {}{}", d.index, d.name, if d.virtual_cam { "[virtual] " } else { "" }, if d.capture_card { "[capturadora]" } else { "" });
            }
            for d in ffmpeg::list() {
                println!("dshow  \"{}\" {}", d.name, if d.virtual_cam { "[virtual]" } else { "" });
            }
        }
        "formats" => {
            let i: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            match dkvm_video::native::formats(i) {
                Ok(v) => v.iter().for_each(|l| println!("{l}")),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1)
                }
            }
        }
        "native" => {
            let idx = arg(&args, "--idx").and_then(|s| s.parse().ok());
            let name = arg(&args, "--name");
            run_source(Source::Native { name, index: idx }, &mode, secs, json, "nokhwa");
        }
        "ffmpeg" => {
            let name = arg(&args, "--name").unwrap_or_else(|| "USB Video".into());
            run_source(Source::Ffmpeg { name }, &mode, secs, json, "ffmpeg-dshow");
        }
        "e2e" => e2e(&args, &mode),
        "backlog" => backlog(&mode, arg(&args, "--idx").and_then(|s| s.parse().ok()).unwrap_or(1)),
        "snap" | "lum" => snap_or_lum(cmd, &args, &mode, secs),
        "pattern" => run_source(Source::Pattern, &mode, secs, json, "patron"),
        "lavfi" => lavfi(&mode, secs, json),
        _ => eprintln!("uso: vidbench list | formats <idx> | native | ffmpeg --name N | lavfi | pattern  [--res WxH --fps F --format mjpeg|yuyv --secs S --json]"),
    }
}

struct Report {
    label: String,
    info: String,
    frames: u64,
    secs: f64,
    first_ms: f64,
    interval: stats::Summary,
    lag: stats::Summary,
    decode: stats::Summary,
    cpu_pct: f64,
    steady_fps: f64,
    dropped: u64,
    size: (u32, u32),
    error: Option<String>,
}

fn print_report(r: &Report, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({"label": r.label, "info": r.info, "frames": r.frames, "secs": r.secs, "fps": r.frames as f64 / r.secs, "steady_fps": r.steady_fps,
              "first_frame_ms": r.first_ms, "size": [r.size.0, r.size.1], "interval_ms": r.interval.json(), "lag_ms": r.lag.json(),
              "decode_ms": r.decode.json(), "cpu_pct_of_one_core": r.cpu_pct, "dropped": r.dropped, "error": r.error})
        );
        return;
    }
    println!("== {} ({})", r.label, r.info);
    if let Some(e) = &r.error {
        println!("   ERROR: {e}");
    }
    println!("   fps en régimen (sin arranque): {:.1}", r.steady_fps);
    println!("   cuadros={} en {:.2}s -> {:.1} fps (incluye arranque), tamaño {}x{}, primer cuadro tras {:.0} ms, descartados={}", r.frames, r.secs, r.frames as f64 / r.secs, r.size.0, r.size.1, r.first_ms, r.dropped);
    let f = |s: &stats::Summary| format!("min {:.2} p50 {:.2} p95 {:.2} p99 {:.2} max {:.2}", s.min, s.p50, s.p95, s.p99, s.max);
    println!("   intervalo ms : {}", f(&r.interval));
    println!("   lag s/suelo  : {}", f(&r.lag));
    println!("   decod. en proc ms: {}", f(&r.decode));
    println!("   CPU (proceso + hijo): {:.1}% de un núcleo", r.cpu_pct);
}

/// `lag`: cada llegada se compara con una rejilla ideal de periodo 1/fps (fps medidos) anclada en
/// la llegada más temprana respecto a la rejilla; mide cuánto se retrasan los cuadros (búfer/jitter)
/// respecto al mejor caso, no la latencia absoluta del dispositivo.
fn lag_from(arrivals: &[f64], period: f64) -> Vec<f64> {
    if arrivals.is_empty() {
        return vec![];
    }
    let off: Vec<f64> = arrivals.iter().enumerate().map(|(n, t)| t - n as f64 * period).collect();
    let floor = off.iter().cloned().fold(f64::INFINITY, f64::min);
    off.iter().map(|o| (o - floor) * 1e3).collect()
}

fn run_source(src: Source, mode: &Mode, secs: f64, json: bool, label: &str) {
    let latest = Latest::new();
    let cpu0 = stats::cpu_self();
    let t0 = Instant::now();
    let h = dkvm_video::start(&src, mode, latest.clone());
    let mut arrivals: Vec<f64> = Vec::new();
    let mut decode: Vec<f64> = Vec::new();
    let mut first_ms = f64::NAN;
    let mut size = (0, 0);
    let mut last_seq = None;
    let mut seq_gaps = 0u64;
    while t0.elapsed().as_secs_f64() < secs && latest.error().is_none() {
        if let Some(f) = latest.wait(Duration::from_millis(500)) {
            if first_ms.is_nan() {
                first_ms = t0.elapsed().as_secs_f64() * 1e3;
            }
            arrivals.push(f.arrived.duration_since(t0).as_secs_f64());
            decode.push(f.decode.as_secs_f64() * 1e3);
            size = (f.w, f.h);
            if let Some(l) = last_seq {
                seq_gaps += f.seq.saturating_sub(l + 1);
            }
            last_seq = Some(f.seq);
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    latest.stop();
    let _ = h.join();
    let cpu = stats::cpu_self() - cpu0 + if latest.child_cpu().is_nan() { 0.0 } else { latest.child_cpu() };
    // el productor nativo/ffmpeg publica cada cuadro; los saltos de seq son cuadros pisados antes de leerse
    let skip = (arrivals.len() / 10).min(30); // calentamiento
    let tail = &arrivals[skip.min(arrivals.len())..];
    let interval: Vec<f64> = tail.windows(2).map(|w| (w[1] - w[0]) * 1e3).collect();
    let fps = if tail.len() > 1 { (tail.len() - 1) as f64 / (tail[tail.len() - 1] - tail[0]) } else { 1.0 };
    let r = Report {
        label: label.into(),
        info: latest.info(),
        frames: arrivals.len() as u64,
        secs: elapsed,
        first_ms,
        interval: stats::summarize(interval),
        lag: stats::summarize(lag_from(tail, 1.0 / fps)),
        decode: stats::summarize(decode[skip.min(decode.len())..].to_vec()),
        cpu_pct: cpu / elapsed * 100.0,
        steady_fps: fps,
        dropped: seq_gaps,
        size,
        error: latest.error(),
    };
    print_report(&r, json);
}

fn lavfi(mode: &Mode, secs: f64, json: bool) {
    // Fuente sintética `-re` a fps fijos + conversión a RGBA y tubería rawvideo: mide el coste de la tubería
    // (proceso hijo, copia por pipe, lectura), sin dispositivo.
    let (w, h) = (mode.w as usize, mode.h as usize);
    let input: Vec<String> = ["-fflags", "nobuffer", "-flags", "low_delay", "-re", "-f", "lavfi", "-i", &format!("testsrc2=size={w}x{h}:rate={}", mode.fps)]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let cpu0 = stats::cpu_self();
    let t0 = Instant::now();
    let mut pipe = match ffmpeg::Pipe::spawn(&input, w, h) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    };
    let mut buf = vec![0u8; w * h * 4];
    let mut arrivals = Vec::new();
    while t0.elapsed().as_secs_f64() < secs {
        if pipe.read_frame(&mut buf).is_err() {
            break;
        }
        arrivals.push(t0.elapsed().as_secs_f64());
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let cpu = stats::cpu_self() - cpu0 + stats::cpu_child(&pipe.child);
    let skip = (arrivals.len() / 10).min(30);
    let tail = &arrivals[skip.min(arrivals.len())..];
    let interval: Vec<f64> = tail.windows(2).map(|x| (x[1] - x[0]) * 1e3).collect();
    let r = Report {
        label: "ffmpeg-lavfi".into(),
        info: format!("testsrc2 {w}x{h}@{} -> rgba pipe", mode.fps),
        frames: arrivals.len() as u64,
        secs: elapsed,
        first_ms: arrivals.first().copied().unwrap_or(f64::NAN) * 1e3,
        interval: stats::summarize(interval),
        lag: stats::summarize(lag_from(tail, 1.0 / mode.fps as f64)),
        decode: stats::summarize(vec![]),
        cpu_pct: cpu / elapsed * 100.0,
        steady_fps: if tail.len() > 1 { (tail.len() - 1) as f64 / (tail[tail.len() - 1] - tail[0]) } else { 0.0 },
        dropped: 0,
        size: (w as u32, h as u32),
        error: None,
    };
    print_report(&r, json);
}

fn source_from(args: &[String]) -> Source {
    match arg(args, "--backend").as_deref() {
        Some("ffmpeg") => Source::Ffmpeg { name: arg(args, "--name").unwrap_or_else(|| "USB Video".into()) },
        Some("pattern") => Source::Pattern,
        _ => Source::Native { name: arg(args, "--name"), index: arg(args, "--idx").and_then(|s| s.parse().ok()) },
    }
}

/// `snap --out f.png`: guarda un cuadro (tras 1 s de calentamiento) y muestra estadísticas de contenido.
/// `lum [--secs S]`: registra la luminancia media por cuadro (t en ms, desde el arranque) y marca los saltos
/// bruscos (> 25 niveles entre cuadros), para medir latencia contra un evento externo conocido.
fn snap_or_lum(cmd: &str, args: &[String], mode: &Mode, secs: f64) {
    let latest = Latest::new();
    let t0 = Instant::now();
    let h = dkvm_video::start(&source_from(args), mode, latest.clone());
    let mut prev: Option<f64> = None;
    // --cmd "..." : se lanza a los 2 s (p. ej. `dkvm key win`); el primer salto de luminancia posterior da la latencia
    // entrada->pantalla medida con el mismo reloj (incluye HID + USB + captura + decodificación).
    let trig = arg(args, "--cmd");
    let mut t_cmd: Option<(f64, f64)> = None;
    let mut first_jump: Option<f64> = None;
    let mut saved = false;
    let mut n = 0u64;
    while t0.elapsed().as_secs_f64() < if cmd == "snap" { 1.5 } else { secs } && latest.error().is_none() {
        if let (Some(c), None) = (&trig, t_cmd) {
            if t0.elapsed().as_secs_f64() > 2.0 {
                let a = t0.elapsed().as_secs_f64() * 1e3;
                let ok = std::process::Command::new("cmd").args(["/C", c]).stdout(std::process::Stdio::null()).status().is_ok();
                t_cmd = Some((a, t0.elapsed().as_secs_f64() * 1e3));
                println!("orden lanzada en t={a:.1} ms, terminó en t={:.1} ms (ok={ok})", t_cmd.unwrap().1);
            }
        }
        let Some(f) = latest.wait(Duration::from_millis(500)) else { continue };
        n += 1;
        // luminancia media sobre una rejilla de píxeles (barato)
        let (mut sum, mut cnt) = (0f64, 0f64);
        let mut i = 0;
        while i < f.rgba.len() {
            sum += 0.299 * f.rgba[i] as f64 + 0.587 * f.rgba[i + 1] as f64 + 0.114 * f.rgba[i + 2] as f64;
            cnt += 1.0;
            i += 4 * 97;
        }
        let l = sum / cnt;
        let t = f.arrived.duration_since(t0).as_secs_f64() * 1e3;
        if cmd == "lum" && prev.map(|p| (l - p).abs() > 25.0).unwrap_or(false) {
            println!("SALTO t={t:.1} ms  lum {:.0} -> {l:.0}  (cuadro {n})", prev.unwrap());
            if let (Some((a, _)), None) = (t_cmd, first_jump) {
                if t > a {
                    first_jump = Some(t);
                    println!("LATENCIA orden->pantalla: {:.1} ms desde el lanzamiento", t - a);
                }
            }
        }
        prev = Some(l);
        if cmd == "snap" && !saved && t0.elapsed().as_secs_f64() > 1.0 {
            let out = arg(args, "--out").unwrap_or_else(|| "snap.png".into());
            if let Some(img) = image::RgbaImage::from_raw(f.w, f.h, f.rgba.clone()) {
                let mut rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
                for p in rgb.pixels_mut() { let _ = p; }
                rgb.save(&out).expect("guardar png");
            }
            println!("guardado {out} ({}x{}), luminancia media {l:.1}", f.w, f.h);
            saved = true;
        }
    }
    latest.stop();
    let _ = h.join();
    if let Some(e) = latest.error() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
    if cmd == "lum" {
        println!("{n} cuadros en {secs} s");
    }
}

/// Profundidad de la cola de Media Foundation: el consumidor se para `stall` ms y luego lee cuadros seguidos.
/// Los que salen inmediatamente (< 40 % del periodo) estaban ya en cola, es decir, tienen esa edad extra.
/// Cada cuadro en cola = un periodo de latencia añadida en el uso normal solo si el consumidor va por detrás;
/// con consumidor al día la cola está vacía. Esto mide cuánto búfer existe, no la latencia de cristal a cristal.
fn backlog(mode: &Mode, idx: usize) {
    use nokhwa::pixel_format::RgbAFormat;
    use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};
    nokhwa::nokhwa_initialize(|_| {});
    let want = CameraFormat::new(Resolution::new(mode.w, mode.h), dkvm_video::native::frame_format(mode), mode.fps);
    let req = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(want));
    let mut cam = nokhwa::Camera::new(CameraIndex::Index(idx as u32), req).expect("abrir cámara");
    cam.open_stream().expect("iniciar captura");
    let f = cam.camera_format();
    let period = 1000.0 / mode.fps.max(1) as f64; // nokhwa informa frame_rate=1 con MJPEG: se usa el pedido
    println!("backlog: {:?} {}x{}@{} (periodo {:.1} ms)", f.format(), f.width(), f.height(), f.frame_rate(), period);
    let _ = FrameFormat::MJPEG;
    for stall in [0u64, 100, 300, 600] {
        for _ in 0..10 {
            let _ = cam.frame_raw(); // calentar / vaciar
        }
        std::thread::sleep(Duration::from_millis(stall));
        let mut gaps = Vec::new();
        let mut last = Instant::now();
        for _ in 0..25 {
            let _ = cam.frame_raw();
            gaps.push(last.elapsed().as_secs_f64() * 1e3);
            last = Instant::now();
        }
        let queued = gaps.iter().filter(|g| **g < period * 0.4).count();
        println!("  parada {stall:>3} ms: {queued:>2} cuadros salieron al instante (en cola) de 25; huecos ms: {}", gaps.iter().take(12).map(|g| format!("{g:.0}")).collect::<Vec<_>>().join(" "));
    }
    let _ = cam.stop_stream();
}

/// Cuadrícula 96x54 de luminancia (muestreo central de cada celda): barata y suficiente para detectar un cambio de escena.
fn grid(f: &dkvm_video::Frame) -> Vec<u8> {
    let (gw, gh) = (96usize, 54usize);
    let (w, h) = (f.w as usize, f.h as usize);
    let mut g = Vec::with_capacity(gw * gh);
    for gy in 0..gh {
        let y = ((gy * 2 + 1) * h / (gh * 2)).min(h - 1);
        for gx in 0..gw {
            let x = ((gx * 2 + 1) * w / (gw * 2)).min(w - 1);
            let i = (y * w + x) * 4;
            g.push((0.299 * f.rgba[i] as f32 + 0.587 * f.rgba[i + 1] as f32 + 0.114 * f.rgba[i + 2] as f32) as u8);
        }
    }
    g
}

fn gdiff(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as f64).sum::<f64>() / a.len().max(1) as f64
}

/// Latencia entrada->pantalla con el enlace BLE ya abierto (sin pagar la conexión en la medición).
/// Cada prueba: `key` abajo, 40 ms, y se mide desde justo antes de SOLTARLA (GNOME abre/cierra el Activities al
/// soltar Win) hasta el primer cuadro que difiere del anterior en más de `--thr` niveles medios (0-255).
/// Reporta: retorno del write BLE (el flush vuelve cuando la pila de Windows acepta el paquete), llegada del
/// cuadro a este proceso y llegada + decodificación. NO incluye la presentación de la GUI (unos ms más).
fn e2e(args: &[String], mode: &Mode) {
    use dkvm_core::input::Kvm;
    use std::sync::{mpsc, Arc, Mutex};
    let thr: f64 = arg(args, "--thr").and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let n: usize = arg(args, "--n").and_then(|s| s.parse().ok()).unwrap_or(6);
    let key = arg(args, "--key").unwrap_or_else(|| "win".into());
    let bto: u64 = arg(args, "--ble-timeout").and_then(|s| s.parse().ok()).unwrap_or(15);
    let t_open = Instant::now();
    let t = match dkvm_core::transport::select::open_ble(Duration::from_secs(bto)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("BLE: {}", e.message());
            std::process::exit(3)
        }
    };
    println!("BLE conectado en {:.0} ms", t_open.elapsed().as_secs_f64() * 1e3);
    let mut kvm = Kvm::new(Box::new(t), "us").expect("kvm");
    for _ in 0..3 {
        let _ = kvm.raw("status");
    }
    let latest = Latest::new();
    let _h = dkvm_video::start(&source_from(args), mode, latest.clone());
    let state: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None)); // t0 de la prueba en curso
    let (rtx, rrx) = mpsc::channel::<(f64, f64, f64)>(); // (llegada ms, llegada+dec ms, diff máx)
    let (wtx, wrx) = mpsc::channel::<f64>();
    let st2 = state.clone();
    let tl = latest.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500)); // calentamiento de vídeo
        let mut out = Vec::new();
        for i in 0..n {
            let _ = kvm.key_down_name(&key);
            std::thread::sleep(Duration::from_millis(40));
            let t0 = Instant::now();
            *st2.lock().unwrap() = Some(t0);
            let r = kvm.key_up_name(&key);
            let w = t0.elapsed().as_secs_f64() * 1e3;
            let _ = wtx.send(w);
            let det = rrx.recv_timeout(Duration::from_millis(2500)).ok();
            println!("  prueba {}: write {:.1} ms{}  {}", i + 1, w, if r.is_err() { " (ERROR)" } else { "" },
                det.map(|d| format!("llegada {:.1} ms, +dec {:.1} ms (diff {:.1})", d.0, d.1, d.2)).unwrap_or_else(|| "SIN CAMBIO detectado (¿umbral alto? ¿host sin respuesta?)".into()));
            out.push((w, det));
            *st2.lock().unwrap() = None;
            std::thread::sleep(Duration::from_millis(1800)); // la animación termina y el baseline se reasienta
        }
        if n % 2 == 1 {
            let _ = kvm.key_down_name(&key);
            std::thread::sleep(Duration::from_millis(40));
            let _ = kvm.key_up_name(&key);
        }
        let _ = kvm.release_all();
        tl.stop();
        out
    });
    let mut base: Option<Vec<u8>> = None;
    let mut maxd = 0f64;
    let mut done_for: Option<Instant> = None;
    let mut frames = 0u64;
    let mut last_arr: Option<Instant> = None;
    let t_start = Instant::now();
    while !latest.stopped() {
        let Some(f) = latest.wait(Duration::from_millis(200)) else { continue };
        frames += 1;
        if let Some(l) = last_arr {
            let gap = f.arrived.duration_since(l).as_secs_f64() * 1e3;
            if gap > 80.0 {
                println!("  [hueco de vídeo {gap:.0} ms en t={:.1} s]", t_start.elapsed().as_secs_f64());
            }
        }
        last_arr = Some(f.arrived);
        let g = grid(&f);
        let t0 = *state.lock().unwrap();
        match t0 {
            None => {
                base = Some(g);
                maxd = 0.0;
                done_for = None;
            }
            Some(t0) => {
                if let Some(b) = &base {
                    let d = gdiff(&g, b);
                    maxd = maxd.max(d);
                    if f.arrived > t0 && d > thr && done_for != Some(t0) {
                        done_for = Some(t0);
                        let a = f.arrived.duration_since(t0).as_secs_f64() * 1e3;
                        let _ = rtx.send((a, a + f.decode.as_secs_f64() * 1e3, d));
                    }
                }
            }
        }
    }
    let out = worker.join().unwrap_or_default();
    let secs = t_start.elapsed().as_secs_f64();
    println!("cuadros recibidos: {frames} en {secs:.1} s = {:.1} fps (el hilo de pruebas no bloquea el bucle de vídeo)", frames as f64 / secs);
    let _ = wrx;
    let arr: Vec<f64> = out.iter().filter_map(|(_, d)| d.map(|d| d.0)).collect();
    let mut dec: Vec<f64> = out.iter().filter_map(|(_, d)| d.map(|d| d.1)).collect();
    let mut wr: Vec<f64> = out.iter().map(|(w, _)| *w).collect();
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() { f64::NAN } else { v[v.len() / 2] }
    };
    println!("RESUMEN ({}/{} detectadas, umbral {thr}): write p50 {:.1} ms | cuadro llegado p50 {:.1} ms (min {:.1}, max {:.1}) | +decodificación p50 {:.1} ms", arr.len(), out.len(), med(&mut wr), med(&mut arr.clone()), arr.iter().cloned().fold(f64::INFINITY, f64::min), arr.iter().cloned().fold(0.0, f64::max), med(&mut dec));
    let ws = dkvm_core::latency::snapshot();
    println!("BLE salida ({} escrituras): encolado->write medio {:.2} ms (máx {:.2}); llamada write media {:.2} ms (máx {:.2})", ws.writes, ws.queue_avg_ms, ws.queue_max_ms, ws.write_avg_ms, ws.write_max_ms);
    let _ = (base, maxd);
}
