//! Sesión interactiva (GUI, TUI...): un hilo dueño del `Kvm` que consume órdenes de entrada.
//!
//! Ruta de entrada:  hilo de UI / hook de teclado --`Cmd`--> hilo de sesión --> `Kvm` --> hilo de E/S.
//! - Quien genera la entrada solo hace un `mpsc::send` (no bloquea, no formatea, no toca el puerto).
//! - El hilo de sesión drena todo lo pendiente de una vez: el movimiento de ratón se agrupa en una
//!   sola orden por lote (`MouseCoalescer`), conservando el orden respecto a teclas y botones.
//! - Sondea `status` cada segundo (LEDs, host, RTT) y se reconecta solo si el dongle se va.

use crate::error::{Error, Result};
use crate::input::{Kvm, MouseCoalescer};
use crate::transport::{LinkState, TEvent, Transport};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum Cmd {
    Key { usage: u8, down: bool },
    MouseAbs { x: i64, y: i64 },
    MouseRel { dx: i64, dy: i64 },
    /// Rueda: positivo = arriba.
    Wheel { amount: i64, rel: bool },
    /// `name`: left | right | middle.
    Button { name: &'static str, down: bool, rel: bool },
    /// Combinación completa (`ctrl+alt+del`): pulsa y suelta. Equivale a `Keys`.
    Combo(String),
    /// Una o varias combinaciones separadas por espacios (`alt+f4 enter`, misma sintaxis que `dkvm key`): se pulsan
    /// una tras otra y SIEMPRE se suelta todo al terminar o fallar.
    Keys(String),
    /// Repite una combinación cada `interval_ms` durante `secs` (p. ej. Supr/F2 para entrar en la BIOS). Se para con `cancel_job`.
    Repeat { combo: String, interval_ms: u32, secs: f32 },
    /// Cambia la distribución del teclado del servidor (nombre o alias).
    SetLayout(String),
    /// Teclea un texto largo por lotes (portapapeles). Cancelable con `cancel_job`.
    Paste(PasteJob),
    /// Escribe texto con el layout del destino.
    Type(String),
    ReleaseAll,
    Quit,
}

/// Trabajo de tecleo de texto. El contenido NUNCA se muestra en `Debug` ni en logs (puede llevar contraseñas).
#[derive(Clone)]
pub struct PasteJob {
    pub text: String,
    /// Distribución a usar (`us` = "teclas físicas" para BIOS).
    pub layout: String,
    /// Pausa por pulsación en ms (0 = lo más rápido que admita el enlace). Defecto recomendado: 5.
    pub per_key_ms: u32,
}

impl std::fmt::Debug for PasteJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PasteJob {{ chars: {}, layout: {}, per_key_ms: {} }}", self.text.chars().count(), self.layout, self.per_key_ms)
    }
}

/// Opciones de la sesión.
#[derive(Debug, Clone, Copy)]
pub struct SessionOpts {
    /// En modo ABSOLUTO, botones y rueda salen por el ratón RELATIVO (`mbr`, rueda `mm`) justo tras el `ma` de la posición.
    /// Defecto true (workaround: el Fedora no reacciona a los botones del informe absoluto). `DKVM_ABS_BUTTONS=abs` lo desactiva.
    pub abs_buttons_via_rel: bool,
}

