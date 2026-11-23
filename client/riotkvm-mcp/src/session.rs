//! Estado persistente entre llamadas `execute`: conexión con el dongle (BLE o serie), la capturadora y el
//! espacio de coordenadas de la última captura enviada al modelo.

use base64::Engine;
use dkvm_core::error::Error as KErr;
use dkvm_core::input::Kvm;
use dkvm_core::transport::select::{self, Choice};
use dkvm_video::{Latest, Mode, Source};
use image::{imageops::FilterType, DynamicImage, RgbaImage};
use serde_json::{json, Map, Value};
use std::io::Cursor;
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

pub struct Session {
    port: Option<String>,
    ble_timeout: f64,
    layout: String,
    kvm: Option<Kvm>,
    link: String,
    video: Option<Video>,
    video_idle: Duration,
    /// Tamaño de la última captura ENVIADA al modelo: las coordenadas de `mouse.move` están en este espacio.
    view: Option<(u32, u32)>,
    /// Tamaño real del último cuadro de la capturadora.
    frame: Option<(u32, u32)>,
    pub deadline: Instant,
    pub shots: Vec<Shot>,
    pub logs: Vec<String>,
    logs_bytes: usize,
}

const DANGEROUS: [&str; 6] = ["bootloader", "mode", "reboot", "ble", "coredump", "kall_never"];

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

