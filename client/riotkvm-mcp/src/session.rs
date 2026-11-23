//! Estado persistente entre llamadas `execute`: conexión con el dongle (BLE o serie), la capturadora, el
//! espacio de coordenadas de la última captura enviada al modelo y el sistema del servidor (camino de los clics).
//!
//! Todo lo que el modelo pide pasa por `call()`, que comprueba el plazo (`timeoutSeconds`) y la cancelación del
//! cliente; las operaciones largas (tecleo, esperas, teclas mantenidas) se trocean para volver a comprobarlos.

use base64::Engine;
use riotkvm_core::error::Error as KErr;
use riotkvm_core::input::Kvm;
use riotkvm_core::session::{abs_buttons_env_override, TargetOs};
use riotkvm_core::transport::select::{self, Choice};
use riotkvm_video::{Latest, Mode, Source};
use image::{imageops::FilterType, RgbaImage};
use serde_json::{json, Map, Value};
use std::fs::File;
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Shot {
    pub mime: &'static str,
    pub data_b64: String,
}

struct Video {
    latest: Arc<Latest>,
    last_use: Arc<Mutex<Instant>>,
    desc: String,
}

/// Límites de lo que el modelo puede pedir en una sola orden (S1).
pub const MAX_SCROLL: i64 = 1000;
pub const MAX_REL: i64 = 10_000;
pub const MAX_TYPE_CHARS: usize = 8192;
const TYPE_CHUNK: usize = 32;
/// Log de `console.*` por ejecución (S2).
pub const LOG_MAX: usize = 64 * 1024;
/// Capturas por respuesta: número y presupuesto total en base64 (P4).
pub const MAX_SHOTS: usize = 6;
pub const SHOTS_BUDGET: usize = 4 * 1024 * 1024;
/// Ancho máximo de captura (P3): más ancho no aporta al modelo y solo gasta contexto.
pub const MAX_WIDTH: f64 = 1568.0;
/// Capturadora apagada tras estos segundos sin capturas (P1).
const VIDEO_IDLE_DEFAULT: u64 = 20;

/// Órdenes CDC/BLE crudas que `raw()` deja pasar (S4). Por BLE solo existen `kd ku kall mm ma mbr mba status info type`;
/// `stats`, `heap` y `log` son diagnósticos de solo lectura que únicamente responden por serie.
pub const RAW_ALLOWED: [&str; 13] = ["kd", "ku", "kall", "mm", "ma", "mbr", "mba", "status", "info", "type", "stats", "heap", "log"];

/// Variable `RIOTKVM_MCP_<name>`, o la vieja `DKVM_MCP_<name>` como alias.
pub fn env(name: &str) -> Option<String> {
    std::env::var(format!("RIOTKVM_MCP_{name}")).ok().or_else(|| std::env::var(format!("DKVM_MCP_{name}")).ok()).filter(|v| !v.is_empty())
}

/// Fichero de bloqueo: una sola instancia de `riotkvm-mcp` usa el dongle a la vez (S3).
pub fn lock_path() -> PathBuf {
    std::env::temp_dir().join("riotkvm-mcp.lock")
}

/// Toma el bloqueo (sin esperar). Mientras viva el `File`, ninguna otra instancia puede tenerlo.
pub fn acquire_lock(path: &Path) -> Result<File, String> {
    let mut f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|e| format!("[bloqueo] no se pudo abrir {}: {e}", path.display()))?;
    match f.try_lock() {
        Ok(()) => {
            let _ = f.set_len(0);
            let _ = f.rewind();
            let _ = write!(f, "{}", std::process::id());
            let _ = f.flush();
            Ok(f)
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = f.read_to_string(&mut pid);
            let pid = pid.trim();
            let who = if pid.is_empty() { String::new() } else { format!(" (pid {pid})") };
            Err(format!(
                "[ocupado] otra instancia de riotkvm-mcp{who} está usando el dongle; lo suelta sola tras 60 s sin peticiones. Si es otra sesión del LLM, espera o ciérrala"
            ))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("[bloqueo] {}: {e}", path.display())),
    }
}