impl Default for SessionOpts {
    fn default() -> Self {
        SessionOpts { abs_buttons_via_rel: std::env::var("DKVM_ABS_BUTTONS").map(|v| v != "abs").unwrap_or(true) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Progreso de un trabajo largo (pegado, repetición): hechos / total (0 = desconocido).
    Job { label: String, done: usize, total: usize },
    /// Fin de un trabajo (o de un envío de teclas): resumen para la barra de estado.
    JobDone { label: String, summary: String, error: Option<String>, cancelled: bool },
    /// Estado del enlace (buscando / conectando / conectado / reconectando). Serie: Connected o Reconnecting.
    Link(LinkState),
    /// Estado del dongle: host USB, HID, LEDs, SD, modo (A/B/C, BLE), boot protocol (BIOS).
    Status { host: bool, hidready: bool, leds: u8, sd: bool, mode: Option<String>, boot: bool },
    /// RTT del enlace en microsegundos (serie: `status`; BLE: ping con secuencia).
    Rtt(u64),
    /// El dongle rechazó una orden (`err nohost`...), con pista legible.
    DeviceError(String),
}

pub type Connector = Box<dyn FnMut() -> Result<Box<dyn Transport>> + Send>;

pub struct Session {
    pub tx: Sender<Cmd>,
    pub events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Session {
    /// `notify` se llama tras publicar cada evento (p. ej. `ctx.request_repaint()`).
    pub fn spawn(connect: Connector, layout: &str, notify: Option<Box<dyn Fn() + Send>>) -> Result<Session> {
        Session::spawn_with(connect, layout, notify, SessionOpts::default())
    }

    /// Para el trabajo largo en curso (pegado o repetición): suelta todas las teclas y botones.
    pub fn cancel_job(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn spawn_with(mut connect: Connector, layout: &str, notify: Option<Box<dyn Fn() + Send>>, opts: SessionOpts) -> Result<Session> {
        crate::layouts::get_layout(layout)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel2 = cancel.clone(); // falla pronto si el layout no existe
        let layout = layout.to_string();
        let (tx, rx) = channel::<Cmd>();
        let (etx, events) = channel::<Event>();
        let join = std::thread::Builder::new()
            .name("dkvm-session".into())
            .spawn(move || {
                crate::rt::boost_current_thread();
                let emit = |e: Event| {
                    let _ = etx.send(e);
                    if let Some(n) = &notify {
                        n();
                    }
                };
                run(&mut connect, &layout, rx, &emit, &cancel2, opts);
            })
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Session { tx, events, cancel, join: Some(join) })
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn is_link_error(e: &Error) -> bool {
    matches!(e, Error::Transport(_) | Error::DeviceNotFound(_))
}

fn apply(k: &mut Kvm, co: &mut MouseCoalescer, c: Cmd, opts: SessionOpts) -> Result<()> {
    match c {
        Cmd::MouseAbs { x, y } => {
            co.set_abs(x, y);
            return Ok(());
        }
        Cmd::MouseRel { dx, dy } => {
            co.add_rel(dx, dy);
            return Ok(());
        }
        _ => {}
    }
    // cualquier otra orden respeta el orden: antes se vacía el movimiento acumulado
    co.flush_into(k)?;
    match c {
        Cmd::Key { usage, down } => {
            if down {
                k.key_down(usage as i64)
            } else {
                k.key_up(usage as i64)
            }
        }
        Cmd::Wheel { amount, rel } => k.scroll(amount, rel || opts.abs_buttons_via_rel),
        Cmd::Button { name, down, rel } => {
            let rel = rel || opts.abs_buttons_via_rel; // modo absoluto: el botón sale por el ratón relativo (`mbr`)
            if down {
                k.button_down(name, rel)
            } else {
                k.button_up(name, rel)
            }
        }
        Cmd::Combo(s) | Cmd::Keys(s) => k.press(&s, 0.05),
        Cmd::SetLayout(n) => k.set_layout(&n),
        Cmd::Repeat { .. } | Cmd::Paste(_) => Ok(()), // los trata `run` (necesitan eventos y cancelación)
        Cmd::Type(s) => k.type_text(&s, 0.0),
        Cmd::ReleaseAll => k.release_all(),
        Cmd::Quit | Cmd::MouseAbs { .. } | Cmd::MouseRel { .. } => Ok(()),
    }
}

fn nap(ms: u64, cancel: &AtomicBool) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(ms) && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(ms.clamp(1, 10)));
    }
}

fn first_chars(bad: &[char], n: usize) -> String {
    let mut seen: Vec<char> = Vec::new();
    for c in bad {
        if !seen.contains(c) {
            seen.push(*c);
        }
        if seen.len() >= n {
            break;
        }
    }
    seen.iter().map(|c| if c.is_control() { format!("U+{:04X}", *c as u32) } else { c.to_string() }).collect::<Vec<_>>().join(" ")
}

/// Pulsa combinaciones una tras otra y SIEMPRE suelta todo al final (también si falla una).
fn keys_run(k: &mut Kvm, text: &str, emit: &dyn Fn(Event)) -> Result<()> {
    let mut err: Option<Error> = None;
    for combo in text.split_whitespace() {
        if let Err(e) = k.press(combo, 0.05) {
            err = Some(e);
            break;
        }
    }
    let rel = k.release_all().and_then(|_| k.flush());
    if let Some(e) = &err {
        if is_link_error(e) {
            return Err(e.clone());
        }
    }
    emit(Event::JobDone { label: "teclas".into(), summary: format!("enviado: {}", text.trim()), error: err.as_ref().map(|e| e.message().to_string()), cancelled: false });
    rel
}

/// Repite una combinación (Supr/F2...) hasta `secs` o cancelación; suelta todo al acabar.
fn repeat_run(k: &mut Kvm, combo: &str, interval_ms: u32, secs: f32, cancel: &AtomicBool, emit: &dyn Fn(Event)) -> Result<()> {
    cancel.store(false, Ordering::Relaxed);
    let interval = interval_ms.max(10) as u64;
    let total = ((secs.max(0.1) as f64 * 1000.0) / interval as f64).ceil() as usize;
    let (t0, mut n, mut last_emit) = (Instant::now(), 0usize, Instant::now());
    let mut err: Option<Error> = None;
    while n < total && !cancel.load(Ordering::Relaxed) {
        if let Err(e) = k.press(combo, 0.02) {
            err = Some(e);
            break;
        }
        n += 1;
        if last_emit.elapsed() > Duration::from_millis(200) {
            last_emit = Instant::now();
            emit(Event::Job { label: format!("repetir {combo}"), done: n, total });
        }
        nap(interval, cancel);
    }
    let cancelled = cancel.swap(false, Ordering::Relaxed);
    let rel = k.release_all().and_then(|_| k.flush());
    if let Some(e) = &err {
        if is_link_error(e) {
            return Err(e.clone());
        }
    }
    emit(Event::JobDone {
        label: "repetir".into(),
        summary: format!("{combo} x{n} en {:.1} s{}", t0.elapsed().as_secs_f64(), if cancelled { " (parado)" } else { "" }),
        error: err.map(|e| e.message().to_string()),
        cancelled,
    });
    rel
}

/// Teclea el texto por lotes: envía un lote pequeño, espera a que el enlace lo acepte, deja pasar el tiempo configurado y
/// comprueba si el dongle contestó ERR. `err busy` (informe perdido) -> suelta todo y REINTENTA el lote con más pausa
/// (puede duplicar un carácter del lote, nunca se pierde en silencio); cualquier otro ERR (sin host, sin HID...) aborta.
fn paste_run(k: &mut Kvm, job: PasteJob, cancel: &AtomicBool, emit: &dyn Fn(Event)) -> Result<()> {
    cancel.store(false, Ordering::Relaxed);
    let t0 = Instant::now();
    let layout = match crate::layouts::get_layout(&job.layout) {
        Ok(l) => l,
        Err(e) => {
            emit(Event::JobDone { label: "pegar".into(), summary: String::new(), error: Some(e.message().to_string()), cancelled: false });
            return Ok(());
        }
    };
    let (strokes, bad) = layout.text_strokes_lossy(&job.text);
    let total = strokes.len();
    const CHUNK: usize = 4;
    let (mut done, mut retries, mut delay) = (0usize, 0u32, job.per_key_ms as u64);
    let mut last_emit = Instant::now();
    let mut failure: Option<String> = None;
    let mut cancelled = false;
    while done < total {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let end = (done + CHUNK).min(total);
        let mut res: Result<()> = Ok(());
        for (usage, mods) in &strokes[done..end] {
            if let Err(e) = k.tap(*usage, *mods) {
                res = Err(e);
                break;
            }
        }
        if res.is_ok() {
            res = k.t.flush();
        }
        if res.is_ok() {
            nap(delay * (end - done) as u64, cancel);
            res = k.t.poll_error();
        }
        match res {
            Ok(()) => {
                done = end;
                if last_emit.elapsed() > Duration::from_millis(100) {
                    last_emit = Instant::now();
                    emit(Event::Job { label: "pegar".into(), done, total });
                }
            }
            Err(e) if is_link_error(&e) => {
                let _ = k.release_all();
                return Err(e);
            }
            Err(e) if e.message().to_lowercase().contains("busy") && retries < 15 => {
                retries += 1;
                delay = (delay * 2).clamp(5, 40);
                let _ = k.release_all();
                let _ = k.flush();
                nap(30, cancel);
            }
            Err(e) => {
                failure = Some(e.message().to_string());
                break;
            }
        }
    }
    let _ = k.release_all();
    let _ = k.flush();
    let mut summary = format!("{} caracteres en {:.1} s", job.text.chars().count(), t0.elapsed().as_secs_f64());
    if retries > 0 {
        summary.push_str(&format!(", {retries} reintentos por dongle ocupado"));
    }
    if !bad.is_empty() {
        summary.push_str(&format!(", {} NO representables en el layout {} (saltados): {}", bad.len(), layout.name, first_chars(&bad, 8)));
    }
    if cancelled {
        summary.push_str(&format!(" (cancelado a las {done}/{total} pulsaciones)"));
    }
    emit(Event::JobDone { label: "pegar".into(), summary, error: failure, cancelled });
    Ok(())
}

fn status_event(line: &str) -> Option<Event> {
    let m = crate::protocol::parse_status(line).ok()?;
    let b = |n: &str| m.get(n).and_then(|v| v.as_bool()).unwrap_or(false);
    Some(Event::Status {
        host: b("host"),
        hidready: b("hidready"),
        leds: m.get("leds").and_then(|v| v.as_i64()).unwrap_or(0) as u8,
        sd: b("sd"),
        mode: m.get("mode").and_then(|v| v.as_str()).map(String::from),
        boot: m.get("proto").and_then(|v| v.as_str()) == Some("boot"),
    })
}

fn run(connect: &mut Connector, layout: &str, rx: Receiver<Cmd>, emit: &dyn Fn(Event), cancel: &AtomicBool, opts: SessionOpts) {
    let mut kvm: Option<Kvm> = None;
    let mut co = MouseCoalescer::default();
    let mut next_retry = Instant::now();
    let mut next_poll = Instant::now();
    let mut last_err = String::new();
    let mut last_dev_err = Instant::now() - Duration::from_secs(10);
    let mut attempt = 0u32;
    let mut need_status = true;

    loop {
        if kvm.is_none() && Instant::now() >= next_retry {
            match connect().and_then(|t| Kvm::new(t, layout)) {
                Ok(mut k) => {
                    k.auto_flush = false;
                    match k.t.link_state() {
                        Some(s) => emit(Event::Link(s)),
                        None => emit(Event::Link(LinkState::Connected { name: k.t.name().to_string(), mtu: 0 })),
                    }
                    if !k.t.self_healing() {
                        let _ = k.release_all();
                        let _ = k.flush();
                    }
                    last_err.clear();
                    attempt = 0;
                    need_status = true;
                    kvm = Some(k);
                    next_poll = Instant::now();
                }
                Err(e) => {
                    attempt += 1;
                    if e.message() != last_err {
                        last_err = e.message().to_string();
                        emit(Event::Link(LinkState::Reconnecting { attempt, reason: last_err.clone() }));
                    }
                    next_retry = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        let healing = kvm.as_ref().map(|k| k.t.self_healing()).unwrap_or(false);
        let due = if kvm.is_some() { next_poll } else { next_retry };
        let mut wait = due.saturating_duration_since(Instant::now());
        if healing {
            wait = wait.min(Duration::from_millis(40)); // eventos del transporte (estado de enlace, EVT)
        }
        let mut batch = Vec::new();
        match rx.recv_timeout(wait) {
            Ok(c) => {
                batch.push(c);
                while let Ok(c) = rx.try_recv() {
                    batch.push(c);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let quit = batch.iter().any(|c| matches!(c, Cmd::Quit));
        let mut failure: Option<Error> = None;
        if let Some(k) = kvm.as_mut() {
            let healing = k.t.self_healing();
            let up = k.t.link_state().map(|s| s.is_connected()).unwrap_or(true);
            let report = |e: Error, failure: &mut Option<Error>, last: &mut Instant| {
                if is_link_error(&e) {
                    if !healing {
                        *failure = Some(e);
                    } // BLE: el transporte se reconecta solo; el estado llega por eventos de enlace
                } else if last.elapsed() > Duration::from_millis(500) {
                    *last = Instant::now();
                    emit(Event::DeviceError(e.message().to_string()));
                }
            };
            if up {
                for c in batch {
                    if matches!(c, Cmd::Quit) {
                        continue;
                    }
                    let r = match c {
                        Cmd::Paste(job) => co.flush_into(k).and_then(|_| paste_run(k, job, cancel, emit)),
                        Cmd::Keys(s) | Cmd::Combo(s) => co.flush_into(k).and_then(|_| keys_run(k, &s, emit)),
                        Cmd::Repeat { combo, interval_ms, secs } => co.flush_into(k).and_then(|_| repeat_run(k, &combo, interval_ms, secs, cancel, emit)),
                        other => apply(k, &mut co, other, opts),
                    };
                    if let Err(e) = r {
                        report(e, &mut failure, &mut last_dev_err);
                        if failure.is_some() {
                            break;
                        }
                    }
                }
                if failure.is_none() {
                    if let Err(e) = co.flush_into(k).and_then(|_| if healing { k.t.poll_error() } else { k.flush() }) {
                        report(e, &mut failure, &mut last_dev_err);
                    }
                }
            } else {
                co = MouseCoalescer::default(); // enlace caído: la entrada no se acumula
            }
            for ev in k.t.poll_events() {
                match ev {
                    TEvent::Link(s) => {
                        if s.is_connected() {
                            need_status = true;
                            next_poll = Instant::now();
                        }
                        emit(Event::Link(s));
                    }
                    TEvent::StatusLine(l) => {
                        if let Some(e) = status_event(&l) {
                            emit(e);
                        }
                    }
                    TEvent::Rtt(us) => emit(Event::Rtt(us)),
                }
            }
            if failure.is_none() && Instant::now() >= next_poll && up {
                next_poll = Instant::now() + Duration::from_secs(1);
                if healing {
                    // BLE: ping con secuencia (RTT) y, tras (re)conectar, una lectura de estado
                    let _ = k.t.ping_nowait(); // sin esperar el pong: el RTT llega como evento y la entrada no se frena
                    if need_status {
                        if let Ok(m) = k.raw(&crate::protocol::status()) {
                            if let Some(e) = status_event(&m) {
                                emit(e);
                                need_status = false;
                            }
                        }
                    }
                } else {
                    let t = Instant::now();
                    match k.raw(&crate::protocol::status()) {
                        Ok(line) => {
                            if let Some(e) = status_event(&line) {
                                emit(e);
                            }
                            emit(Event::Rtt(t.elapsed().as_micros() as u64));
                        }
                        Err(e) if is_link_error(&e) => failure = Some(e),
                        Err(_) => {}
                    }
                }
            }
        } else {
            // sin conexión la entrada se descarta (no se acumula para reproducirla luego)
            co = MouseCoalescer::default();
        }
        if let Some(e) = failure {
            if let Some(mut k) = kvm.take() {
                let _ = k.t.close();
            }
            co = MouseCoalescer::default();
            last_err = e.message().to_string();
            attempt += 1;
            emit(Event::Link(LinkState::Reconnecting { attempt, reason: last_err.clone() }));
            next_retry = Instant::now() + Duration::from_millis(500);
        }
        if quit {
            break;
        }
    }
    if let Some(mut k) = kvm {
        let _ = k.release_all();
        let _ = k.flush();
        let _ = k.t.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{MockHandle, MockTransport};
    use std::sync::{Arc, Mutex};

    fn wait_until(f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(20), "timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn mock_session() -> (Session, MockHandle) {
        let (m, h) = MockTransport::with_handle();
        let slot = Arc::new(Mutex::new(Some(m)));
        let s = Session::spawn(
            Box::new(move || match slot.lock().unwrap().take() {
                Some(m) => Ok(Box::new(m) as Box<dyn Transport>),
                None => Err(Error::DeviceNotFound("no hay dongle".into())),
            }),
            "es",
            None,
        )
        .unwrap();
        (s, h)
    }

    #[test]
    fn connects_polls_status_and_sends_in_order() {
        std::env::set_var("DKVM_ABS_BUTTONS", "abs"); // estas pruebas miden el camino absoluto original
        let (s, h) = mock_session();
        let ev = s.events.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(matches!(ev, Event::Link(LinkState::Connected { .. })));
        let ev = s.events.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(matches!(ev, Event::Status { host: true, hidready: true, .. }), "{ev:?}");
        s.send(Cmd::Key { usage: 0x04, down: true });
        s.send(Cmd::Key { usage: 0x04, down: false });
        s.send(Cmd::Button { name: "left", down: true, rel: false });
        s.send(Cmd::Button { name: "left", down: false, rel: false });
        wait_until(|| h.lock().abs_buttons == 0 && h.lock().sent.iter().filter(|l| l.starts_with("mba")).count() >= 2);
        let st = h.lock();
        let seq: Vec<&str> = st.sent.iter().map(String::as_str).filter(|l| !l.starts_with("status") && *l != "kall" && !l.starts_with("mbr 0") && *l != "mba 0").collect();
        assert_eq!(seq, ["kd 0x04", "ku 0x04", "mba 1"]);
    }

    #[test]
    fn mouse_moves_coalesce_and_keep_order_around_buttons() {
        std::env::set_var("DKVM_ABS_BUTTONS", "abs"); // estas pruebas miden el camino absoluto original
        let (s, h) = mock_session();
        let _ = s.events.recv_timeout(Duration::from_secs(20));
        // Determinista: el hilo de sesión queda ocupado en una repetición (no drena la cola hasta que se cancela); se encola
        // toda la ráfaga y SOLO entonces se cancela, así que se drena en un único lote sin depender de ningún reloj.
        s.send(Cmd::Repeat { combo: "f2".into(), interval_ms: 20, secs: 120.0 });
        wait_until(|| h.lock().sent.iter().any(|l| l.starts_with("kd")));
        for i in 0..100 {
            s.send(Cmd::MouseAbs { x: i, y: 2 * i });
        }
        s.send(Cmd::Button { name: "left", down: true, rel: false });
        for i in 0..50 {
            s.send(Cmd::MouseRel { dx: 1, dy: -1 });
            let _ = i;
        }
        s.send(Cmd::Button { name: "left", down: false, rel: false });
        s.cancel_job();
        wait_until(|| {
            let st = h.lock();
            st.sent.iter().position(|l| l == "mba 1").map(|d| st.sent[d..].iter().any(|l| l == "mba 0")).unwrap_or(false)
        });
        let st = h.lock();
        let ma: Vec<_> = st.sent.iter().filter(|l| l.starts_with("ma ")).collect();
        // un lote: una sola orden `ma` antes del botón y una tras él (la última posición), nunca 100
        assert!(ma.len() <= 4, "no se agrupó el movimiento absoluto: {}", ma.len());
        assert_eq!(st.abs_pos, (99, 198), "debe quedar la última posición");
        assert_eq!(st.rel_total, [50, -50], "no se pierde movimiento relativo");
        // el orden pulsar/soltar se conserva
        let pos_down = st.sent.iter().position(|l| l == "mba 1").unwrap();
        let pos_up = st.sent.iter().rposition(|l| l == "mba 0").unwrap();
        assert!(pos_down < pos_up);
    }

    struct Dead;
    impl Transport for Dead {
        fn name(&self) -> &str {
            "dead"
        }
        fn send(&mut self, _l: &str, wait: bool) -> Result<Option<String>> {
            if wait {
                Err(Error::Transport("puerto desaparecido".into()))
            } else {
                Ok(None)
            }
        }
        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
        fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reconnects_after_link_failure() {
        let n = Arc::new(Mutex::new(0));
        let s = Session::spawn(
            Box::new(move || {
                let mut c = n.lock().unwrap();
                *c += 1;
                match *c {
                    1 => Ok(Box::new(Dead) as Box<dyn Transport>),
                    2 => Err(Error::DeviceNotFound("desenchufado".into())),
                    _ => Ok(Box::new(MockTransport::new()) as Box<dyn Transport>),
                }
            }),
            "us",
            None,
        )
        .unwrap();
        let mut seen = Vec::new();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(30) {
            if let Ok(e) = s.events.recv_timeout(Duration::from_millis(100)) {
                let done = matches!(e, Event::Status { .. }) && seen.iter().filter(|x: &&String| x.starts_with('C')).count() >= 2;
                seen.push(match &e {
                    Event::Link(LinkState::Connected { .. }) => "C".to_string(),
                    Event::Link(LinkState::Reconnecting { .. }) => "D".to_string(),
                    other => format!("{other:?}"),
                });
                if done {
                    break;
                }
            }
        }
        // conecta, cae, no encuentra, vuelve a conectar y sondea
        let seq: String = seen.iter().filter(|x| x.len() == 1).cloned().collect();
        assert_eq!(seq, "CDDC", "{seen:?}");
    }

    #[test]
    fn combo_and_type_use_target_layout() {
        let (s, h) = mock_session();
        let _ = s.events.recv_timeout(Duration::from_secs(20));
        s.send(Cmd::Combo("ctrl+alt+del".into()));
        wait_until(|| h.lock().sent.iter().any(|l| l == "ku 0xE0"));
        let st = h.lock();
        let kd: Vec<u8> = st.kd_usages();
        assert_eq!(kd, [0xE0, 0xE2, 0x4C]);
    }

    #[test]
    fn works_over_simulated_ble_with_state_events_and_reconnection() {
        std::env::set_var("DKVM_ABS_BUTTONS", "abs"); // estas pruebas miden el camino absoluto original
        use crate::transport::ble::BleOptions;
        let slot: Arc<Mutex<Option<(crate::transport::ble::BleTransport, crate::ble_v1::sim::Sim, crate::transport::ble::mockble::MockDongle)>>> =
            Arc::new(Mutex::new(Some(crate::transport::select::ble_sim(BleOptions { backoff: vec![Duration::from_millis(5)], ..BleOptions::default() }))));
        let (sim, link) = {
            let g = slot.lock().unwrap();
            let t = g.as_ref().unwrap();
            (t.1.clone(), t.2.clone())
        };
        let s2 = slot.clone();
        let s = Session::spawn(
            Box::new(move || match s2.lock().unwrap().take() {
                Some((t, _, _)) => Ok(Box::new(t) as Box<dyn Transport>),
                None => Err(Error::DeviceNotFound("ya consumido".into())),
            }),
            "es",
            None,
        )
        .unwrap();
        // eventos: enlace conectado, estado del dongle (LEDs/modo/host), RTT por ping
        let mut got_link = false;
        let mut got_status = false;
        let mut got_rtt = false;
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(30) && !(got_link && got_status && got_rtt) {
            match s.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Link(LinkState::Connected { .. })) => got_link = true,
                Ok(Event::Status { host: true, hidready: true, mode: Some(m), .. }) => {
                    assert_eq!(m, "C");
                    got_status = true
                }
                Ok(Event::Rtt(_)) => got_rtt = true,
                _ => {}
            }
        }
        assert!(got_link && got_status && got_rtt, "{got_link} {got_status} {got_rtt}");
        // entrada: tecla + movimiento + botón llegan al dongle simulado en orden y con el estado correcto
        s.send(Cmd::Key { usage: 0x04, down: true });
        s.send(Cmd::MouseAbs { x: 100, y: 200 });
        s.send(Cmd::Button { name: "left", down: true, rel: false });
        wait_until(|| sim.state().buttons_abs == 1);
        let st = sim.state();
        assert!(st.keys.contains(&4));
        assert_eq!(st.abs, (100, 200));
        // se pierde el enlace: el dongle suelta todo (lo hace el firmware; aquí RELEASE_ALL al reconectar) y el cliente vuelve solo
        link.drop_link("flasheo");
        let t = Instant::now();
        let mut reconnecting = false;
        let mut back = false;
        while t.elapsed() < Duration::from_secs(30) && !back {
            match s.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Link(LinkState::Reconnecting { .. })) => reconnecting = true,
                Ok(Event::Link(LinkState::Connected { .. })) if reconnecting => back = true,
                _ => {}
            }
        }
        assert!(reconnecting && back);
        s.send(Cmd::Key { usage: 0x05, down: true });
        wait_until(|| sim.state().keys.contains(&5));
    }

    /// Modo ABSOLUTO: un clic (down+up sin movimiento entre medias) llega siempre, en la posición del clic y en orden,
    /// aunque haya una tormenta de movimiento alrededor (fusión de 7 ms del hilo BLE y fusión de la sesión).
    #[test]
    fn absolute_click_is_never_lost_or_reordered_by_motion_merging() {
        abs_click_rounds(false); // camino original: botones por el informe absoluto (DKVM_ABS_BUTTONS=abs)
    }

    /// Workaround por defecto: ma x y -> mbr 1 -> ma x y -> mbr 0 (los botones salen por el ratón relativo).
    #[test]
    fn absolute_click_via_relative_buttons_default() {
        abs_click_rounds(true);
    }

    fn abs_click_rounds(via_rel: bool) {
        use crate::ble_v1::{split_records, BUTTONS_ABS, BUTTONS_REL, MOUSE_ABS};
        use crate::transport::ble::BleOptions;
        let btn_op = if via_rel { BUTTONS_REL } else { BUTTONS_ABS };
        let (t, _sim, link) = crate::transport::select::ble_sim(BleOptions::default());
        let slot = Arc::new(Mutex::new(Some(t)));
        let s = Session::spawn_with(
            Box::new(move || slot.lock().unwrap().take().map(|t| Box::new(t) as Box<dyn Transport>).ok_or(Error::DeviceNotFound("consumido".into()))),
            "es",
            None,
            SessionOpts { abs_buttons_via_rel: via_rel },
        )
        .unwrap();
        wait_link(&s);
        let parse = |link: &crate::transport::ble::mockble::MockDongle| -> Vec<(u8, Vec<u8>)> {
            link.writes().iter().flat_map(|w| split_records(w).0.into_iter().map(|(o, p)| (o, p.to_vec())).collect::<Vec<_>>()).collect()
        };
        for round in 0..30i64 {
            // movimiento, clic sin movimiento entre down y up, más movimiento: todo pegado (sin esperas)
            for i in 0..20 {
                s.send(Cmd::MouseAbs { x: 1000 + i, y: 2000 });
            }
            s.send(Cmd::MouseAbs { x: 5000 + round, y: 6000 });
            s.send(Cmd::Button { name: "left", down: true, rel: false });
            s.send(Cmd::Button { name: "left", down: false, rel: false });
            for i in 0..20 {
                s.send(Cmd::MouseAbs { x: 7000 + i, y: 8000 });
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        wait_until(|| parse(&link).iter().filter(|(o, p)| *o == btn_op && p == &[0]).count() >= 30);
        let recs = parse(&link);
        let downs: Vec<usize> = recs.iter().enumerate().filter(|(_, (o, p))| *o == btn_op && p == &[1]).map(|(i, _)| i).collect();
        let ups: Vec<usize> = recs.iter().enumerate().filter(|(_, (o, p))| *o == btn_op && p == &[0]).map(|(i, _)| i).collect();
        assert_eq!(downs.len(), 30, "botón abajo perdido");
        assert_eq!(ups.len(), 30, "botón arriba perdido");
        for (d, u) in downs.iter().zip(&ups) {
            assert!(d < u, "down después de up");
            // el clic va en su posición: el último MOUSE_ABS antes del down es el de (5000+round, 6000)
            let last_abs = recs[..*d].iter().rev().find(|(o, _)| *o == MOUSE_ABS).expect("movimiento antes del clic");
            let (x, y) = (u16::from_le_bytes([last_abs.1[0], last_abs.1[1]]), u16::from_le_bytes([last_abs.1[2], last_abs.1[3]]));
            assert!((5000..5030).contains(&(x as i64)) && y == 6000, "clic fuera de posición: ({x},{y})");
        }
    }

    // ---------- pegado de texto, envío de teclas y repetición (BLE simulado) ----------

    type SimRig = (Session, crate::ble_v1::sim::Sim, crate::transport::ble::mockble::MockDongle);

    fn sim_rig(layout: &str) -> SimRig {
        use crate::transport::ble::BleOptions;
        let (t, sim, link) = crate::transport::select::ble_sim(BleOptions::default());
        let slot = Arc::new(Mutex::new(Some(t)));
        let s = Session::spawn_with(
            Box::new(move || slot.lock().unwrap().take().map(|t| Box::new(t) as Box<dyn Transport>).ok_or(Error::DeviceNotFound("consumido".into()))),
            layout,
            None,
            SessionOpts { abs_buttons_via_rel: true },
        )
        .unwrap();
        wait_link(&s);
        (s, sim, link)
    }

    /// Espera (hasta 20 s) al evento de enlace conectado de la sesión.
    fn wait_link(s: &Session) {
        let t0 = Instant::now();
        loop {
            match s.events.recv_timeout(Duration::from_millis(50)) {
                Ok(Event::Link(LinkState::Connected { .. })) => return,
                _ => assert!(t0.elapsed() < Duration::from_secs(20), "la sesión no conectó"),
            }
        }
    }

    fn records(link: &crate::transport::ble::mockble::MockDongle) -> Vec<(u8, Vec<u8>)> {
        link.writes().iter().flat_map(|w| crate::ble_v1::split_records(w).0.into_iter().map(|(o, p)| (o, p.to_vec())).collect::<Vec<_>>()).collect()
    }

    fn wait_done(s: &Session, secs: u64) -> (String, Option<String>, bool) {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(secs) {
            if let Ok(Event::JobDone { summary, error, cancelled, .. }) = s.events.recv_timeout(Duration::from_millis(50)) {
                return (summary, error, cancelled);
            }
        }
        panic!("el trabajo no terminó en {secs} s");
    }

    fn typed_usages(link: &crate::transport::ble::mockble::MockDongle) -> Vec<u8> {
        records(link).into_iter().filter(|(o, p)| *o == crate::ble_v1::KEY_DOWN && !(0xE0..=0xE7).contains(&p[0])).map(|(_, p)| p[0]).collect()
    }

    /// Texto largo (varios KB, con saltos de línea CRLF, tabuladores y caracteres del layout es): todas las pulsaciones
    /// llegan, en orden, sin pérdidas ni ERR, y al final no queda nada pulsado.
    #[test]
    fn long_paste_arrives_complete_and_in_order() {
        let (s, sim, link) = sim_rig("es");
        let mut text = String::new();
        let mut i = 0;
        while text.len() < 9000 {
            text.push_str(&format!("línea {i}: ñandú ¿qué? @#~|\\{{}}[] 0123456789\tfin\r\n"));
            i += 1;
        }
        let lay = crate::layouts::get_layout("es").unwrap();
        let (strokes, bad) = lay.text_strokes_lossy(&text);
        assert!(bad.is_empty(), "{bad:?}");
        let expected: Vec<u8> = strokes.iter().map(|(u, _)| *u as u8).collect();
        s.send(Cmd::Paste(PasteJob { text: text.clone(), layout: "es".into(), per_key_ms: 0 }));
        let (summary, error, cancelled) = wait_done(&s, 60);
        assert!(error.is_none() && !cancelled, "{summary} {error:?}");
        assert_eq!(typed_usages(&link), expected, "orden o pérdida");
        // cada KEY_DOWN tiene su KEY_UP y no queda nada pulsado
        let recs = records(&link);
        let downs = recs.iter().filter(|(o, _)| *o == crate::ble_v1::KEY_DOWN).count();
        let ups = recs.iter().filter(|(o, _)| *o == crate::ble_v1::KEY_UP).count();
        assert_eq!(downs, ups);
        assert!(sim.state().keys.is_empty(), "teclas pegadas");
        assert!(!summary.contains("NO representables"));
    }

    /// Los no representables se cuentan y listan, y el resto sí se teclea.
    #[test]
    fn paste_reports_unrepresentable_characters() {
        let (s, _sim, link) = sim_rig("us");
        s.send(Cmd::Paste(PasteJob { text: "a漢b€c".into(), layout: "us".into(), per_key_ms: 0 }));
        let (summary, error, _) = wait_done(&s, 10);
        assert!(error.is_none());
        assert!(summary.contains("2 NO representables") && summary.contains("漢") && summary.contains("€"), "{summary}");
        assert_eq!(typed_usages(&link), vec![0x04, 0x05, 0x06]); // a b c
    }

    #[test]
    fn paste_can_be_cancelled_and_releases_everything() {
        let (s, sim, link) = sim_rig("us");
        s.send(Cmd::Paste(PasteJob { text: "x".repeat(2000), layout: "us".into(), per_key_ms: 10 }));
        wait_until(|| typed_usages(&link).len() >= 8);
        s.cancel_job();
        let (summary, error, cancelled) = wait_done(&s, 10);
        assert!(cancelled && error.is_none(), "{summary}");
        let n = typed_usages(&link).len();
        assert!(n > 0 && n < 2000, "{n}");
        assert!(sim.state().keys.is_empty());
        assert!(records(&link).iter().any(|(o, _)| *o == crate::ble_v1::RELEASE_ALL));
    }

    /// Un ERR "busy" del dongle durante el pegado: se suelta todo y se reintenta el lote (se avisa en el resumen).
    #[test]
    fn paste_retries_on_busy_error() {
        let (s, _sim, link) = sim_rig("us");
        s.send(Cmd::Paste(PasteJob { text: "abcdefghijklmnopqrstuvwxyz".into(), layout: "us".into(), per_key_ms: 20 }));
        wait_until(|| !typed_usages(&link).is_empty());
        link.notify(&crate::ble_v1::record(crate::ble_v1::ERR, &[crate::ble_v1::KEY_DOWN, 1])); // BUSY
        let (summary, error, cancelled) = wait_done(&s, 20);
        assert!(error.is_none() && !cancelled, "{summary} {error:?}");
        assert!(summary.contains("reintentos"), "{summary}");
        // ningún carácter se pierde (puede haber alguno repetido por el reintento)
        let got = typed_usages(&link);
        for u in 0x04u8..=0x1D {
            assert!(got.contains(&u), "falta el uso {u:#x}");
        }
    }

    /// Un ERR que no es "busy" (sin host / sin HID) aborta con mensaje en vez de teclear a ciegas.
    #[test]
    fn paste_aborts_on_non_busy_error() {
        let (s, sim, _link) = sim_rig("us");
        sim.set_flags(0); // host no enumerado (el simulado lo aplica ya a las siguientes órdenes)
        s.send(Cmd::Paste(PasteJob { text: "x".repeat(400), layout: "us".into(), per_key_ms: 5 }));
        let (_, error, _) = wait_done(&s, 20);
        assert!(error.is_some(), "debía abortar con error");
    }

    #[test]
    fn keys_sequence_releases_everything_and_reports() {
        let (s, sim, link) = sim_rig("es");
        s.send(Cmd::Keys("ctrl+alt+del alt+f4 enter".into()));
        let (summary, error, _) = wait_done(&s, 10);
        assert!(error.is_none() && summary.contains("ctrl+alt+del"), "{summary}");
        let downs: Vec<u8> = records(&link).into_iter().filter(|(o, _)| *o == crate::ble_v1::KEY_DOWN).map(|(_, p)| p[0]).collect();
        for u in [0xE0u8, 0xE2, 0x4C, 0x3D, 0x28] {
            assert!(downs.contains(&u), "falta {u:#x} en {downs:x?}");
        }
        assert!(sim.state().keys.is_empty());
        // combinación inválida: error claro y también se suelta todo
        s.send(Cmd::Keys("ctrl+zzz".into()));
        let (_, error, _) = wait_done(&s, 10);
        assert!(error.is_some());
        assert!(sim.state().keys.is_empty());
        assert!(records(&link).iter().any(|(o, _)| *o == crate::ble_v1::RELEASE_ALL));
    }

    #[test]
    fn repeat_presses_many_times_and_stops_on_cancel() {
        let (s, sim, link) = sim_rig("us");
        s.send(Cmd::Repeat { combo: "f2".into(), interval_ms: 20, secs: 0.4 });
        let (summary, error, cancelled) = wait_done(&s, 10);
        assert!(error.is_none() && !cancelled, "{summary}");
        let n = typed_usages(&link).iter().filter(|u| **u == 0x3B).count();
        assert!((8..=21).contains(&n), "{n} pulsaciones de F2 ({summary})");
        s.send(Cmd::Repeat { combo: "del".into(), interval_ms: 20, secs: 60.0 });
        wait_until(|| typed_usages(&link).iter().filter(|u| **u == 0x4C).count() >= 3);
        s.cancel_job();
        let (_, error, cancelled) = wait_done(&s, 10);
        assert!(cancelled && error.is_none());
        assert!(sim.state().keys.is_empty());
    }

    #[test]
    fn set_layout_changes_what_paste_uses_and_text_command() {
        let (s, _sim, link) = sim_rig("us");
        s.send(Cmd::SetLayout("de".into()));
        s.send(Cmd::Type("z".into()));
        wait_until(|| !typed_usages(&link).is_empty());
        // en de la z está en la tecla Y de EE. UU. (0x1C)
        assert_eq!(typed_usages(&link), vec![0x1C]);
    }
}
