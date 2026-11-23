//! Estadística global de la ruta de salida BLE (GUI y bancos).
//!
//! - `mark_event()`: quien genera una entrada (hook, UI) la llama justo antes de encolarla; se guarda el instante del
//!   evento MÁS ANTIGUO aún no enviado.
//! - El hilo BLE, al volver `write_cmd`, registra: evento->write (latencia de envío real del cliente), encolado->write
//!   y duración de la llamada. Con Write Without Response "write" = la pila de Windows aceptó el paquete.
//! Una muestra por escritura; anillo de 4096 muestras para p50/p99.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static T0: OnceLock<Instant> = OnceLock::new();
static EVENT_NS: AtomicU64 = AtomicU64::new(0);
static N: AtomicU64 = AtomicU64::new(0);
static SUM_Q_US: AtomicU64 = AtomicU64::new(0);
static MAX_Q_US: AtomicU64 = AtomicU64::new(0);
static LAST_Q_US: AtomicU64 = AtomicU64::new(0);
static SUM_W_US: AtomicU64 = AtomicU64::new(0);
static MAX_W_US: AtomicU64 = AtomicU64::new(0);
static EV: Mutex<Vec<u32>> = Mutex::new(Vec::new()); // evento->write, µs
static EV_POS: AtomicU64 = AtomicU64::new(0);
const RING: usize = 4096;

fn now_ns() -> u64 {
    T0.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1
}

/// Marca el instante de un evento de entrada (solo si no hay otro pendiente: se conserva el más antiguo).
pub fn mark_event() {
    let _ = EVENT_NS.compare_exchange(0, now_ns(), Relaxed, Relaxed);
}

fn take_event() -> Option<Duration> {
    let ns = EVENT_NS.swap(0, Relaxed);
    if ns == 0 {
        None
    } else {
        Some(Duration::from_nanos(now_ns().saturating_sub(ns)))
    }
}

pub fn record_write(queued: Option<Duration>, write: Duration) {
    N.fetch_add(1, Relaxed);
    let w = write.as_micros() as u64;
    SUM_W_US.fetch_add(w, Relaxed);
    MAX_W_US.fetch_max(w, Relaxed);
    if let Some(q) = queued {
        let q = q.as_micros() as u64;
        SUM_Q_US.fetch_add(q, Relaxed);
        MAX_Q_US.fetch_max(q, Relaxed);
        LAST_Q_US.store(q, Relaxed);
    }
    if let Some(e) = take_event() {
        if let Ok(mut v) = EV.lock() {
            let i = EV_POS.fetch_add(1, Relaxed) as usize % RING;
            let us = e.as_micros().min(u32::MAX as u128) as u32;
            if v.len() < RING {
                v.push(us);
            } else {
                v[i] = us;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WriteStats {
    pub writes: u64,
    pub queue_avg_ms: f64,
    pub queue_max_ms: f64,
    pub queue_last_ms: f64,
    pub write_avg_ms: f64,
    pub write_max_ms: f64,
    /// evento->write (solo escrituras con evento marcado)
    pub ev_n: u64,
    pub ev_p50_ms: f64,
    pub ev_p99_ms: f64,
    pub ev_max_ms: f64,
}

pub fn snapshot() -> WriteStats {
    let n = N.load(Relaxed).max(1) as f64;
    let (mut p50, mut p99, mut mx, mut en) = (0.0, 0.0, 0.0, 0u64);
    if let Ok(v) = EV.lock() {
        if !v.is_empty() {
            let mut s = v.clone();
            s.sort_unstable();
            en = EV_POS.load(Relaxed);
            p50 = s[s.len() / 2] as f64 / 1e3;
            p99 = s[(s.len() * 99 / 100).min(s.len() - 1)] as f64 / 1e3;
            mx = *s.last().unwrap() as f64 / 1e3;
        }
    }
    WriteStats {
        writes: N.load(Relaxed),
        queue_avg_ms: SUM_Q_US.load(Relaxed) as f64 / n / 1e3,
        queue_max_ms: MAX_Q_US.load(Relaxed) as f64 / 1e3,
        queue_last_ms: LAST_Q_US.load(Relaxed) as f64 / 1e3,
        write_avg_ms: SUM_W_US.load(Relaxed) as f64 / n / 1e3,
        write_max_ms: MAX_W_US.load(Relaxed) as f64 / 1e3,
        ev_n: en,
        ev_p50_ms: p50,
        ev_p99_ms: p99,
        ev_max_ms: mx,
    }
}

pub fn reset() {
    for a in [&N, &SUM_Q_US, &MAX_Q_US, &LAST_Q_US, &SUM_W_US, &MAX_W_US, &EV_POS, &EVENT_NS] {
        a.store(0, Relaxed);
    }
    if let Ok(mut v) = EV.lock() {
        v.clear();
    }
}