impl Session {
    pub fn new() -> Session {
        let layout = dkvm_core::config::effective_layout(std::env::var("DKVM_LAYOUT").ok().as_deref());
        Session {
            port: std::env::var("DKVM_PORT").ok().filter(|p| !p.is_empty()),
            ble_timeout: std::env::var("DKVM_BLE_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(8.0),
            layout,
            kvm: None,
            link: "desconectado".into(),
            video: None,
            video_idle: Duration::from_secs(std::env::var("DKVM_MCP_VIDEO_IDLE").ok().and_then(|v| v.parse().ok()).unwrap_or(120)),
            view: None,
            frame: None,
            deadline: Instant::now(),
            shots: Vec::new(),
            logs: Vec::new(),
            logs_bytes: 0,
        }
    }

    fn check_deadline(&self) -> Result<(), String> {
        if Instant::now() >= self.deadline {
            Err("tiempo agotado (timeoutSeconds): se cortó la ejecución".into())
        } else {
            Ok(())
        }
    }

    fn kvm(&mut self) -> Result<&mut Kvm, String> {
        if self.kvm.is_none() {
            let choice = Choice::parse(self.port.as_deref(), false);
            let t = select::open(&choice, Duration::from_secs_f64(self.ble_timeout)).map_err(kerr)?;
            self.link = match t.name() {
                "ble" => "BLE".to_string(),
                n if n.to_lowercase().contains("ble") => format!("BLE ({n})"),
                n => format!("serie ({n})"),
            };
            self.kvm = Some(Kvm::new(t, &self.layout).map_err(kerr)?);
        }
        Ok(self.kvm.as_mut().unwrap())
    }

    /// Ejecuta una acción sobre el Kvm; si el enlace o el dongle desaparecen, se olvida la conexión
    /// para reconectar en la siguiente llamada.
    fn with_kvm<T>(&mut self, op: impl FnOnce(&mut Kvm) -> dkvm_core::error::Result<T>) -> Result<T, String> {
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

    fn drop_kvm(&mut self) {
        if let Some(mut k) = self.kvm.take() {
            let _ = k.t.close();
        }
        self.link = "desconectado".into();
    }

    /// Al terminar cada `execute`: nada se queda pulsado.
    pub fn release_after_run(&mut self) {
        if let Some(k) = self.kvm.as_mut() {
            if k.release_all().is_err() {
                self.drop_kvm();
            }
        }
    }

    pub fn log(&mut self, line: String) {
        if self.logs_bytes > 64 * 1024 {
            return;
        }
        self.logs_bytes += line.len();
        self.logs.push(line);
        if self.logs_bytes > 64 * 1024 {
            self.logs.push("... (log truncado a 64 KB)".into());
        }
    }

    fn coord_space(&self) -> Result<(i64, i64), String> {
        self.view
            .or(self.frame)
            .map(|(w, h)| (w as i64, h as i64))
            .ok_or_else(|| "aún no hay captura: llama antes a qkvm.screenshot() (o usa qkvm.mouse.moveAbs con 0..32767)".into())
    }

    fn move_px(&mut self, x: f64, y: f64) -> Result<(), String> {
        let (w, h) = self.coord_space()?;
        self.with_kvm(|k| k.move_px(x, y, w, h))
    }

    /// Punto de entrada desde el sandbox: `op` + argumentos JSON -> valor JSON.
    pub fn call(&mut self, op: &str, a: &Value) -> Result<Value, String> {
        if op != "log" {
            self.check_deadline()?;
        }
        match op {
            "log" => {
                self.log(s(a, "text").unwrap_or("").to_string());
                Ok(Value::Null)
            }
            "connection" => Ok(json!({
                "connected": self.kvm.is_some(),
                "link": self.link,
                "port": self.port.clone().unwrap_or_else(|| "auto (BLE si anuncia, si no serie)".into()),
                "layout": self.layout,
                "video": self.video.as_ref().map(|v| v.desc.clone()),
                "coordSpace": self.view.or(self.frame).map(|(w, h)| json!([w, h])),
            })),
            "status" => {
                let mut m: Map<String, Value> = self.with_kvm(|k| k.status())?;
                m.insert("link".into(), json!(self.link));
                m.insert("layout".into(), json!(self.layout));
                Ok(Value::Object(m))
            }
            "info" => Ok(json!(self.with_kvm(|k| k.info())?)),
            "disconnect" => {
                self.drop_kvm();
                Ok(json!(true))
            }
            "setLayout" => {
                let name = s(a, "name").ok_or("setLayout(nombre)")?;
                let canon = dkvm_core::layouts::canonical(name).ok_or_else(|| {
                    format!("layout desconocido {name:?}; válidos: {}", dkvm_core::layouts::LAYOUT_NAMES.join(", "))
                })?;
                if let Some(k) = self.kvm.as_mut() {
                    k.set_layout(canon).map_err(kerr)?;
                }
                self.layout = canon.to_string();
                Ok(json!(canon))
            }
            "type" => {
                let mut text = s(a, "text").ok_or("type(texto)")?.to_string();
                if b(a, "enter") {
                    text.push('\n');
                }
                let delay = f(a, "delay").unwrap_or(0.0).clamp(0.0, 1.0);
                let n = text.chars().count();
                if b(a, "fw") {
                    self.with_kvm(|k| k.type_text_firmware(&text))?;
                } else {
                    self.with_kvm(|k| k.type_text(&text, delay))?;
                }
                Ok(json!({ "typed": n }))
            }
            "key" => {
                let combos: Vec<String> = a.get("combos").and_then(Value::as_array).ok_or("key(combo, ...)")?.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                let hold = f(a, "hold").unwrap_or(0.0).clamp(0.0, 10.0);
                for c in &combos {
                    self.check_deadline()?;
                    self.with_kvm(|k| k.press(c, hold))?;
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
                let (x, y) = (f(a, "x").ok_or("moveAbs(x, y)")? as i64, f(a, "y").ok_or("moveAbs(x, y)")? as i64);
                self.with_kvm(|k| k.move_abs(x, y))?;
                Ok(Value::Null)
            }
            "moveRel" => {
                let (dx, dy) = (f(a, "dx").ok_or("moveRel(dx, dy)")? as i64, f(a, "dy").ok_or("moveRel(dx, dy)")? as i64);
                self.with_kvm(|k| k.move_rel(dx, dy, 0))?;
                Ok(Value::Null)
            }
            "click" => {
                let button = s(a, "button").unwrap_or("left").to_string();
                let count = f(a, "count").unwrap_or(1.0).clamp(1.0, 5.0) as i64;
                let rel = b(a, "relative");
                let interval = f(a, "interval").unwrap_or(0.08).clamp(0.0, 2.0);
                if let (Some(x), Some(y)) = (f(a, "x"), f(a, "y")) {
                    self.move_px(x, y)?;
                }
                self.with_kvm(|k| k.click(&button, count, rel, interval))?;
                Ok(Value::Null)
            }
            "down" | "up" => {
                let button = s(a, "button").unwrap_or("left").to_string();
                let rel = b(a, "relative");
                if op == "down" {
                    self.with_kvm(|k| k.button_down(&button, rel))?;
                } else {
                    self.with_kvm(|k| k.button_up(&button, rel))?;
                }
                Ok(Value::Null)
            }
            "scroll" => {
                let n = f(a, "amount").ok_or("scroll(n)")? as i64;
                let rel = b(a, "relative");
                self.with_kvm(|k| k.scroll(n, rel))?;
                Ok(Value::Null)
            }
            "sleep" => {
                let secs = f(a, "secs").unwrap_or(0.0).max(0.0);
                let left = self.deadline.saturating_duration_since(Instant::now());
                let d = Duration::from_secs_f64(secs).min(left);
                std::thread::sleep(d);
                self.check_deadline()?;
                Ok(Value::Null)
            }
            "raw" => {
                let line = s(a, "line").ok_or("raw(orden)")?.trim().to_string();
                let first = line.split_whitespace().next().unwrap_or("").to_lowercase();
                if DANGEROUS.contains(&first.as_str()) {
                    return Err(format!("'{first}' está bloqueada en el MCP (reinicia el dongle, cambia de modo o corta el BLE): hazlo a mano con `dkvm`"));
                }
                Ok(json!(self.with_kvm(|k| k.raw(&line))?))
            }
            "videoDevices" => Ok(Value::Array(
                dkvm_video::list_devices()
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
        let pref = std::env::var("DKVM_MCP_VIDEO").unwrap_or_else(|_| "capture".into());
        let devs = dkvm_video::list_devices();
        let (src, desc) = match pref.to_lowercase().as_str() {
            "pattern" => (Source::Pattern, "patrón de prueba (DKVM_MCP_VIDEO=pattern): NO es la pantalla real".to_string()),
            "capture" | "auto" | "" => match devs.iter().find(|d| d.capture_card) {
                Some(d) => (Source::Native { name: Some(d.name.clone()), index: Some(d.index) }, format!("{} (capturadora)", d.name)),
                None => {
                    let names: Vec<String> = devs.iter().map(|d| d.name.clone()).collect();
                    return Err(format!(
                        "no hay ninguna capturadora HDMI conectada al portátil (dispositivos de vídeo: {:?}). Sin ella no se ve la pantalla del servidor; el teclado y el ratón siguen funcionando. Fija otra con DKVM_MCP_VIDEO=<nombre>.",
                        names
                    ));
                }
            },
            _ => {
                let (s, warn) = dkvm_video::source_from_pref(&pref, None, &devs);
                if let Some(w) = warn {
                    return Err(w);
                }
                let d = dkvm_video::resolve(&s);
                (d.0, d.1)
            }
        };
        let (w, h, fps) = std::env::var("DKVM_MCP_RES")
            .ok()
            .and_then(|r| {
                let (wh, fps) = r.split_once('@').unwrap_or((&r, "30"));
                let (w, h) = wh.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?, fps.parse().ok()?))
            })
            .unwrap_or((1920u32, 1080u32, 30u32));
        let mut mode = Mode { w, h, fps, format: "mjpeg".into() };
        if let Source::Native { index: Some(i), .. } = &src {
            let modes = dkvm_video::native::modes(*i);
            if let Some(m) = dkvm_video::pick_mode((w, h, fps), &modes) {
                mode = Mode { w: m.0, h: m.1, fps: m.2, format: mode.format };
            }
        }
        let latest = Latest::new();
        let _ = dkvm_video::start(&src, &mode, latest.clone());
        let last_use = Arc::new(Mutex::new(Instant::now()));
        // apaga la capturadora si nadie pide capturas (CPU y cámara libres)
        {
            let (latest, last_use, idle) = (latest.clone(), last_use.clone(), self.video_idle);
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                if latest.stopped() {
                    break;
                }
                if last_use.lock().unwrap().elapsed() > idle {
                    latest.stop();
                    break;
                }
            });
        }
        self.video = Some(Video { latest, last_use, desc: format!("{desc}, {}x{}@{}", mode.w, mode.h, mode.fps) });
        Ok(())
    }

    fn screenshot(&mut self, a: &Value) -> Result<Value, String> {
        if self.video.as_ref().map(|v| v.latest.stopped()).unwrap_or(true) {
            self.video = None;
            self.start_video()?;
        }
        let max_w = f(a, "maxWidth").unwrap_or(1280.0).clamp(160.0, 1920.0) as u32;
        let png = s(a, "format") == Some("png");
        let quality = f(a, "quality").unwrap_or(75.0).clamp(30.0, 95.0) as u8;
        let settle = f(a, "settleMs").unwrap_or(250.0).clamp(0.0, 5000.0);
        if self.shots.len() >= 6 {
            return Err("máximo 6 capturas por execute (contexto del modelo): divide el trabajo en varias llamadas".into());
        }
        let v = self.video.as_ref().unwrap();
        *v.last_use.lock().unwrap() = Instant::now();
        std::thread::sleep(Duration::from_secs_f64(settle / 1000.0));
        let _ = v.latest.take(); // descarta el cuadro viejo: queremos uno posterior a las acciones
        let limit = Instant::now() + Duration::from_secs(8);
        let frame = loop {
            if let Some(fr) = v.latest.wait(Duration::from_millis(200)) {
                break fr;
            }
            if Instant::now() >= limit || Instant::now() >= self.deadline {
                let why = v.latest.error().unwrap_or_else(|| "no llegó ningún cuadro".into());
                return Err(format!("sin vídeo de {}: {why}", v.desc));
            }
        };
        let desc = v.desc.clone();
        let img = RgbaImage::from_raw(frame.w, frame.h, frame.rgba).ok_or("cuadro con tamaño inconsistente")?;
        let mut dynimg = DynamicImage::ImageRgba8(img);
        if frame.w > max_w {
            let nh = (frame.h as f64 * max_w as f64 / frame.w as f64).round() as u32;
            dynimg = dynimg.resize_exact(max_w, nh, FilterType::Triangle);
        }
        let rgb = dynimg.to_rgb8();
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
        self.shots.push(Shot { mime, data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes) });
        self.view = Some((vw, vh));
        self.frame = Some((frame.w, frame.h));
        Ok(json!({
            "width": vw, "height": vh,
            "sourceWidth": frame.w, "sourceHeight": frame.h,
            "bytes": bytes.len(), "source": desc, "seq": frame.seq,
            "image": self.shots.len(),
            "note": "imagen adjunta al resultado; qkvm.mouse.* usa ahora coordenadas de ESTA imagen"
        }))
    }
}
