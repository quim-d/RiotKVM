//! CLI de riotkvm. Códigos de salida: 0 ok, 1 error, 2 uso, 3 dongle no encontrado,
//! 4 el dongle dijo `err`, 5 fallo de E/S/timeout.
//!
//! `run()` recibe el entorno (stdin/stdout/stderr, fábrica de transporte, listado de puertos)
//! para poder probar la CLI entera en proceso, sin hardware.

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use riotkvm_core::input::{parse_screen, Kvm};
use riotkvm_core::script::{do_move, run_script, Context};
use riotkvm_core::transport::select::Choice;
use riotkvm_core::transport::{list_ports, PortInfo};
use riotkvm_core::{Error, MockTransport, Result, Transport};
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod tools;

pub type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
pub type TransportFactory = Box<dyn Fn() -> Result<Box<dyn Transport>>>;

pub struct Env {
    pub stdin: Box<dyn Read>,
    pub stdout: SharedWriter,
    pub stderr: SharedWriter,
    /// Sustituye al transporte real (pruebas). Tiene prioridad sobre `--port`.
    pub transport_factory: Option<TransportFactory>,
    pub list_ports: Box<dyn Fn() -> Vec<PortInfo>>,
}

impl Env {
    pub fn real() -> Env {
        let w = |b: Box<dyn Write + Send>| -> SharedWriter { Arc::new(Mutex::new(b)) };
        Env {
            stdin: Box::new(std::io::stdin()),
            stdout: w(Box::new(std::io::stdout())),
            stderr: w(Box::new(std::io::stderr())),
            transport_factory: None,
            list_ports: Box::new(list_ports),
        }
    }
}

fn put(w: &SharedWriter, s: &str) {
    let mut g = w.lock().unwrap();
    let _ = g.write_all(s.as_bytes());
    let _ = g.write_all(b"\n");
    let _ = g.flush();
}

