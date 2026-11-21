//! Transporte BLE detrás del trait `Transport`.
//!
//! Un hilo (`dkvm-ble`) es dueño del enlace: escanea por UUID de servicio, conecta, escribe CMD
//! sin respuesta (agrupando varias tramas por escritura hasta la MTU) y recibe EVT. Si el enlace cae se
//! reconecta solo con espera corta (100 ms, 200, 400, 800, 1 s...), como un ratón Bluetooth.
//! El formato de trama lo pone el `Codec` (protocolo "BLE v1" de docs/PROTOCOL.md): este módulo no lo conoce.

pub mod gatt;
pub mod mockble;
#[cfg(feature = "ble")]
pub mod native;
pub mod remember;

pub use gatt::*;

use super::{LinkState, TEvent, Transport};
use crate::error::{Error, Result};
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Escanea dispositivos BLE (`all`: todo lo que anuncie; si no, solo el servicio del dongle).
#[cfg(feature = "ble")]
pub fn scan_devices(window: Duration, all: bool) -> Result<Vec<Seen>> {
    native::scan(window, all)
}
#[cfg(not(feature = "ble"))]
pub fn scan_devices(_window: Duration, _all: bool) -> Result<Vec<Seen>> {
    Err(Error::Usage("este binario se compiló sin soporte BLE (feature `ble`)".into()))
}

/// Qué hacer con una orden codificada.
pub enum Action {
    /// Registros a escribir en CMD (cada elemento es un registro entero: se pueden agrupar).
    Frames(Vec<Vec<u8>>),
    /// Leer la característica INFO (la respuesta sale de `Codec::info_text`).
    ReadInfo,
}

/// Orden codificada.
pub struct Encoded {
    pub action: Action,
    /// La orden espera una respuesta por EVT (status...). Con `ReadInfo` la respuesta es la lectura.
    pub reply: bool,
}

/// Lo que el codec extrae de una notificación EVT.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    /// Respuesta a la orden pendiente más antigua que espera respuesta, ya en el formato de texto del CDC
    /// (`host=1 leds=0x00 ...`, `ok`, `err ...`) para reutilizar el análisis de alto nivel.
    Reply(String),
    /// Estado empujado por el dongle sin que se pida.
    Status(String),
    Pong { seq: u32 },
    /// Error asíncrono notificado por el dongle (se devuelve en el siguiente `flush`).
    Err(String),
}

/// Formato de trama (BLE v1). Lo implementa `dkvm_core::ble_v1` cuando exista la especificación.
pub trait Codec: Send {
    fn name(&self) -> &str;
    /// Orden de alto nivel (línea del protocolo de texto) -> trama.
    fn encode(&mut self, line: &str) -> Result<Encoded>;
    fn decode(&mut self, evt: &[u8]) -> Vec<Decoded>;
    /// Trama de ping con secuencia (`None` si el protocolo no tiene ping).
    fn ping(&mut self, seq: u32) -> Option<Vec<u8>>;
    /// ¿Se pueden concatenar varias tramas en una escritura? (agrupación hasta la MTU)
    fn concat_ok(&self) -> bool {
        false
    }
    /// Fusiona dos registros consecutivos (movimiento de ratón) en uno; `None` si no se pueden fusionar.
    fn merge(&self, _prev: &[u8], _next: &[u8]) -> Option<Vec<u8>> {
        None
    }
    /// Comprueba el INFO leído al conectar (versión de protocolo...). `Err` = no usar este dongle.
    fn check_info(&self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }
    /// INFO crudo -> texto legible.
    fn info_text(&self, bytes: &[u8]) -> String {
        format!("{bytes:02x?}")
    }
}

