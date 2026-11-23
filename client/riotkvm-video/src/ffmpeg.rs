//! Backend ffmpeg: tubería rawvideo RGBA con búfer mínimo.
//!
//! `ffmpeg -fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0 -f dshow ... -f rawvideo -`
//! Requiere `ffmpeg` en el PATH (o `DKVM_FFMPEG=ruta`). Es el comparador/alternativa de `native`.

use crate::{Frame, Latest, Mode};
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

/// Ruta de ffmpeg: `DKVM_FFMPEG`, o búsqueda manual en el PATH. Rust no resuelve los enlaces de
/// WinGet (`...\WinGet\Linksfmpeg.exe`, reparse point) al lanzar procesos; aquí sí.
pub fn exe() -> String {
    if let Ok(v) = std::env::var("DKVM_FFMPEG") {
        return v;
    }
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let c = dir.join(name);
            if std::fs::symlink_metadata(&c).is_ok() {
                return c.to_string_lossy().into_owned();
            }
        }
    }
    #[cfg(windows)]
    {
        // `where` resuelve entradas de PATH con variables sin expandir y los enlaces de WinGet
        if let Ok(o) = Command::new("where.exe").arg("ffmpeg").stdin(Stdio::null()).output() {
            if let Some(l) = String::from_utf8_lossy(&o.stdout).lines().next() {
                if !l.trim().is_empty() {
                    return l.trim().to_string();
                }
            }
        }
    }
    "ffmpeg".into()
}

/// Lista dispositivos de vídeo dshow (Windows) parseando `ffmpeg -list_devices`.
pub fn list() -> Vec<crate::DeviceInfo> {
    let out = Command::new(exe())
        .args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .stdin(Stdio::null())
        .output();
    let Ok(out) = out else { return vec![] };
    let text = String::from_utf8_lossy(&out.stderr);
    let mut v = Vec::new();
    for line in text.lines() {
        if let (Some(a), true) = (line.find('"'), line.trim_end().ends_with("(video)")) {
            if let Some(b) = line[a + 1..].find('"') {
                let name = line[a + 1..a + 1 + b].to_string();
                let (virtual_cam, capture_card) = crate::classify(&name);
                v.push(crate::DeviceInfo { backend: crate::Backend::Ffmpeg, index: v.len(), name, node: String::new(), caps: String::new(), virtual_cam, capture_card });
            }
        }
    }
    v
}

/// Argumentos de entrada para un dispositivo dshow con baja latencia.
pub fn dshow_input(name: &str, mode: &Mode) -> Vec<String> {
    let mut a: Vec<String> = ["-fflags", "nobuffer", "-flags", "low_delay", "-probesize", "32", "-analyzeduration", "0", "-f", "dshow", "-rtbufsize", "4M"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if mode.format == "yuyv" {
        a.extend(["-pixel_format".into(), "yuyv422".into()]);
    } else {
        a.extend(["-vcodec".into(), "mjpeg".into()]);
    }
    a.extend([
        "-video_size".into(),
        format!("{}x{}", mode.w, mode.h),
        "-framerate".into(),
        mode.fps.to_string(),
        "-i".into(),
        format!("video={name}"),
    ]);
    a
}

pub struct Pipe {
    pub child: Child,
    out: ChildStdout,
    pub w: usize,
    pub h: usize,
}

impl Pipe {
    /// `input`: argumentos de entrada (`-f dshow ... -i ...`). La salida es siempre rgba rawvideo.
    pub fn spawn(input: &[String], w: usize, h: usize) -> Result<Pipe, String> {
        let mut c = Command::new(exe());
        c.args(["-hide_banner", "-loglevel", "error"]).args(input);
        c.args(["-an", "-pix_fmt", "rgba", "-f", "rawvideo", "-flush_packets", "1", "-"]);
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = c.spawn().map_err(|e| format!("no se pudo lanzar ffmpeg ({}): {e}", exe()))?;
        let out = child.stdout.take().unwrap();
        Ok(Pipe { child, out, w, h })
    }

    pub fn read_frame(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        self.out.read_exact(buf)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn run(name: &str, mode: &Mode, latest: &Latest) -> Result<(), String> {
    let (w, h) = (mode.w as usize, mode.h as usize);
    let mut pipe = Pipe::spawn(&dshow_input(name, mode), w, h)?;
    latest.set_info(format!("ffmpeg {} {}x{}@{}", mode.format, w, h, mode.fps));
    let mut seq = 0u64;
    while !latest.stopped() {
        let mut rgba = vec![0u8; w * h * 4];
        latest.set_child_cpu(crate::stats::cpu_child(&pipe.child));
        pipe.read_frame(&mut rgba).map_err(|e| format!("ffmpeg terminó o no entrega cuadros: {e}"))?;
        latest.publish(Frame { w: w as u32, h: h as u32, rgba, seq, arrived: Instant::now(), decode: Duration::ZERO });
        seq += 1;
    }
    Ok(())
}
