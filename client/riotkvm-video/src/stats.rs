//! Estadística de intervalos/latencias (µs) y consumo de CPU del proceso.

#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn summarize(mut v: Vec<f64>) -> Summary {
    if v.is_empty() {
        return Summary::default();
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Summary { n: v.len(), min: v[0], mean: v.iter().sum::<f64>() / v.len() as f64, p50: q(0.5), p95: q(0.95), p99: q(0.99), max: *v.last().unwrap() }
}

impl Summary {
    pub fn json(&self) -> serde_json::Value {
        let r = |x: f64| (x * 10.0).round() / 10.0;
        serde_json::json!({"n": self.n, "min": r(self.min), "mean": r(self.mean), "p50": r(self.p50), "p95": r(self.p95), "p99": r(self.p99), "max": r(self.max)})
    }
}

#[cfg(windows)]
unsafe fn cpu_of(h: windows_sys::Win32::Foundation::HANDLE) -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) == 0 {
        return f64::NAN;
    }
    let ft = |f: &FILETIME| (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) as f64 * 1e-7;
    ft(&k) + ft(&u)
}

/// Tiempo de CPU (usuario+núcleo) acumulado de este proceso, en segundos.
#[cfg(windows)]
pub fn cpu_self() -> f64 {
    unsafe { cpu_of(windows_sys::Win32::System::Threading::GetCurrentProcess()) }
}

/// CPU de un proceso hijo (asa de `std::process::Child`).
#[cfg(windows)]
pub fn cpu_child(child: &std::process::Child) -> f64 {
    use std::os::windows::io::AsRawHandle;
    unsafe { cpu_of(child.as_raw_handle() as _) }
}

#[cfg(not(windows))]
pub fn cpu_self() -> f64 {
    f64::NAN
}
#[cfg(not(windows))]
pub fn cpu_child(_c: &std::process::Child) -> f64 {
    f64::NAN
}