pub struct Session {
    port: Option<String>,
    ble_timeout: f64,
    layout: String,
    kvm: Option<Kvm>,
    lock: Option<File>,
    link: String,
    /// Nombre del dongle conectado (leído de `info` al conectar; `None` si su firmware no lo publica). Solo lectura:
    /// renombrar NO se expone al LLM (la orden `name` tampoco está en `RAW_ALLOWED`).
    name: Option<String>,
    video: Option<Video>,
    video_idle: Duration,
    /// Tamaño de la última captura ENVIADA al modelo: las coordenadas de `mouse.move` están en este espacio.
    view: Option<(u32, u32)>,
    /// Tamaño real del último cuadro de la capturadora.
    frame: Option<(u32, u32)>,
    /// ¿Hemos fijado nosotros la posición absoluta? Si no, la rueda no debe ir por el absoluto (movería al centro).
    abs_known: bool,
    /// Sistema del servidor (como el ajuste "Sistema del servidor" de la GUI): None = manual.
    target: Option<TargetOs>,
    /// Clics y rueda del ratón absoluto por el ratón relativo (Windows/Linux/Fedora) o por el absoluto (macOS).
    abs_via_rel: bool,
    pub deadline: Instant,
    pub cancel: Arc<AtomicBool>,
    pub shots: Vec<Shot>,
    shots_bytes: usize,
    pub logs: Vec<String>,
    logs_bytes: usize,
    logs_cut: bool,
}

fn kerr(e: KErr) -> String {
    let kind = match e {
        KErr::Usage(_) => "uso",
        KErr::DeviceNotFound(_) => "sin-dongle",
        KErr::Device(_) => "dongle",
        KErr::Transport(_) => "enlace",
    };
    format!("[{kind}] {}", e.message())
}

