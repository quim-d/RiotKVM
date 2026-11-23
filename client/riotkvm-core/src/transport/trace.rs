//! Instrumentación: timestamps por orden (cola -> escritura -> respuesta), en microsegundos
//! desde el origen del `Trace`. Se registra desde el hilo de E/S, no desde el de entrada.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub seq: u64,
    pub line: String,
    /// El llamante encoló la orden.
    pub t_enq_us: u64,
    /// El hilo de E/S la escribió en el puerto.
    pub t_write_us: u64,
    /// Llegó la respuesta (o el error).
    pub t_reply_us: u64,
    pub ok: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub n: usize,
    pub min: u64,
    pub mean: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
}

impl Stats {
    pub fn from(mut v: Vec<u64>) -> Stats {
        if v.is_empty() {
            return Stats::default();
        }
        v.sort_unstable();
        let at = |q: f64| v[(((v.len() - 1) as f64) * q).round() as usize];
        Stats {
            n: v.len(),
            min: v[0],
            mean: v.iter().sum::<u64>() / v.len() as u64,
            p50: at(0.5),
            p95: at(0.95),
            p99: at(0.99),
            max: *v.last().unwrap(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub count: u64,
    pub coalesced: u64,
    pub errors: u64,
    /// cola de entrada -> escritura en el puerto (µs)
    pub queue_us: Stats,
    /// escritura -> respuesta del dongle (µs)
    pub device_us: Stats,
    /// cola -> respuesta (µs)
    pub total_us: Stats,
}

struct Inner {
    samples: VecDeque<Sample>,
    seq: u64,
    coalesced: u64,
    errors: u64,
}

pub struct Trace {
    epoch: Instant,
    cap: usize,
    inner: Mutex<Inner>,
}

impl Trace {
    pub fn new(cap: usize) -> Trace {
        Trace {
            epoch: Instant::now(),
            cap: cap.max(1),
            inner: Mutex::new(Inner { samples: VecDeque::new(), seq: 0, coalesced: 0, errors: 0 }),
        }
    }

    pub fn us(&self, t: Instant) -> u64 {
        t.saturating_duration_since(self.epoch).as_micros() as u64
    }

    pub fn record(&self, line: String, t_enq: Instant, t_write: Instant, t_reply: Instant, ok: bool) {
        let s = Sample {
            seq: 0,
            line,
            t_enq_us: self.us(t_enq),
            t_write_us: self.us(t_write),
            t_reply_us: self.us(t_reply),
            ok,
        };
        let mut g = self.inner.lock().unwrap();
        g.seq += 1;
        if !ok {
            g.errors += 1;
        }
        let seq = g.seq;
        if g.samples.len() == self.cap {
            g.samples.pop_front();
        }
        g.samples.push_back(Sample { seq, ..s });
    }

    pub fn note_coalesced(&self) {
        self.inner.lock().unwrap().coalesced += 1;
    }

    pub fn samples(&self) -> Vec<Sample> {
        self.inner.lock().unwrap().samples.iter().cloned().collect()
    }

    pub fn summary(&self) -> Summary {
        let g = self.inner.lock().unwrap();
        let q = g.samples.iter().map(|s| s.t_write_us.saturating_sub(s.t_enq_us)).collect();
        let d = g.samples.iter().map(|s| s.t_reply_us.saturating_sub(s.t_write_us)).collect();
        let t = g.samples.iter().map(|s| s.t_reply_us.saturating_sub(s.t_enq_us)).collect();
        Summary {
            count: g.seq,
            coalesced: g.coalesced,
            errors: g.errors,
            queue_us: Stats::from(q),
            device_us: Stats::from(d),
            total_us: Stats::from(t),
        }
    }

    pub fn to_csv(&self) -> String {
        let mut out = String::from("seq,t_enq_us,t_write_us,t_reply_us,ok,line\n");
        for s in self.samples() {
            out.push_str(&format!(
                "{},{},{},{},{},\"{}\"\n",
                s.seq,
                s.t_enq_us,
                s.t_write_us,
                s.t_reply_us,
                s.ok as u8,
                s.line.replace('"', "\"\"")
            ));
        }
        out
    }
}