/// Codec de relleno mientras no exista la especificación: no inventa tramas.
pub struct PendingSpec;
impl Codec for PendingSpec {
    fn name(&self) -> &str {
        "pendiente"
    }
    fn encode(&mut self, _line: &str) -> Result<Encoded> {
        Err(Error::Transport("el protocolo BLE v1 aún no está especificado (docs/PROTOCOL.md): no hay codificador".into()))
    }
    fn decode(&mut self, _evt: &[u8]) -> Vec<Decoded> {
        Vec::new()
    }
    fn ping(&mut self, _seq: u32) -> Option<Vec<u8>> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct BleOptions {
    /// Esperas entre intentos de reconexión.
    pub backoff: Vec<Duration>,
    /// Plazo de `open()` bloqueante (CLI). 0 = no bloquea (GUI).
    pub open_timeout: Duration,
    pub reply_timeout: Duration,
}

impl Default for BleOptions {
    fn default() -> Self {
        BleOptions {
            backoff: [100, 200, 400, 800, 1000].iter().map(|m| Duration::from_millis(*m)).collect(),
            open_timeout: Duration::ZERO,
            reply_timeout: Duration::from_secs(2),
        }
    }
}

struct Shared {
    state: Mutex<LinkState>,
    /// Primer error diferido de envíos sin espera (se devuelve en `flush`).
    pending_err: Mutex<Option<Error>>,
    events: Mutex<VecDeque<TEvent>>,
}

pub struct BleTransport {
    tx: Sender<Inbox>,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
    opts: BleOptions,
}

impl BleTransport {
    /// Arranca el hilo del enlace (no bloquea; el estado inicial es `Searching`).
    pub fn spawn(connector: Box<dyn GattConnector>, codec: Box<dyn Codec>, opts: BleOptions) -> BleTransport {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(LinkState::Searching),
            pending_err: Mutex::new(None),
            events: Mutex::new(VecDeque::new()),
        });
        let (sh, o, tx2) = (shared.clone(), opts.clone(), tx.clone());
        let join = std::thread::Builder::new()
            .name("dkvm-ble".into())
            .spawn(move || link_thread(connector, codec, o, rx, tx2, sh))
            .expect("hilo BLE");
        BleTransport { tx, shared, join: Some(join), opts }
    }

    /// Como `spawn`, pero espera a que haya enlace (CLI) o devuelve `DeviceNotFound` al vencer `timeout`.
    pub fn open(connector: Box<dyn GattConnector>, codec: Box<dyn Codec>, opts: BleOptions, timeout: Duration) -> Result<BleTransport> {
        let mut t = BleTransport::spawn(connector, codec, opts);
        let start = Instant::now();
        loop {
            match t.link_state() {
                Some(LinkState::Connected { .. }) => return Ok(t),
                other => {
                    if start.elapsed() >= timeout {
                        let why = other.map(|s| s.describe()).unwrap_or_default();
                        let _ = t.close();
                        return Err(Error::DeviceNotFound(format!(
                            "no se pudo conectar con el dongle por BLE en {:.0} s ({why}); ¿está cerca y anunciando el servicio {SERVICE_UUID}?",
                            timeout.as_secs_f64()
                        )));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn down(&self) -> Error {
        Error::Transport(format!("sin enlace BLE ({})", self.shared.state.lock().unwrap().describe()))
    }
}

impl Transport for BleTransport {
    fn name(&self) -> &str {
        "ble"
    }

    fn send(&mut self, line: &str, wait: bool) -> Result<Option<String>> {
        if !self.shared.state.lock().unwrap().is_connected() {
            let e = self.down();
            if wait {
                return Err(e);
            }
            self.shared.pending_err.lock().unwrap().get_or_insert(e);
            return Ok(None);
        }
        if wait {
            let (r, rx) = channel();
            self.tx.send(Inbox::Out { line: line.to_string(), reply: Some(r), t: Instant::now() }).map_err(|_| Error::Transport("hilo BLE terminado".into()))?;
            match rx.recv_timeout(self.opts.reply_timeout + Duration::from_secs(1)) {
                Ok(r) => r.map(Some),
                Err(_) => Err(Error::Transport(format!("sin respuesta BLE a {:?}", line))),
            }
        } else {
            self.tx.send(Inbox::Out { line: line.to_string(), reply: None, t: Instant::now() }).map_err(|_| Error::Transport("hilo BLE terminado".into()))?;
            Ok(None)
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.shared.state.lock().unwrap().is_connected() {
            let (r, rx) = channel();
            if self.tx.send(Inbox::Flush(r)).is_ok() {
                if let Ok(res) = rx.recv_timeout(Duration::from_secs(2)) {
                    res?;
                }
            }
        }
        match self.shared.pending_err.lock().unwrap().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn poll_error(&mut self) -> Result<()> {
        match self.shared.pending_err.lock().unwrap().take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn close(&mut self) -> Result<()> {
        let _ = self.tx.send(Inbox::Close);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        Ok(())
    }

    fn poll_events(&mut self) -> Vec<TEvent> {
        self.shared.events.lock().unwrap().drain(..).collect()
    }

    fn self_healing(&self) -> bool {
        true
    }

    fn link_state(&self) -> Option<LinkState> {
        Some(self.shared.state.lock().unwrap().clone())
    }

    fn ping(&mut self) -> Result<Duration> {
        if !self.shared.state.lock().unwrap().is_connected() {
            return Err(self.down());
        }
        let (r, rx) = channel();
        self.tx.send(Inbox::Ping(r)).map_err(|_| Error::Transport("hilo BLE terminado".into()))?;
        rx.recv_timeout(self.opts.reply_timeout + Duration::from_secs(1)).map_err(|_| Error::Transport("sin pong BLE".into()))?
    }
}

impl Drop for BleTransport {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn set_state(sh: &Shared, s: LinkState) {
    let mut g = sh.state.lock().unwrap();
    if *g != s {
        *g = s.clone();
        drop(g);
        sh.events.lock().unwrap().push_back(TEvent::Link(s));
    }
}

struct Waiting {
    reply: Sender<Result<String>>,
    since: Instant,
}

/// Hilo del enlace: conectar -> servir -> (perdido) -> esperar -> conectar...
#[allow(unused_assignments)]
fn link_thread(mut connector: Box<dyn GattConnector>, mut codec: Box<dyn Codec>, opts: BleOptions, rx: Receiver<Inbox>, tx: Sender<Inbox>, sh: Arc<Shared>) {
    crate::rt::boost_current_thread();
    let mut attempt = 0u32;
    let mut seq = 0u32;
    'outer: loop {
        // ---- conectar
        let mut last_reason = String::new();
        let link = loop {
            // atender órdenes de cierre mientras no hay enlace; el resto se descarta (no se acumula entrada vieja)
            loop {
                match rx.try_recv() {
                    Ok(Inbox::Close) => break 'outer,
                    Ok(Inbox::Out { reply: Some(r), .. }) => {
                        let _ = r.send(Err(Error::Transport("sin enlace BLE".into())));
                    }
                    Ok(Inbox::Flush(r)) => {
                        let _ = r.send(Ok(()));
                    }
                    Ok(Inbox::Ping(r)) => {
                        let _ = r.send(Err(Error::Transport("sin enlace BLE".into())));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let mut status = |s: LinkState| set_state(&sh, s);
            match connector.connect(tx.clone(), &mut status) {
                Ok(l) => break l,
                Err(e) => {
                    attempt += 1;
                    last_reason = e.message().to_string();
                    set_state(&sh, LinkState::Reconnecting { attempt, reason: last_reason.clone() });
                    let wait = opts.backoff.get((attempt as usize - 1).min(opts.backoff.len().saturating_sub(1))).copied().unwrap_or(Duration::from_secs(1));
                    // espera interrumpible por Close
                    let t0 = Instant::now();
                    while t0.elapsed() < wait {
                        match rx.recv_timeout(Duration::from_millis(20)) {
                            Ok(Inbox::Close) => break 'outer,
                            Ok(Inbox::Out { reply: Some(r), .. }) => {
                                let _ = r.send(Err(Error::Transport("sin enlace BLE".into())));
                            }
                            Ok(Inbox::Flush(r)) => {
                                let _ = r.send(Ok(()));
                            }
                            _ => {}
                        }
                    }
                }
            }
        };
        let _ = last_reason;
        let mut link = link;
        // compatibilidad de protocolo: se lee INFO antes de dar el enlace por bueno
        if let Ok(info) = link.read_info() {
            if let Err(e) = codec.check_info(&info) {
                link.disconnect();
                attempt += 1;
                set_state(&sh, LinkState::Reconnecting { attempt, reason: e.message().to_string() });
                std::thread::sleep(Duration::from_millis(1000));
                continue 'outer;
            }
        }
        let mtu = link.mtu();

        set_state(&sh, LinkState::Connected { name: link.name(), mtu });
        // un `Lost` de un enlace anterior que quedara en cola no debe tumbar este
        let mut carry: VecDeque<Inbox> = VecDeque::new();
        while let Ok(m) = rx.try_recv() {
            if !matches!(m, Inbox::Lost(_)) {
                carry.push_back(m);
            }
        }
        let mut waiting: VecDeque<Waiting> = VecDeque::new();
        let mut ping: Option<(u32, Instant, Sender<Result<Duration>>)> = None;

        // ---- servir
        let move_gap = Duration::from_micros(std::env::var("DKVM_BLE_MOVE_GAP_US").ok().and_then(|v| v.parse().ok()).unwrap_or(7000));
        let mut last_write = Instant::now() - Duration::from_secs(1);
        let lost: String = 'serve: loop {
            let first = match carry.pop_front() {
                Some(m) => Some(m),
                None => match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break 'outer,
                },
            };
            // caducan respuestas y pings sin contestar
            while let Some(w) = waiting.front() {
                if w.since.elapsed() > opts.reply_timeout {
                    let w = waiting.pop_front().unwrap();
                    let _ = w.reply.send(Err(Error::Transport("sin respuesta del dongle (timeout BLE)".into())));
                } else {
                    break;
                }
            }
            if ping.as_ref().map(|p| p.1.elapsed() > opts.reply_timeout).unwrap_or(false) {
                let p = ping.take().unwrap();
                let _ = p.2.send(Err(Error::Transport("sin pong (timeout BLE)".into())));
            }
            let Some(first) = first else { continue };
            // drenar todo lo pendiente para agrupar en escrituras
            let mut batch = vec![first];
            batch.extend(carry.drain(..));
            while let Ok(m) = rx.try_recv() {
                batch.push(m);
            }
            // Movimiento puro: no se escribe más de una vez por `move_gap` (~medio intervalo BLE). Mientras se espera
            // se sigue fusionando (último destino / suma de deltas): menos paquetes y nada de cola en la pila.
            let motion_only = batch.iter().all(|m| matches!(m, Inbox::Out { line, reply: None, .. } if line.starts_with("mm ") || line.starts_with("ma ")));
            if motion_only {
                let since = last_write.elapsed();
                if since < move_gap {
                    // espera troceada: una tecla/botón/ping que llegue corta la espera y sale ya
                    let until = last_write + move_gap;
                    loop {
                        let left = until.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            break;
                        }
                        match rx.recv_timeout(left) {
                            Ok(m) => {
                                let mo = matches!(&m, Inbox::Out { line, reply: None, .. } if line.starts_with("mm ") || line.starts_with("ma "));
                                batch.push(m);
                                if !mo {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
            let max_payload = mtu.saturating_sub(3).max(20);
            let mut buf: Vec<u8> = Vec::new();
            #[allow(unused_assignments)]
            let mut last_start = 0usize;
            let mut flushes: Vec<Sender<Result<()>>> = Vec::new();
            let mut buf_t: Option<Instant> = None; // encolado del registro más antiguo de `buf`
            macro_rules! write_buf {
                () => {
                    if !buf.is_empty() {
                        let tw = Instant::now();
                        if let Err(e) = link.write_cmd(&buf) {
                            break 'serve e.message().to_string();
                        }
                        last_write = Instant::now();
                        crate::latency::record_write(buf_t.take().map(|t| t.elapsed()), tw.elapsed());
                        buf.clear();
                    }
                    buf_t = None;
                    last_start = 0;
                };
            }
            for m in batch {
                match m {
                    Inbox::Close => {
                        write_buf!();
                        link.disconnect();
                        break 'outer;
                    }
                    Inbox::Lost(r) => break 'serve r,
                    Inbox::Evt(bytes) => {
                        for d in codec.decode(&bytes) {
                            match d {
                                Decoded::Reply(line) => {
                                    if let Some(w) = waiting.pop_front() {
                                        let _ = w.reply.send(Ok(line));
                                    }
                                }
                                Decoded::Status(line) => {
                                    sh.events.lock().unwrap().push_back(TEvent::StatusLine(line));
                                }
                                Decoded::Err(m) => {
                                    sh.pending_err.lock().unwrap().get_or_insert(Error::Device(m));
                                }
                                Decoded::Pong { seq: s } => {
                                    if let Some((want, t, r)) = ping.take() {
                                        if want == s {
                                            let d = t.elapsed();
                                            sh.events.lock().unwrap().push_back(TEvent::Rtt(d.as_micros() as u64));
                                            let _ = r.send(Ok(d));
                                        } else {
                                            ping = Some((want, t, r));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Inbox::Flush(r) => flushes.push(r),
                    Inbox::Ping(r) => {
                        seq = (seq + 1) & 0xFFFF;
                        match codec.ping(seq) {
                            Some(frame) => {
                                write_buf!();
                                ping = Some((seq, Instant::now(), r));
                                if let Err(e) = link.write_cmd(&frame) {
                                    break 'serve e.message().to_string();
                                }
                            }
                            None => {
                                let _ = r.send(Err(Error::Transport("el protocolo BLE no tiene ping".into())));
                            }
                        }
                    }
                    Inbox::Out { line, reply, t: t_enq } => match codec.encode(&line) {
                        Err(e) => match reply {
                            Some(r) => {
                                let _ = r.send(Err(e));
                            }
                            None => {
                                sh.pending_err.lock().unwrap().get_or_insert(e);
                            }
                        },
                        Ok(Encoded { action: Action::ReadInfo, .. }) => {
                            write_buf!();
                            match link.read_info() {
                                Ok(b) => {
                                    if let Some(r) = reply {
                                        let _ = r.send(Ok(codec.info_text(&b)));
                                    }
                                }
                                Err(e) => {
                                    if let Some(r) = reply {
                                        let _ = r.send(Err(e.clone()));
                                    }
                                    break 'serve e.message().to_string();
                                }
                            }
                        }
                        Ok(Encoded { action: Action::Frames(frames), reply: wants }) => {
                            if wants {
                                // las órdenes con respuesta salen solas, en orden
                                write_buf!();
                                if let Some(r) = reply {
                                    waiting.push_back(Waiting { reply: r, since: Instant::now() });
                                }
                                for f in frames {
                                    if let Err(e) = link.write_cmd(&f) {
                                        break 'serve e.message().to_string();
                                    }
                                }
                            } else {
                                if let Some(r) = reply {
                                    let _ = r.send(Ok("ok".into()));
                                }
                                for f in frames {
                                    if !codec.concat_ok() {
                                        write_buf!();
                                        if let Err(e) = link.write_cmd(&f) {
                                            break 'serve e.message().to_string();
                                        }
                                        continue;
                                    }
                                    // fusión de movimiento con el registro anterior de la misma escritura
                                    let merged = if buf.len() > last_start { codec.merge(&buf[last_start..], &f) } else { None };
                                    if let Some(m) = merged {
                                        buf.truncate(last_start);
                                        buf.extend_from_slice(&m);
                                        continue;
                                    }
                                    if buf.len() + f.len() > max_payload {
                                        write_buf!();
                                    }
                                    last_start = buf.len();
                                    buf_t.get_or_insert(t_enq);
                                    buf.extend_from_slice(&f);
                                }
                            }
                        }
                    },
                }
            }
            write_buf!();
            for f in flushes {
                let _ = f.send(Ok(()));
            }
        };

        // ---- enlace perdido: se avisa a quien esperaba y se reconecta ya
        for w in waiting.drain(..) {
            let _ = w.reply.send(Err(Error::Transport(format!("enlace BLE perdido: {lost}"))));
        }
        if let Some(p) = ping.take() {
            let _ = p.2.send(Err(Error::Transport("enlace BLE perdido".into())));
        }
        link.disconnect();
        attempt = 1;
        set_state(&sh, LinkState::Reconnecting { attempt, reason: lost });
    }
    set_state(&sh, LinkState::Searching);
}

#[cfg(test)]
mod tests {
    use super::mockble::MockDongle;
    use super::*;

    /// Codec de PRUEBA (no es el formato BLE v1): trama = [len, bytes de la línea]. Solo ejercita la maquinaria.
    struct TestCodec;
    impl Codec for TestCodec {
        fn name(&self) -> &str {
            "test"
        }
        fn encode(&mut self, line: &str) -> Result<Encoded> {
            let mut f = vec![line.len() as u8];
            f.extend_from_slice(line.as_bytes());
            Ok(Encoded { action: Action::Frames(vec![f]), reply: line == "status" || line == "info" })
        }
        fn decode(&mut self, evt: &[u8]) -> Vec<Decoded> {
            match evt.first() {
                Some(b'R') => vec![Decoded::Reply(String::from_utf8_lossy(&evt[1..]).into())],
                Some(b'S') => vec![Decoded::Status(String::from_utf8_lossy(&evt[1..]).into())],
                Some(b'P') => vec![Decoded::Pong { seq: u32::from_le_bytes(evt[1..5].try_into().unwrap()) }],
                _ => vec![],
            }
        }
        fn ping(&mut self, seq: u32) -> Option<Vec<u8>> {
            let mut f = vec![b'p'];
            f.extend_from_slice(&seq.to_le_bytes());
            Some(f)
        }
        fn concat_ok(&self) -> bool {
            true
        }
    }

    fn fast() -> BleOptions {
        BleOptions { backoff: vec![Duration::from_millis(5), Duration::from_millis(10)], ..BleOptions::default() }
    }

    fn open(d: &MockDongle) -> BleTransport {
        BleTransport::open(d.connector(), Box::new(TestCodec), fast(), Duration::from_secs(2)).unwrap()
    }

    fn wait(f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(3), "timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn connects_and_reports_state() {
        let d = MockDongle::new();
        let mut t = open(&d);
        assert!(matches!(t.link_state(), Some(LinkState::Connected { mtu: 247, .. })));
        let evs = t.poll_events();
        assert!(evs.iter().any(|e| matches!(e, TEvent::Link(LinkState::Connected { .. }))));
        assert!(t.self_healing());
    }

    #[test]
    fn open_times_out_when_dongle_absent() {
        let d = MockDongle::new();
        d.fail_next_connects(1000);
        let e = BleTransport::open(d.connector(), Box::new(TestCodec), fast(), Duration::from_millis(150)).err().unwrap();
        assert!(matches!(e, Error::DeviceNotFound(_)), "{e:?}");
        assert_eq!(e.exit_code(), 3);
    }

    #[test]
    fn batches_frames_up_to_mtu() {
        let d = MockDongle::new();
        d.set_mtu(23); // carga útil 20
        let mut t = open(&d);
        for _ in 0..10 {
            t.send("kd 4", false).unwrap(); // trama de 5 bytes
        }
        t.flush().unwrap();
        let w = d.writes();
        assert!(w.iter().all(|x| x.len() <= 20), "{w:?}");
        assert_eq!(w.iter().map(|x| x.len()).sum::<usize>(), 50, "no se pierde ninguna trama");
        assert!(w.len() < 10, "se agruparon: {} escrituras", w.len());
    }

    #[test]
    fn motion_is_throttled_but_keys_go_out_at_once() {
        let d = MockDongle::new();
        let mut t = open(&d);
        // 60 movimientos a ~1 kHz: no más de ~1 escritura cada 7 ms (con TestCodec no se fusionan, pero sí se agrupan)
        let t0 = Instant::now();
        for _ in 0..60 {
            t.send("mm 1 0", false).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        t.flush().unwrap();
        let dur = t0.elapsed();
        let w = d.writes().len();
        assert!(w as f64 <= dur.as_secs_f64() * 1000.0 / 7.0 + 3.0, "{w} escrituras en {dur:?}");
        assert!(w >= 2);
        // una tecla justo después de movimiento sale sin esperar el hueco de 7 ms
        t.send("mm 1 0", false).unwrap();
        let t1 = Instant::now();
        t.send("kd 4", false).unwrap();
        t.flush().unwrap();
        assert!(t1.elapsed() < Duration::from_millis(6), "tecla retenida {:?}", t1.elapsed());
    }

    #[test]
    fn event_to_write_latency_is_recorded_and_small() {
        let d = MockDongle::new();
        let mut t = open(&d);
        crate::latency::reset();
        for _ in 0..200 {
            crate::latency::mark_event();
            t.send("kd 4", false).unwrap();
            t.flush().unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        let s = crate::latency::snapshot();
        println!("evento->write (sim): p50 {:.2} p99 {:.2} max {:.2} ms, n={}", s.ev_p50_ms, s.ev_p99_ms, s.ev_max_ms, s.ev_n);
        assert!(s.ev_n >= 100 && s.ev_p50_ms < 10.0, "{s:?}");
    }

    #[test]
    fn reply_commands_get_their_evt_reply() {
        let d = MockDongle::new();
        d.set_responder(|w| if &w[1..] == b"status" { vec![b"Rhost=1 leds=0x02 hidready=1".to_vec()] } else { vec![] });
        let mut t = open(&d);
        let r = t.send("status", true).unwrap().unwrap();
        assert_eq!(r, "host=1 leds=0x02 hidready=1");
        assert!(crate::protocol::parse_status(&r).unwrap()["hidready"].as_bool().unwrap());
    }

    #[test]
    fn pushed_status_becomes_event_and_ping_measures_rtt() {
        let d = MockDongle::new();
        d.set_responder(|w| {
            if w[0] == b'p' {
                let mut r = vec![b'P'];
                r.extend_from_slice(&w[1..5]);
                vec![r]
            } else {
                vec![]
            }
        });
        let mut t = open(&d);
        d.notify(b"Shost=1 leds=0x01");
        let rtt = t.ping().unwrap();
        assert!(rtt < Duration::from_secs(1));
        let evs = t.poll_events();
        assert!(evs.contains(&TEvent::StatusLine("host=1 leds=0x01".into())), "{evs:?}");
        assert!(evs.iter().any(|e| matches!(e, TEvent::Rtt(_))));
    }

    #[test]
    fn reconnects_by_itself_after_link_loss_and_backs_off() {
        let d = MockDongle::new();
        let mut t = open(&d);
        assert_eq!(d.connects(), 1);
        d.fail_next_connects(2); // el dongle tarda en volver (reinicio al flashear)
        d.drop_link("el dongle se reinició");
        wait(|| d.connects() == 2 && t.link_state().map(|s| s.is_connected()).unwrap_or(false));
        let evs = t.poll_events();
        let seq: Vec<&str> = evs
            .iter()
            .filter_map(|e| match e {
                TEvent::Link(LinkState::Connected { .. }) => Some("C"),
                TEvent::Link(LinkState::Reconnecting { .. }) => Some("R"),
                _ => None,
            })
            .collect();
        assert_eq!(seq.first(), Some(&"C"));
        assert!(seq.contains(&"R"), "{seq:?}");
        assert_eq!(seq.last(), Some(&"C"));
        t.send("kd 4", false).unwrap();
        t.flush().unwrap();
        assert!(!d.writes().is_empty());
    }

    #[test]
    fn send_while_down_is_dropped_and_flush_reports_it() {
        let d = MockDongle::new();
        let mut t = open(&d);
        d.fail_next_connects(1000);
        d.drop_link("fuera de alcance");
        wait(|| !t.link_state().unwrap().is_connected());
        assert!(t.send("kd 4", false).unwrap().is_none());
        assert!(matches!(t.flush(), Err(Error::Transport(_))));
        assert!(t.send("status", true).is_err());
        assert!(t.ping().is_err());
    }

    #[test]
    fn write_failure_triggers_reconnect() {
        let d = MockDongle::new();
        let mut t = open(&d);
        d.fail_next_write();
        t.send("kd 4", false).unwrap();
        wait(|| d.connects() == 2 && t.link_state().map(|s| s.is_connected()).unwrap_or(false));
    }

    #[test]
    fn pending_spec_codec_refuses_to_invent_frames() {
        let d = MockDongle::new();
        let mut t = BleTransport::open(d.connector(), Box::new(PendingSpec), fast(), Duration::from_secs(2)).unwrap();
        let e = t.send("status", true).unwrap_err();
        assert!(e.message().contains("BLE v1"), "{e}");
    }
}
