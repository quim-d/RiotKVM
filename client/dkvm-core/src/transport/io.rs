//! Transporte de línea con hilo de E/S dedicado (serie CDC hoy; sirve para cualquier `Link`).
//!
//! Latencia:
//! - El hilo del llamante solo hace `Instant::now()` y un `mpsc::send` (sin esperar al puerto).
//! - El hilo de E/S escribe por lotes: todo lo encolado (hasta la ventana) sale en UNA escritura.
//! - Ráfagas sin esperar el `ok`: hasta `window` (16) órdenes en vuelo.
//! - Mientras la ventana está llena, las órdenes de movimiento consecutivas que esperan en cola
//!   se fusionan (`ma` absoluto: gana la última; `mm` relativo: se suman si caben en ±127).
//! - Cada orden registra timestamps cola -> escritura -> respuesta en `Trace` (off del hilo de entrada).

use super::trace::Trace;
use super::Transport;
use crate::error::{device_error, Error, Result};
use std::collections::VecDeque;
use std::io;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Canal de bytes bidireccional hacia el dongle.
pub trait Link: Send {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    /// Lee lo que haya; `Ok(0)` = nada todavía (timeout corto del puerto).
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Si `read` no bloquea (fakes), pausa a insertar cuando no llegó nada.
    fn idle_hint(&self) -> Option<Duration> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct IoConfig {
    pub name: String,
    /// Órdenes en vuelo sin respuesta como máximo.
    pub window: usize,
    pub reply_timeout: Duration,
    pub coalesce: bool,
}

impl Default for IoConfig {
    fn default() -> Self {
        IoConfig { name: "serial".into(), window: 16, reply_timeout: Duration::from_secs(1), coalesce: true }
    }
}

type Reply = Sender<Result<String>>;

struct Pending {
    line: String,
    reply: Option<Reply>,
    t_enq: Instant,
    t_sent: Instant,
    /// Líneas de respuesta que produce la orden (stats = 2) y las ya recibidas.
    need: usize,
    got: Vec<String>,
}

impl Pending {
    fn new(line: String, reply: Option<Reply>, t_enq: Instant) -> Pending {
        let need = crate::protocol::reply_lines(&line);
        Pending { line, reply, t_enq, t_sent: t_enq, need, got: Vec::new() }
    }
}

enum Cmd {
    Line { line: String, reply: Option<Reply>, t_enq: Instant },
    Flush(Sender<Result<()>>),
    Close(Sender<()>),
}

pub struct IoTransport {
    name: String,
    tx: Option<Sender<Cmd>>,
    join: Option<JoinHandle<()>>,
    trace: Arc<Trace>,
}

impl IoTransport {
    pub fn new(link: Box<dyn Link>, cfg: IoConfig) -> IoTransport {
        let (tx, rx) = channel();
        let trace = Arc::new(Trace::new(65536));
        let t2 = trace.clone();
        let name = cfg.name.clone();
        let join = std::thread::Builder::new()
            .name("dkvm-io".into())
            .spawn(move || worker(link, rx, cfg, t2))
            .expect("no se pudo crear el hilo de E/S");
        IoTransport { name, tx: Some(tx), join: Some(join), trace }
    }

    fn tx(&self) -> Result<&Sender<Cmd>> {
        self.tx.as_ref().ok_or_else(|| Error::Transport("transporte cerrado".into()))
    }
}

const GONE: &str = "el hilo de E/S terminó";

impl Transport for IoTransport {
    fn name(&self) -> &str {
        &self.name
    }

    fn send(&mut self, line: &str, wait: bool) -> Result<Option<String>> {
        if line.contains('\n') || line.contains('\r') {
            return Err(Error::Transport("la orden no puede contener saltos de línea".into()));
        }
        let t_enq = Instant::now();
        let (reply, rrx) = if wait {
            let (a, b) = channel();
            (Some(a), Some(b))
        } else {
            (None, None)
        };
        self.tx()?
            .send(Cmd::Line { line: line.to_string(), reply, t_enq })
            .map_err(|_| Error::Transport(GONE.into()))?;
        match rrx {
            Some(r) => r.recv().map_err(|_| Error::Transport(GONE.into()))?.map(Some),
            None => Ok(None),
        }
    }

