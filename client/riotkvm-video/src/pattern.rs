//! Patrón de prueba generado: barras de color, rejilla con esquinas marcadas (para comprobar el
//! mapeo de ratón) y un cuadro móvil + contador binario de cuadro (para medir latencia/pérdidas).

use crate::{Frame, Latest, Mode};
use std::time::{Duration, Instant};

pub fn render(buf: &mut [u8], w: usize, h: usize, seq: u64) {
    const BARS: [[u8; 3]; 8] = [
        [235, 235, 235],
        [235, 235, 16],
        [16, 235, 235],
        [16, 235, 16],
        [235, 16, 235],
        [235, 16, 16],
        [16, 16, 235],
        [16, 16, 16],
    ];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let mut c = if y < h / 3 {
                BARS[x * 8 / w]
            } else {
                let g = (40 + (x * 60 / w)) as u8;
                [g, g, g.saturating_add(10)]
            };
            if x % 100 == 0 || y % 100 == 0 {
                c = [200, 200, 60];
            }
            buf[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    // esquinas: rojo (sup-izq), verde (sup-dcha), azul (inf-izq), blanco (inf-dcha)
    let m = 24.min(w / 8).min(h / 8);
    for (cx, cy, c) in [(0, 0, [255, 0, 0]), (w - m, 0, [0, 255, 0]), (0, h - m, [0, 0, 255]), (w - m, h - m, [255, 255, 255])] {
        for y in cy..cy + m {
            for x in cx..cx + m {
                let i = (y * w + x) * 4;
                buf[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
    }
    // cuadro móvil
    let bx = (seq as usize * 6) % (w.saturating_sub(60).max(1));
    let by = h / 2;
    for y in by..(by + 60).min(h) {
        for x in bx..(bx + 60).min(w) {
            let i = (y * w + x) * 4;
            buf[i..i + 4].copy_from_slice(&[255, 128, 0, 255]);
        }
    }
    // contador binario (32 bits) bajo las barras, celdas de 12 px
    for bit in 0..32usize {
        let on = (seq >> bit) & 1 == 1;
        let x0 = w.saturating_sub(12 * 33) + bit * 12;
        for y in (h / 3 + 4)..(h / 3 + 16).min(h) {
            for x in x0..(x0 + 10).min(w) {
                let i = (y * w + x) * 4;
                let v = if on { 255 } else { 0 };
                buf[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
}

pub fn run(mode: &Mode, latest: &Latest) -> Result<(), String> {
    run_for(mode, latest, Duration::from_secs(u64::MAX / 4))
}

pub fn run_for(mode: &Mode, latest: &Latest, dur: Duration) -> Result<(), String> {
    let end = Instant::now() + dur;
    let (w, h) = (mode.w.min(1280) as usize, mode.h.min(720) as usize);
    let period = Duration::from_secs_f64(1.0 / mode.fps.max(1) as f64);
    latest.set_info(format!("patrón {w}x{h}@{}", mode.fps));
    let mut next = Instant::now();
    let mut seq = 0u64;
    while !latest.stopped() && Instant::now() < end {
        let mut rgba = vec![0u8; w * h * 4];
        render(&mut rgba, w, h, seq);
        latest.publish(Frame { w: w as u32, h: h as u32, rgba, seq, arrived: Instant::now(), decode: Duration::ZERO });
        seq += 1;
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    Ok(())
}