#[derive(Parser, Debug)]
#[command(name = "riotkvm", version, about = "riotkvm: controla el dongle KVM (teclado y ratón del servidor).", long_about = None)]
pub struct Cli {
    /// Puerto serie (COM5, /dev/ttyACM0), 'ble', 'mock' o 'ble-mock' (dongle BLE simulado).
    /// Por defecto: BLE si hay un dongle anunciando; si no, serie (autodetecta VID 1209:0001).
    #[arg(short, long, global = true, env = "RIOTKVM_PORT")]
    port: Option<String>,
    /// Fuerza BLE (equivale a -p ble). Con `scan`, escanea por BLE.
    #[arg(long, global = true)]
    ble: bool,
    /// Fuerza serie (sin probar BLE).
    #[arg(long, global = true, conflicts_with = "ble")]
    serial: bool,
    /// Espera máxima para conectar por BLE (s).
    #[arg(long, global = true, default_value_t = 8.0, value_name = "SEG")]
    ble_timeout: f64,
    /// Distribución de teclado del equipo DESTINO (es, us, gb, de, fr, it, pt, latam). Por defecto la guardada por la GUI (`%APPDATA%\riotkvm\layout.txt`), si no `es`.
    #[arg(short, long, global = true, env = "RIOTKVM_LAYOUT")]
    layout: Option<String>,
    /// Resolución del destino ANCHOxALTO: las coordenadas absolutas pasan a ser píxeles.
    #[arg(long, global = true, env = "RIOTKVM_SCREEN")]
    screen: Option<String>,
    /// Salida JSON en stdout (errores incluidos).
    #[arg(long = "json", global = true)]
    json: bool,
    /// Vuelca las órdenes enviadas a stderr.
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Guarda en FILE los timestamps de cada orden (CSV: cola, escritura, respuesta, en µs) y
    /// un resumen en stderr.
    #[arg(long, global = true, value_name = "FILE")]
    trace: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Lista los puertos serie y marca el dongle (VID 1209:0001). Con --ble escanea dispositivos BLE.
    Scan {
        /// (con --ble) lista TODO lo que anuncia, no solo el servicio del dongle.
        #[arg(long)]
        all: bool,
        /// (con --ble) segundos de escaneo.
        #[arg(long, default_value_t = 3.0)]
        secs: f64,
    },
    /// Ping BLE con secuencia (RTT de enlace); en serie mide `status`.
    Ping {
        #[arg(short = 'n', long, default_value_t = 10)]
        count: usize,
    },
    /// Estado del dongle: host USB visto, LEDs del teclado, HID listo.
    Status,
    /// Versión y tiempos de arranque del firmware (orden `info`).
    Info,
    /// Contadores de latencia del firmware (orden `stats`; con --reset los pone a cero).
    Stats {
        #[arg(long)]
        reset: bool,
    },
    /// Escribe TEXTO en el servidor. Sin TEXTO o con '-', lee stdin.
    Type {
        text: Option<String>,
        /// Pausa entre caracteres (s).
        #[arg(long, default_value_t = 0.0)]
        delay: f64,
        /// Pulsa Enter al final.
        #[arg(long)]
        enter: bool,
        /// Usa 'type' del firmware (layout US fijo) en vez de kd/ku con el layout.
        #[arg(long)]
        fw: bool,
    },
    /// Pulsa combinaciones: riotkvm key ctrl+alt+del | riotkvm key win+r | riotkvm key f5 enter
    Key {
        #[arg(required = true)]
        combos: Vec<String>,
        /// Segundos que se mantiene cada combinación.
        #[arg(long, default_value_t = 0.0)]
        hold: f64,
        /// Solo pulsar (no soltar). Ojo: el dongle suelta todo al cerrar el puerto (fin del proceso);
        /// para mantener teclas usa una macro (`riotkvm run`) con keydown/sleep/keyup.
        #[arg(long)]
        down: bool,
        /// Solo soltar.
        #[arg(long)]
        up: bool,
    },
    /// Suelta todas las teclas y botones (por si algo se quedó pulsado).
    Release,
    /// Mueve el ratón. Absoluto: X Y en 0..32767 (o píxeles con --screen). Relativo: dx dy.
    Mouse {
        #[arg(allow_negative_numbers = true)]
        x: f64,
        #[arg(allow_negative_numbers = true)]
        y: f64,
        /// Movimiento relativo (dx dy) en vez de posición absoluta.
        #[arg(long = "rel")]
        relative: bool,
    },
    /// Clic: riotkvm click [left|right|middle] [-n 2] [--at X Y]
    Click {
        #[arg(default_value = "left", value_parser = ["left", "right", "middle"])]
        button: String,
        #[arg(short = 'n', long, default_value_t = 1)]
        count: i64,
        /// Mueve a X Y (absoluto) antes de hacer clic.
        #[arg(long, num_args = 2, value_names = ["X", "Y"], allow_negative_numbers = true)]
        at: Option<Vec<f64>>,
        /// Usa los botones del ratón relativo.
        #[arg(long = "rel")]
        relative: bool,
        /// Segundos entre clics múltiples.
        #[arg(long, default_value_t = 0.05)]
        interval: f64,
    },
    /// Rueda del ratón: positivo = arriba.
    Scroll {
        #[arg(allow_negative_numbers = true)]
        amount: i64,
        #[arg(long = "rel")]
        relative: bool,
    },
    /// Envía una orden cruda del protocolo del dongle (depuración): riotkvm raw kd 0x04
    Raw {
        #[arg(required = true, allow_hyphen_values = true)]
        line: Vec<String>,
    },
    /// Ejecuta una macro (fichero o '-' para stdin). Formato: ver docs del módulo script.
    Run {
        script: String,
        /// No toca el dongle: valida y muestra las órdenes que enviaría.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "abs", value_parser = ["abs", "rel"])]
        mouse_mode: String,
    },
    /// Mide la latencia de ida y vuelta (status) y el caudal de órdenes en ráfaga. Con el
    /// dongle real, solo con el bloqueo del dispositivo; no inyecta teclas ni ratón.
    Bench {
        /// Número de órdenes `status` en serie (ida y vuelta).
        #[arg(short = 'n', long, default_value_t = 100)]
        count: usize,
    },
    /// Imprime el script de autocompletado. Ej.: eval "$(riotkvm completion bash)"
    Completion { shell: Shell },
    /// Firmware del dongle: `riotkvm fw check` | `riotkvm fw flash [--file X] [--port P] [--dry-run]`
    Fw {
        #[command(subcommand)]
        cmd: tools::FwCmd,
    },
    /// microSD con Ventoy: `riotkvm sd list` | `riotkvm sd ventoy --disk diskN [--yes]`
    Sd {
        #[command(subcommand)]
        cmd: tools::SdCmd,
    },
}