    fn flush(&mut self) -> Result<()> {
        let (a, b) = channel();
        self.tx()?.send(Cmd::Flush(a)).map_err(|_| Error::Transport(GONE.into()))?;
        b.recv().map_err(|_| Error::Transport(GONE.into()))?
    }

    fn close(&mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            let (a, b) = channel();
            if tx.send(Cmd::Close(a)).is_ok() {
                let _ = b.recv();
            }
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        Ok(())
    }

    fn trace(&self) -> Option<Arc<Trace>> {
        Some(self.trace.clone())
    }
}

impl Drop for IoTransport {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Fusiona `new` en `prev` si son movimientos consecutivos equivalentes. Devuelve true si se fusionó.
fn try_merge(prev: &mut Pending, new_line: &str) -> bool {
    if prev.reply.is_some() {
        return false;
    }
    let a: Vec<&str> = prev.line.split_whitespace().collect();
    let b: Vec<&str> = new_line.split_whitespace().collect();
    if a.len() != 4 || b.len() != 4 || a[0] != b[0] || a[3] != "0" || b[3] != "0" {
        return false;
    }
    match a[0] {
        "ma" => {
            prev.line = new_line.to_string();
            true
        }
        "mm" => {
            let p = |s: &str| s.parse::<i64>().ok();
            let (Some(ax), Some(ay), Some(bx), Some(by)) = (p(a[1]), p(a[2]), p(b[1]), p(b[2])) else {
                return false;
            };
            let (sx, sy) = (ax + bx, ay + by);
            if !(-127..=127).contains(&sx) || !(-127..=127).contains(&sy) {
                return false;
            }
            prev.line = format!("mm {sx} {sy} 0");
            true
        }
        _ => false,
    }
}

// Falla lo que estaba en vuelo/encolado (las órdenes sin espera dejan el error para el flush).
fn fail_all(q: &mut VecDeque<Pending>, first_err: &mut Option<Error>, e: &Error) {
    for p in q.drain(..) {
        match p.reply {
            Some(r) => {
                let _ = r.send(Err(e.clone()));
            }
            None => {
                first_err.get_or_insert_with(|| e.clone());
            }
        }
    }
}

fn worker(mut link: Box<dyn Link>, rx: Receiver<Cmd>, cfg: IoConfig, trace: Arc<Trace>) {
    let window = cfg.window.max(1);
    let mut pending: VecDeque<Pending> = VecDeque::new(); // escritas, esperando respuesta
    let mut backlog: VecDeque<Pending> = VecDeque::new(); // recibidas, aún sin hueco en la ventana
    let mut flushers: Vec<Sender<Result<()>>> = Vec::new();
    let mut closer: Option<Sender<()>> = None;
    let mut first_err: Option<Error> = None;
    let mut dead: Option<Error> = None;
    let mut rbuf: Vec<u8> = Vec::with_capacity(256);
    let mut wbuf: Vec<u8> = Vec::with_capacity(512);
    let mut disconnected = false;

    loop {
        // 1. Entrada: vaciar el canal (bloquea solo si no hay nada en curso).
        while flushers.is_empty() && closer.is_none() && !disconnected {
            let idle = pending.is_empty() && backlog.is_empty();
            let got = if idle { rx.recv().map_err(|_| TryRecvError::Disconnected) } else { rx.try_recv() };
            match got {
                Ok(Cmd::Line { line, reply, t_enq }) => {
                    let p = Pending::new(line, reply, t_enq);
                    if let Some(e) = &dead {
                        let mut one: VecDeque<Pending> = VecDeque::from([p]);
                        fail_all(&mut one, &mut first_err, e);
                    } else if cfg.coalesce && p.reply.is_none() && backlog.back_mut().is_some_and(|b| try_merge(b, &p.line)) {
                        trace.note_coalesced();
                    } else {
                        backlog.push_back(p);
                    }
                }
                Ok(Cmd::Flush(s)) => flushers.push(s),
                Ok(Cmd::Close(s)) => closer = Some(s),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => disconnected = true,
            }
        }

        // 2. Escritura por lotes: todo lo que cabe en la ventana, en UNA escritura.
        let room = window.saturating_sub(pending.len());
        let n = room.min(backlog.len());
        if n > 0 {
            let batch: Vec<Pending> = backlog.drain(..n).collect();
            wbuf.clear();
            for p in &batch {
                wbuf.extend_from_slice(p.line.as_bytes());
                wbuf.push(b'\n');
            }
            let t_w = Instant::now();
            match link.write_all(&wbuf) {
                Ok(()) => {
                    for mut p in batch {
                        p.t_sent = t_w;
                        pending.push_back(p);
                    }
                }
                Err(e) => {
                    let err = Error::Transport(format!("error de E/S en {}: {e}", cfg.name));
                    let mut dq: VecDeque<Pending> = batch.into_iter().collect();
                    fail_all(&mut dq, &mut first_err, &err);
                    fail_all(&mut pending, &mut first_err, &err);
                    fail_all(&mut backlog, &mut first_err, &err);
                    dead = Some(err);
                }
            }
        }

        // 3. Respuestas.
        if !pending.is_empty() {
            let mut tmp = [0u8; 256];
            match link.read(&mut tmp) {
                Ok(0) => {
                    if let Some(d) = link.idle_hint() {
                        std::thread::sleep(d);
                    }
                }
                Ok(k) => rbuf.extend_from_slice(&tmp[..k]),
                Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(e) => {
                    let err = Error::Transport(format!("error de E/S en {}: {e}", cfg.name));
                    fail_all(&mut pending, &mut first_err, &err);
                    fail_all(&mut backlog, &mut first_err, &err);
                    dead = Some(err);
                }
            }
            while let Some(pos) = rbuf.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = rbuf.drain(..=pos).collect();
                let one = String::from_utf8_lossy(&raw).trim().to_string();
                let Some(front) = pending.front_mut() else { continue };
                front.got.push(one);
                if front.got.len() < front.need && !front.got[0].starts_with("err") {
                    continue; // falta alguna línea de la respuesta (stats)
                }
                let p = pending.pop_front().unwrap();
                let reply = p.got.join("\n");
                let is_err = p.got[0].starts_with("err");
                trace.record(p.line.clone(), p.t_enq, p.t_sent, Instant::now(), !is_err);
                let res = if is_err {
                    Err(device_error(&p.line, &reply))
                } else {
                    Ok(reply)
                };
                match p.reply {
                    Some(r) => {
                        let _ = r.send(res);
                    }
                    None => {
                        if let Err(e) = res {
                            first_err.get_or_insert(e);
                        }
                    }
                }
            }
            if pending.front().is_some_and(|f| f.t_sent.elapsed() > cfg.reply_timeout) {
                let err = Error::Transport("timeout esperando la respuesta del dongle".into());
                fail_all(&mut pending, &mut first_err, &err);
                rbuf.clear();
            }
        }

        // 4. Flush / cierre, cuando no queda nada en vuelo ni en cola.
        if pending.is_empty() && backlog.is_empty() {
            if !flushers.is_empty() {
                let e = first_err.take();
                let r = match dead.clone().or(e) {
                    Some(e) => Err(e),
                    None => Ok(()),
                };
                for f in flushers.drain(..) {
                    let _ = f.send(r.clone());
                }
            }
            if let Some(c) = closer.take() {
                let _ = c.send(());
                return;
            }
            if disconnected {
                return;
            }
        }
    }
}