fn f(a: &Value, k: &str) -> Option<f64> {
    a.get(k).and_then(Value::as_f64)
}
fn s<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}
fn b(a: &Value, k: &str) -> bool {
    a.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// Segundos -> Duration sin pánico (NaN/negativos -> 0; enormes -> acotados).
pub fn secs(v: f64, max: f64) -> Duration {
    if v.is_finite() && v > 0.0 {
        Duration::from_secs_f64(v.min(max))
    } else {
        Duration::ZERO
    }
}

/// Corta `s` a como mucho `max` bytes sin partir un carácter UTF-8.
pub fn cut_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Sistema del servidor guardado por la GUI (`target.txt`: `os=...`, `clicks=abs|rel`); `RIOTKVM_ABS_BUTTONS` manda.
fn initial_target() -> (Option<TargetOs>, bool) {
    if let Some(v) = abs_buttons_env_override() {
        return (None, v);
    }
    let mut os = Some(TargetOs::Windows);
    let mut via_rel = true;
    let mut manual = false;
    if let Some(t) = riotkvm_core::config::load("target.txt") {
        for l in t.lines() {
            match l.split_once('=') {
                Some(("os", v)) if v.trim() == "manual" => manual = true,
                Some(("os", v)) => os = TargetOs::parse(v).or(os),
                Some(("clicks", v)) => via_rel = v.trim() != "abs",
                _ => {}
            }
        }
    }
    if manual {
        (None, via_rel)
    } else {
        (os, os.map(|o| o.abs_buttons_via_rel()).unwrap_or(true))
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Session {
        let layout = riotkvm_core::config::effective_layout(riotkvm_core::config::env("LAYOUT").as_deref());
        let (target, abs_via_rel) = initial_target();
        Session {
            port: riotkvm_core::config::env("PORT").filter(|p| !p.is_empty()),
            ble_timeout: riotkvm_core::config::env("BLE_TIMEOUT").and_then(|v| v.parse().ok()).unwrap_or(8.0),
            layout,
            kvm: None,
            lock: None,
            link: "desconectado".into(),
            name: None,
            video: None,
            video_idle: Duration::from_secs(env("VIDEO_IDLE").and_then(|v| v.parse().ok()).unwrap_or(VIDEO_IDLE_DEFAULT)),
            view: None,
            frame: None,
            abs_known: false,
            target,
            abs_via_rel,
            deadline: Instant::now(),
            cancel: Arc::new(AtomicBool::new(false)),
            shots: Vec::new(),
            shots_bytes: 0,
            logs: Vec::new(),
            logs_bytes: 0,
            logs_cut: false,
        }
    }

    /// Sesión con un `Kvm` ya abierto (tests con `MockTransport`).
    #[cfg(test)]
    pub fn with_kvm_for_test(mut kvm: Kvm) -> Session {
        let mut s = Session::new();
        s.name = kvm.dongle_name().ok().flatten();
        s.kvm = Some(kvm);
        s.link = "mock".into();
        s.target = Some(TargetOs::Windows);
        s.abs_via_rel = true;
        s
    }

    /// Al empezar cada `execute`: plazo, cancelación y salidas nuevas (el log y las capturas son por ejecución).
    pub fn begin_run(&mut self, deadline: Instant, cancel: Arc<AtomicBool>) {
        self.deadline = deadline;
        self.cancel = cancel;
        self.shots.clear();
        self.shots_bytes = 0;
        self.logs.clear();
        self.logs_bytes = 0;
        self.logs_cut = false;
    }

    pub fn is_connected(&self) -> bool {
        self.kvm.is_some()
    }

    fn uses_lock(&self) -> bool {
        !matches!(self.port.as_deref(), Some("mock") | Some("ble-mock"))
    }

    fn check_deadline(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            Err("cancelado por el cliente (notifications/cancelled)".into())
        } else if Instant::now() >= self.deadline {
            Err("tiempo agotado (timeoutSeconds): se cortó la ejecución".into())
        } else {
            Ok(())
        }
    }

    fn left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Espera hasta `d` en trozos cortos, atenta a la cancelación.
    fn pause(&self, d: Duration) -> Result<(), String> {
        let end = Instant::now() + d.min(self.left());
        loop {
            self.check_deadline()?;
            let now = Instant::now();
            if now >= end {
                return Ok(());
            }
            std::thread::sleep((end - now).min(Duration::from_millis(50)));
        }
    }

    fn kvm(&mut self) -> Result<&mut Kvm, String> {
        if self.kvm.is_none() {
            if self.uses_lock() && self.lock.is_none() {
                self.lock = Some(acquire_lock(&lock_path())?);
            }
            let choice = Choice::parse(self.port.as_deref(), false);
            let opened = select::open(&choice, Duration::from_secs_f64(self.ble_timeout.clamp(1.0, 60.0))).map_err(kerr).and_then(|t| {
                let link = match t.name() {
                    "ble" => "BLE".to_string(),
                    n if n.to_lowercase().contains("ble") => format!("BLE ({n})"),
                    n => format!("serie ({n})"),
                };
                let mut k = Kvm::new(t, &self.layout).map_err(kerr)?;
                let name = k.dongle_name().ok().flatten();
                Ok((k, link, name))
            });
            match opened {
                Ok((k, link, name)) => {
                    self.kvm = Some(k);
                    self.link = link;
                    self.name = name;
                    self.abs_known = false;
                }
                Err(e) => {
                    self.lock = None;
                    return Err(e);
                }
            }
        }
        Ok(self.kvm.as_mut().unwrap())
    }

    /// Ejecuta una acción sobre el Kvm; si el enlace o el dongle desaparecen, se olvida la conexión
    /// para reconectar en la siguiente llamada.
    fn with_kvm<T>(&mut self, op: impl FnOnce(&mut Kvm) -> riotkvm_core::error::Result<T>) -> Result<T, String> {
        let k = self.kvm()?;
        match op(k) {
            Ok(v) => Ok(v),
            Err(e) => {
                if matches!(e, KErr::Transport(_) | KErr::DeviceNotFound(_)) {
                    self.drop_kvm();
                }
                Err(kerr(e))
            }
        }
    }

    /// Suelta el dongle (BLE/serie) y el bloqueo, para que la GUI u otra instancia puedan usarlo.
    pub fn drop_kvm(&mut self) {
        if let Some(mut k) = self.kvm.take() {
            let _ = k.release_all();
            let _ = k.t.close();
        }
        self.lock = None;
        self.link = "desconectado".into();
        self.name = None;
    }

    /// Al terminar cada `execute`: nada se queda pulsado.
    pub fn release_after_run(&mut self) {
        if let Some(k) = self.kvm.as_mut() {
            if k.release_all().is_err() {
                self.drop_kvm();
            }
        }
    }

    /// Añade una línea al log de la ejecución, cortando (por límite UTF-8) al llegar a `LOG_MAX`.
    pub fn log(&mut self, line: &str) {
        if self.logs_cut {
            return;
        }
        let room = LOG_MAX.saturating_sub(self.logs_bytes);
        let part = cut_utf8(line, room);
        self.logs_bytes += part.len() + 1;
        self.logs.push(part.to_string());
        if part.len() < line.len() || self.logs_bytes >= LOG_MAX {
            self.logs_cut = true;
            self.logs.push(format!("... (console truncado a {} KB)", LOG_MAX / 1024));
        }
    }

    fn coord_space(&self) -> Result<(i64, i64), String> {
        self.view
            .or(self.frame)
            .map(|(w, h)| (w as i64, h as i64))
            .ok_or_else(|| "aún no hay captura: llama antes a riotkvm.screenshot() (o usa riotkvm.mouse.moveAbs con 0..32767)".into())
    }

    fn move_px(&mut self, x: f64, y: f64) -> Result<(), String> {
        let (w, h) = self.coord_space()?;
        self.with_kvm(|k| k.move_px(x, y, w, h))?;
        self.abs_known = true;
        Ok(())
    }

    fn target_json(&self) -> Value {
        json!({
            "os": self.target.map(|o| o.name()).unwrap_or("manual"),
            "clicks": if self.abs_via_rel { "rel" } else { "abs" },
        })
    }

    /// Cambia el camino de los clics; si hay botones mantenidos se sueltan por los dos informes.
    fn set_target(&mut self, os: Option<TargetOs>, via_rel: bool) -> Result<(), String> {
        if via_rel != self.abs_via_rel {
            if let Some(k) = self.kvm.as_mut() {
                let held = k.buttons;
                if held != 0 {
                    for rel in [true, false] {
                        k.buttons = held;
                        for name in ["left", "right", "middle"] {
                            if held & riotkvm_core::input::button_mask(name).map_err(kerr)? != 0 {
                                k.button_up(name, rel).map_err(kerr)?;
                            }
                        }
                    }
                }
            }
        }
        self.target = os;
        self.abs_via_rel = via_rel;
        Ok(())
    }

    /// Punto de entrada desde el sandbox: `op` + argumentos JSON -> valor JSON.
    pub fn call(&mut self, op: &str, a: &Value) -> Result<Value, String> {
        if op != "log" {
            self.check_deadline()?;
        }
        match op {
            "log" => {
                self.log(s(a, "text").unwrap_or(""));
                Ok(Value::Null)
            }
            "connection" => Ok(json!({
                "connected": self.kvm.is_some(),
                "link": self.link,
                "name": self.name,
                "port": self.port.clone().unwrap_or_else(|| "auto (BLE si anuncia, si no serie)".into()),
                "layout": self.layout,
                "targetOs": self.target_json(),
                "video": self.video.as_ref().map(|v| v.desc.clone()),
                "coordSpace": self.view.or(self.frame).map(|(w, h)| json!([w, h])),
            })),
            "status" => {
                let mut m: Map<String, Value> = self.with_kvm(|k| k.status())?;
                m.insert("link".into(), json!(self.link));
                m.insert("name".into(), json!(self.name));
                m.insert("layout".into(), json!(self.layout));
                Ok(Value::Object(m))
            }
            "info" => Ok(json!(self.with_kvm(|k| k.info())?)),
            "led" => {
                let want = match s(a, "value") {
                    None => None,
                    Some(v) => Some(match (riotkvm_core::Led::parse(v).map_err(kerr)?, f(a, "brightness")) {
                        (riotkvm_core::Led::Color { rgb, .. }, Some(br)) if (0.0..=100.0).contains(&br) => riotkvm_core::Led::Color { rgb, bright: br.round() as u8 },
                        (riotkvm_core::Led::Color { .. }, Some(br)) => return Err(format!("[uso] brightness fuera de rango (0..100): {br}")),
                        (l, _) => l,
                    }),
                };
                let cur = self.with_kvm(|k| {
                    if let Some(l) = want {
                        k.set_led(l)?;
                    }
                    k.led()?.ok_or_else(|| riotkvm_core::Error::Device(riotkvm_core::led::UNSUPPORTED.into()))
                })?;
                Ok(json!(want.unwrap_or(cur).to_string()))
            }
            "disconnect" => {
                self.drop_kvm();
                Ok(json!(true))
            }
            "setLayout" => {
                let name = s(a, "name").ok_or("setLayout(nombre)")?;
                let canon = riotkvm_core::layouts::canonical(name).ok_or_else(|| {
                    format!("layout desconocido {name:?}; válidos: {}", riotkvm_core::layouts::LAYOUT_NAMES.join(", "))
                })?;
                if let Some(k) = self.kvm.as_mut() {
                    k.set_layout(canon).map_err(kerr)?;
                }
                self.layout = canon.to_string();
                Ok(json!(canon))
            }
            "setTargetOs" => {
                let name = s(a, "os").ok_or("setTargetOs(\"windows\" | \"macos\" | \"linux\" | \"fedora\" | \"abs\" | \"rel\")")?;
                match name.trim().to_ascii_lowercase().as_str() {
                    "abs" => self.set_target(None, false)?,
                    "rel" => self.set_target(None, true)?,
                    other => {
                        let os = TargetOs::parse(other).ok_or_else(|| format!("sistema desconocido {name:?}: windows, macos, linux, fedora (o abs/rel a mano)"))?;
                        self.set_target(Some(os), os.abs_buttons_via_rel())?;
                    }
                }
                Ok(self.target_json())
            }
            "type" => {
                let mut text = s(a, "text").ok_or("type(texto)")?.to_string();
                if b(a, "enter") {
                    text.push('\n');
                }
                let chars: Vec<char> = text.chars().collect();
                if chars.len() > MAX_TYPE_CHARS {
                    return Err(format!("type: {} caracteres; el máximo por llamada es {MAX_TYPE_CHARS}: divide el texto", chars.len()));
                }
                let delay = f(a, "delay").filter(|d| d.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
                let fw = b(a, "fw");
                // trozos de ~1 s como mucho, para cortar a tiempo con delay alto
                let chunk = if delay > 0.0 { ((1.0 / delay) as usize).clamp(1, TYPE_CHUNK) } else { TYPE_CHUNK };
                let mut done = 0;
                for part in chars.chunks(chunk) {
                    if let Err(e) = self.check_deadline() {
                        return Err(format!("{e} (tecleados {done} de {} caracteres)", chars.len()));
                    }
                    let p: String = part.iter().collect();
                    let r = if fw { self.with_kvm(|k| k.type_text_firmware(&p)) } else { self.with_kvm(|k| k.type_text(&p, delay)) };
                    r.map_err(|e| format!("{e} (tecleados {done} de {} caracteres)", chars.len()))?;
                    done += part.len();
                }
                Ok(json!({ "typed": done }))
            }
            "key" => {
                let combos: Vec<String> = a.get("combos").and_then(Value::as_array).ok_or("key(combo, ...)")?.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                let hold = secs(f(a, "hold").unwrap_or(0.0), 10.0);
                for c in &combos {
                    self.check_deadline()?;
                    if hold.is_zero() {
                        self.with_kvm(|k| k.press(c, 0.0))?;
                    } else {
                        // mantenida: pulsar, esperar (cortable y dentro del plazo) y soltar pase lo que pase
                        self.with_kvm(|k| k.key_down_name(c))?;
                        let waited = self.pause(hold);
                        self.with_kvm(|k| k.key_up_name(c))?;
                        waited?;
                    }
                }
                Ok(json!({ "keys": combos }))
            }
            "keyDown" => {
                let c = s(a, "combo").ok_or("keyDown(combo)")?.to_string();
                self.with_kvm(|k| k.key_down_name(&c))?;
                Ok(Value::Null)
            }
            "keyUp" => {
                let c = s(a, "combo").ok_or("keyUp(combo)")?.to_string();
                self.with_kvm(|k| k.key_up_name(&c))?;
                Ok(Value::Null)
            }
            "release" => {
                self.with_kvm(|k| k.release_all())?;
                Ok(Value::Null)
            }
            "move" => {
                self.move_px(f(a, "x").ok_or("move(x, y)")?, f(a, "y").ok_or("move(x, y)")?)?;
                Ok(Value::Null)
            }
            "moveAbs" => {
                let (x, y) = (f(a, "x").ok_or("moveAbs(x, y)")?, f(a, "y").ok_or("moveAbs(x, y)")?);
                let (x, y) = (x.clamp(0.0, 32767.0) as i64, y.clamp(0.0, 32767.0) as i64);
                self.with_kvm(|k| k.move_abs(x, y))?;
                self.abs_known = true;
                Ok(Value::Null)
            }
            "moveRel" => {
                let (dx, dy) = (f(a, "dx").ok_or("moveRel(dx, dy)")?, f(a, "dy").ok_or("moveRel(dx, dy)")?);
                let m = MAX_REL as f64;
                let (dx, dy) = (dx.clamp(-m, m) as i64, dy.clamp(-m, m) as i64);
                self.with_kvm(|k| k.move_rel(dx, dy, 0))?;
                Ok(Value::Null)
            }
            "click" => {
                let button = s(a, "button").unwrap_or("left").to_string();
                let count = f(a, "count").filter(|c| c.is_finite()).unwrap_or(1.0).clamp(1.0, 5.0) as i64;
                let rel = b(a, "relative") || self.abs_via_rel;
                let interval = f(a, "interval").filter(|c| c.is_finite()).unwrap_or(0.08).clamp(0.0, 2.0);
                if let (Some(x), Some(y)) = (f(a, "x"), f(a, "y")) {
                    self.move_px(x, y)?;
                }
                self.with_kvm(|k| k.click(&button, count, rel, interval))?;
                Ok(Value::Null)
            }
            "down" | "up" => {
                let button = s(a, "button").unwrap_or("left").to_string();
                let rel = b(a, "relative") || self.abs_via_rel;
                if op == "down" {
                    self.with_kvm(|k| k.button_down(&button, rel))?;
                } else {
                    self.with_kvm(|k| k.button_up(&button, rel))?;
                }
                Ok(Value::Null)
            }
            "scroll" => {
                let n = f(a, "amount").ok_or("scroll(n)")?;
                let m = MAX_SCROLL as f64;
                let n = if n.is_finite() { n.clamp(-m, m) as i64 } else { 0 };
                // por el absoluto solo si sabemos dónde está el puntero: si no, el informe lo llevaría al centro
                let rel = b(a, "relative") || self.abs_via_rel || !self.abs_known;
                self.with_kvm(|k| k.scroll(n, rel))?;
                Ok(Value::Null)
            }
            "sleep" => {
                let want = secs(f(a, "secs").unwrap_or(0.0), 86_400.0);
                self.pause(want)?;
                if want > self.left() {
                    self.check_deadline()?;
                }
                Ok(Value::Null)
            }
            "raw" => {
                let line = s(a, "line").ok_or("raw(orden)")?.trim().to_string();
                if line.chars().any(char::is_control) {
                    return Err("raw: la orden no puede llevar caracteres de control (NUL, saltos de línea...)".into());
                }
                let first = line.split(' ').next().unwrap_or("").to_lowercase();
                if !RAW_ALLOWED.contains(&first.as_str()) {
                    return Err(format!(
                        "raw: '{first}' no está permitida en el MCP; solo {} (reiniciar, cambiar de modo o tocar el BLE se hace a mano con `riotkvm`)",
                        RAW_ALLOWED.join(" ")
                    ));
                }
                Ok(json!(self.with_kvm(|k| k.raw(&line))?))
            }
            "videoDevices" => Ok(Value::Array(
                riotkvm_video::list_devices()
                    .into_iter()
                    .map(|d| json!({"index": d.index, "name": d.name, "captureCard": d.capture_card, "virtual": d.virtual_cam}))
                    .collect(),
            )),
            "screenshot" => self.screenshot(a),
            "stopVideo" => {
                if let Some(v) = self.video.take() {
                    v.latest.stop();
                }
                Ok(json!(true))
            }
            other => Err(format!("operación desconocida {other:?}")),
        }
    }

    fn start_video(&mut self) -> Result<(), String> {
        let pref = env("VIDEO").unwrap_or_else(|| "capture".into());
        let (src, desc) = match pref.to_lowercase().as_str() {
            "none" | "off" | "no" => return Err("vídeo desactivado (RIOTKVM_MCP_VIDEO=none): no hay capturas; el teclado y el ratón siguen funcionando".into()),
            "pattern" => (Source::Pattern, "patrón de prueba (RIOTKVM_MCP_VIDEO=pattern): NO es la pantalla real".to_string()),
            "capture" | "auto" => {
                let devs = riotkvm_video::list_devices();
                match devs.iter().find(|d| d.capture_card) {
                    Some(d) => (Source::Native { name: Some(d.name.clone()), index: Some(d.index) }, format!("{} (capturadora)", d.name)),
                    None => {
                        let names: Vec<String> = devs.iter().map(|d| d.name.clone()).collect();
                        return Err(format!(
                            "no hay ninguna capturadora HDMI conectada al portátil (dispositivos de vídeo: {:?}). Sin ella no se ve la pantalla del servidor; el teclado y el ratón siguen funcionando. Fija otra con RIOTKVM_MCP_VIDEO=<nombre>.",
                            names
                        ));
                    }
                }
            }
            _ => {
                let devs = riotkvm_video::list_devices();
                let (s, warn) = riotkvm_video::source_from_pref(&pref, None, &devs);
                if let Some(w) = warn {
                    return Err(w);
                }
                let d = riotkvm_video::resolve(&s);
                (d.0, d.1)
            }
        };
        let (w, h, fps) = env("RES")
            .and_then(|r| {
                let (wh, fps) = r.split_once('@').unwrap_or((&r, "30"));
                let (w, h) = wh.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?, fps.parse().ok()?))
            })
            .unwrap_or((1920u32, 1080u32, 30u32));
        let mut mode = Mode { w, h, fps, format: "mjpeg".into() };
        if let Source::Native { index: Some(i), .. } = &src {
            let modes = riotkvm_video::native::modes(*i);
            if let Some(m) = riotkvm_video::pick_mode((w, h, fps), &modes) {
                mode = Mode { w: m.0, h: m.1, fps: m.2, format: mode.format };
            }
        }
        let latest = Latest::new();
        let _ = riotkvm_video::start(&src, &mode, latest.clone());
        let last_use = Arc::new(Mutex::new(Instant::now()));
        // apaga la capturadora si nadie pide capturas (CPU y cámara libres)
        {
            let (latest, last_use, idle) = (latest.clone(), last_use.clone(), self.video_idle);
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(2));
                if latest.stopped() {
                    break;
                }
                if last_use.lock().unwrap_or_else(|e| e.into_inner()).elapsed() > idle {
                    latest.stop();
                    break;
                }
            });
        }
        self.video = Some(Video { latest, last_use, desc: format!("{desc}, {}x{}@{}", mode.w, mode.h, mode.fps) });
        Ok(())
    }

    fn screenshot(&mut self, a: &Value) -> Result<Value, String> {
        if self.shots.len() >= MAX_SHOTS {
            return Err(format!("máximo {MAX_SHOTS} capturas por execute (contexto del modelo): divide el trabajo en varias llamadas"));
        }
        if self.video.as_ref().map(|v| v.latest.stopped()).unwrap_or(true) {
            self.video = None;
            self.start_video()?;
        }
        let num = |k: &str, d: f64| f(a, k).filter(|v| v.is_finite()).unwrap_or(d);
        let max_w = num("maxWidth", 1280.0).clamp(160.0, MAX_WIDTH) as u32;
        let png = s(a, "format") == Some("png");
        let quality = num("quality", 75.0).clamp(30.0, 95.0) as u8;
        let settle = num("settleMs", 250.0).clamp(0.0, 5000.0);
        if let Some(v) = self.video.as_ref() {
            *v.last_use.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        }
        self.pause(Duration::from_secs_f64(settle / 1000.0))?;
        let v = self.video.as_ref().ok_or("sin vídeo")?;
        let _ = v.latest.take(); // descarta el cuadro viejo: queremos uno posterior a las acciones
        let limit = Instant::now() + Duration::from_secs(8);
        let frame = loop {
            if let Some(fr) = v.latest.wait(Duration::from_millis(200)) {
                break fr;
            }
            self.check_deadline()?;
            if Instant::now() >= limit {
                let why = v.latest.error().unwrap_or_else(|| "no llegó ningún cuadro".into());
                return Err(format!("sin vídeo de {}: {why}", v.desc));
            }
        };
        let desc = v.desc.clone();
        let img = RgbaImage::from_raw(frame.w, frame.h, frame.rgba).ok_or("cuadro con tamaño inconsistente")?;
        // a RGB antes de reescalar: un 25 % menos de memoria y trabajo (P2)
        let mut rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
        if frame.w > max_w {
            let nh = ((frame.h as f64 * max_w as f64 / frame.w as f64).round() as u32).max(1);
            rgb = image::imageops::resize(&rgb, max_w, nh, FilterType::Triangle);
        }
        let (vw, vh) = rgb.dimensions();
        let mut buf = Cursor::new(Vec::new());
        let mime = if png {
            rgb.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| e.to_string())?;
            "image/png"
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality).encode_image(&rgb).map_err(|e| e.to_string())?;
            "image/jpeg"
        };
        let bytes = buf.into_inner();
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        if self.shots_bytes + data_b64.len() > SHOTS_BUDGET {
            return Err(format!(
                "las capturas de esta llamada superarían {} MB: pide maxWidth menor, JPEG o menos capturas (llevas {})",
                SHOTS_BUDGET / (1024 * 1024),
                self.shots.len()
            ));
        }
        self.shots_bytes += data_b64.len();
        self.shots.push(Shot { mime, data_b64 });
        self.view = Some((vw, vh));
        self.frame = Some((frame.w, frame.h));
        Ok(json!({
            "width": vw, "height": vh,
            "sourceWidth": frame.w, "sourceHeight": frame.h,
            "bytes": bytes.len(), "source": desc, "seq": frame.seq,
            "image": self.shots.len(),
            "note": "imagen adjunta al resultado; riotkvm.mouse.* usa ahora coordenadas de ESTA imagen"
        }))
    }
}