struct App<'a> {
    cli: &'a Cli,
    env: &'a mut Env,
    screen: Option<(i64, i64)>,
}

impl App<'_> {
    fn make_transport(&self, force_mock: Option<bool>) -> Result<Box<dyn Transport>> {
        if let Some(f) = &self.env.transport_factory {
            return f();
        }
        let echo = {
            let e = self.env.stderr.clone();
            let out = self.env.stdout.clone();
            let to_out = force_mock == Some(true) && !self.cli.json;
            move |l: &str| put(if to_out { &out } else { &e }, &format!("> {l}"))
        };
        if force_mock.is_some() || self.cli.port.as_deref() == Some("mock") {
            return Ok(Box::new(MockTransport::new().with_echo(echo)));
        }
        let choice = if self.cli.serial { Choice::Serial(self.cli.port.clone()) } else { Choice::parse(self.cli.port.as_deref(), self.cli.ble) };
        riotkvm_core::transport::select::open(&choice, Duration::from_secs_f64(self.cli.ble_timeout))
    }

    fn emit(&mut self, data: Value, text: Option<String>) {
        if self.cli.json {
            let mut m = Map::new();
            m.insert("ok".into(), Value::Bool(true));
            if let Value::Object(o) = data {
                m.extend(o);
            }
            put(&self.env.stdout, &Value::Object(m).to_string());
        } else if let Some(t) = text {
            put(&self.env.stdout, &t);
        }
    }

    /// Abre el transporte, ejecuta y cierra (el flush final solo propaga error si el cuerpo fue bien).
    fn with_kvm<T>(&mut self, f: impl FnOnce(&mut App, &mut Kvm) -> Result<T>) -> Result<T> {
        let t = self.make_transport(None)?;
        let mut kvm = Kvm::new(t, &riotkvm_core::config::effective_layout(self.cli.layout.as_deref()))?;
        let r = f(self, &mut kvm);
        let fl = kvm.t.flush();
        let trace = kvm.t.trace();
        let cl = kvm.t.close();
        if let (Some(path), Some(tr)) = (&self.cli.trace, trace) {
            if let Err(e) = std::fs::write(path, tr.to_csv()) {
                put(&self.env.stderr, &format!("riotkvm: no se pudo escribir {path}: {e}"));
            }
            let s = serde_json::to_string(&tr.summary()).unwrap_or_default();
            put(&self.env.stderr, &format!("trace: {s}"));
        }
        let v = r?;
        fl?;
        cl?;
        Ok(v)
    }
}

fn status_text(s: &Map<String, Value>) -> String {
    s.iter()
        .map(|(k, v)| match v {
            Value::Bool(b) => format!("{k}={}", *b as i32),
            Value::String(x) => format!("{k}={x}"),
            o => format!("{k}={o}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn usage_err<T>(m: &str) -> Result<T> {
    Err(Error::Usage(m.into()))
}

fn dispatch(app: &mut App) -> Result<i32> {
    let cli = app.cli;
    match &cli.cmd {
        Cmd::Scan { all, secs } if cli.ble => {
            let seen = riotkvm_core::transport::ble::scan_devices(Duration::from_secs_f64(*secs), *all)?;
            let found = seen.iter().any(|s| s.ours);
            if cli.json {
                app.emit(json!({ "devices": seen }), None);
            } else {
                for d in &seen {
                    put(
                        &app.env.stdout,
                        &format!("{} {:<40} {:>5}  {}", if d.ours { '*' } else { ' ' }, d.id, d.rssi.map(|r| format!("{r} dBm")).unwrap_or_else(|| "?".into()), d.name.clone().unwrap_or_default()),
                    );
                }
                if !found {
                    put(&app.env.stderr, "(ningún dongle anunciando el servicio BLE del KVM)");
                }
            }
            Ok(if found { 0 } else { 3 })
        }
        Cmd::Ping { count } => {
            app.with_kvm(|app, kvm| {
                let mut rtt: Vec<u64> = Vec::new();
                for _ in 0..*count {
                    rtt.push(kvm.t.ping()?.as_micros() as u64);
                }
                let st = riotkvm_core::transport::trace::Stats::from(rtt);
                let line = format!("ping x{} ({}): rtt us min={} p50={} p95={} max={}", st.n, kvm.t.name(), st.min, st.p50, st.p95, st.max);
                app.emit(json!({ "via": kvm.t.name(), "rtt_us": st }), Some(line));
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Scan { .. } => {
            let ports = (app.env.list_ports)();
            let found = ports.iter().any(|p| p.is_dongle);
            if cli.json {
                app.emit(json!({ "ports": ports }), None);
            } else {
                for p in &ports {
                    let mark = if p.is_dongle { '*' } else { ' ' };
                    let vp = match (p.vid, p.pid) {
                        (Some(v), Some(i)) => format!("{v:04X}:{i:04X}"),
                        _ => "----:----".into(),
                    };
                    put(&app.env.stdout, &format!("{mark} {:<10} {vp}  {}", p.port, p.description));
                }
                if !found {
                    put(&app.env.stderr, "(ningún dongle detectado)");
                }
            }
            Ok(if found { 0 } else { 3 })
        }
        Cmd::Status => {
            app.with_kvm(|app, kvm| {
                let s = kvm.status()?;
                let text = status_text(&s);
                app.emit(Value::Object(s), Some(text));
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Info | Cmd::Stats { .. } => {
            app.with_kvm(|app, kvm| {
                let r = match &cli.cmd {
                    Cmd::Stats { reset } => kvm.stats(*reset)?,
                    _ => kvm.info()?,
                };
                app.emit(json!({ "reply": r }), Some(r.clone()));
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Type { text, delay, enter, fw } => {
            app.with_kvm(|app, kvm| {
                let mut s = match text.as_deref() {
                    None | Some("-") => {
                        let mut b = Vec::new();
                        app.env.stdin.read_to_end(&mut b).map_err(|e| Error::Transport(format!("stdin: {e}")))?;
                        String::from_utf8_lossy(&b).into_owned()
                    }
                    Some(t) => t.to_string(),
                };
                if *enter {
                    s.push('\n');
                }
                if *fw {
                    kvm.type_text_firmware(&s)?;
                } else {
                    kvm.type_text(&s, *delay)?;
                }
                app.emit(json!({ "typed": s.chars().count() }), None);
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Key { combos, hold, down, up } => {
            app.with_kvm(|app, kvm| {
                if *down && *up {
                    return usage_err("--down y --up son excluyentes");
                }
                for c in combos {
                    if *down {
                        kvm.key_down_name(c)?;
                    } else if *up {
                        kvm.key_up_name(c)?;
                    } else {
                        kvm.press(c, *hold)?;
                    }
                }
                app.emit(json!({ "keys": combos }), None);
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Release => {
            app.with_kvm(|app, kvm| {
                kvm.release_all()?;
                app.emit(json!({ "released": true }), None);
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Mouse { x, y, relative } => {
            let screen = app.screen;
            app.with_kvm(|app, kvm| {
                do_move(kvm, *x, *y, *relative, screen)?;
                app.emit(json!({ "x": x, "y": y, "relative": relative }), None);
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Click { button, count, at, relative, interval } => {
            let screen = app.screen;
            app.with_kvm(|app, kvm| {
                if let Some(a) = at {
                    do_move(kvm, a[0], a[1], false, screen)?;
                }
                kvm.click(button, *count, *relative, *interval)?;
                app.emit(json!({ "button": button, "count": count }), None);
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Scroll { amount, relative } => {
            app.with_kvm(|app, kvm| {
                kvm.scroll(*amount, *relative)?;
                app.emit(json!({ "scroll": amount }), None);
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Raw { line } => {
            app.with_kvm(|app, kvm| {
                let reply = kvm.raw(&line.join(" "))?;
                app.emit(json!({ "reply": reply }), Some(reply.clone()));
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Run { script, dry_run, mouse_mode } => {
            let text = if script == "-" {
                let mut b = Vec::new();
                app.env.stdin.read_to_end(&mut b).map_err(|e| Error::Usage(format!("stdin: {e}")))?;
                String::from_utf8_lossy(&b).into_owned()
            } else {
                std::fs::read(script)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .map_err(|e| Error::Usage(format!("no se pudo leer el script {script:?}: {e}")))?
            };
            let screen = app.screen;
            let t = app.make_transport(if *dry_run { Some(true) } else { None })?;
            let mut kvm = Kvm::new(t, &riotkvm_core::config::effective_layout(cli.layout.as_deref()))?;
            let results;
            let r = {
                let mut ctx = Context::new(&mut kvm);
                ctx.relative = mouse_mode == "rel";
                ctx.screen = screen;
                let err = app.env.stderr.clone();
                let mut step = move |n: usize, tok: &[String]| put(&err, &format!("# {n}: {}", tok.join(" ")));
                let r = run_script(&mut ctx, &text, if cli.verbose { Some(&mut step) } else { None });
                results = std::mem::take(&mut ctx.results);
                r
            };
            let fl = kvm.t.flush();
            let _ = kvm.t.close();
            r?;
            fl?;
            let list: Vec<Value> = results.iter().map(|(c, v)| json!({ "cmd": c, "value": v })).collect();
            let txt = results
                .iter()
                .map(|(c, v)| match v {
                    Value::String(s) => format!("{c}: {s}"),
                    Value::Object(m) => format!("{c}: {}", status_text(m)),
                    o => format!("{c}: {o}"),
                })
                .collect::<Vec<_>>()
                .join("\n");
            app.emit(json!({ "results": list }), if txt.is_empty() { None } else { Some(txt) });
            Ok(0)
        }
        Cmd::Bench { count } => {
            app.with_kvm(|app, kvm| {
                let mut rtt: Vec<u64> = Vec::with_capacity(*count);
                let t0 = Instant::now();
                for _ in 0..*count {
                    let t = Instant::now();
                    kvm.raw("status")?;
                    rtt.push(t.elapsed().as_micros() as u64);
                }
                let total = t0.elapsed();
                let st = riotkvm_core::transport::trace::Stats::from(rtt);
                let line = format!(
                    "status x{}: rtt us min={} p50={} p95={} p99={} max={}  ({:.0} ord/s en serie)",
                    st.n,
                    st.min,
                    st.p50,
                    st.p95,
                    st.p99,
                    st.max,
                    *count as f64 / total.as_secs_f64().max(1e-9)
                );
                app.emit(json!({ "rtt_us": st, "elapsed_s": total.as_secs_f64() }), Some(line));
                Ok(())
            })?;
            Ok(0)
        }
        Cmd::Fw { cmd } => tools::fw(app, cmd),
        Cmd::Sd { cmd } => tools::sd(app, cmd),
        Cmd::Completion { shell } => {
            use clap::CommandFactory;
            let mut buf: Vec<u8> = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "riotkvm", &mut buf);
            put(&app.env.stdout, String::from_utf8_lossy(&buf).trim_end());
            Ok(0)
        }
    }
}

/// Ejecuta la CLI y devuelve el código de salida.
pub fn run<I, T>(args: I, env: &mut Env) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let code = e.exit_code();
            let msg = e.render().to_string();
            put(if e.use_stderr() { &env.stderr } else { &env.stdout }, msg.trim_end());
            return code;
        }
    };
    let screen = match cli.screen.as_deref().map(parse_screen).transpose() {
        Ok(s) => s,
        Err(e) => {
            put(&env.stderr, &format!("error: {e}"));
            return 2;
        }
    };
    let json = cli.json;
    let mut app = App { cli: &cli, env, screen };
    match dispatch(&mut app) {
        Ok(code) => code,
        Err(e) => {
            if json {
                put(&app.env.stdout, &json!({ "ok": false, "error": e.message(), "code": e.exit_code() }).to_string());
            } else {
                put(&app.env.stderr, &format!("riotkvm: error: {e}"));
            }
            e.exit_code()
        }
    }
}
