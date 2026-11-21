//! Control de alto nivel: teclado y ratón sobre un `Transport`.
//!
//! Ruta de entrada sin colas propias: cada acción se traduce a órdenes y se envía sin esperar
//! el `ok` (`wait=false`), con un `flush` al final de la acción (desactivable con `auto_flush`
//! para la GUI, que no debe bloquear el hilo de entrada).

use crate::error::{usage, Error, Result};
use crate::keys::{mods_to_usages, parse_combo};
use crate::layouts::{get_layout, Layout};
use crate::protocol as P;
use crate::transport::Transport;
use std::time::Duration;

pub fn button_mask(name: &str) -> Result<u8> {
    match name.to_lowercase().as_str() {
        "left" | "l" => Ok(P::BTN_LEFT),
        "right" | "r" => Ok(P::BTN_RIGHT),
        "middle" | "m" => Ok(P::BTN_MIDDLE),
        _ => usage(format!("botón desconocido {}; usa left, right o middle", crate::error::pyrepr(name))),
    }
}

/// Posición en el vídeo mostrado (px, py) -> coordenadas absolutas 0..32767.
pub fn abs_from_pixels(px: f64, py: f64, width: i64, height: i64) -> Result<(i64, i64)> {
    if width < 2 || height < 2 {
        return usage("resolución inválida");
    }
    let f = |p: f64, n: i64| -> i64 {
        (p.max(0.0).min((n - 1) as f64) / (n - 1) as f64 * P::ABS_MAX as f64).round_ties_even() as i64
    };
    Ok((f(px, width), f(py, height)))
}

pub fn parse_screen(text: &str) -> Result<(i64, i64)> {
    let bad = || Error::Usage(format!("--screen debe ser ANCHOxALTO (p. ej. 1920x1080), no {}", crate::error::pyrepr(text)));
    let lower = text.to_lowercase();
    let mut it = lower.split('x');
    let (Some(w), Some(h), None) = (it.next(), it.next(), it.next()) else { return Err(bad()) };
    Ok((w.trim().parse().map_err(|_| bad())?, h.trim().parse().map_err(|_| bad())?))
}

pub struct Kvm {
    pub t: Box<dyn Transport>,
    pub layout: &'static Layout,
    sleep: Box<dyn FnMut(Duration) + Send>,
    /// Máscara de botones mantenidos (modo actual).
    pub buttons: u8,
    pub abs_pos: (i64, i64),
    /// `flush` tras cada acción (CLI). La GUI lo desactiva y llama a `flush()` cuando le conviene.
    pub auto_flush: bool,
}

impl Kvm {
    pub fn new(t: Box<dyn Transport>, layout: &str) -> Result<Kvm> {
        Ok(Kvm {
            t,
            layout: get_layout(layout)?,
            sleep: Box::new(std::thread::sleep),
            buttons: 0,
            abs_pos: (P::ABS_MAX / 2, P::ABS_MAX / 2),
            auto_flush: true,
        })
    }

    /// Cambia la distribución del teclado del servidor (nombre o alias).
    pub fn set_layout(&mut self, name: &str) -> Result<()> {
        self.layout = get_layout(name)?;
        Ok(())
    }

    pub fn set_sleep(&mut self, f: impl FnMut(Duration) + Send + 'static) {
        self.sleep = Box::new(f);
    }

    pub fn sleep(&mut self, secs: f64) {
        if secs > 0.0 && secs.is_finite() {
            (self.sleep)(Duration::from_secs_f64(secs));
        }
    }

    // ---------- bajo nivel ----------
    fn send(&mut self, line: Result<String>) -> Result<()> {
        self.t.send(&line?, false)?;
        Ok(())
    }

    fn done(&mut self) -> Result<()> {
        if self.auto_flush {
            self.t.flush()
        } else {
            Ok(())
        }
    }

    pub fn flush(&mut self) -> Result<()> {
        self.t.flush()
    }

    pub fn raw(&mut self, line: &str) -> Result<String> {
        Ok(self.t.send(line, true)?.unwrap_or_default())
    }

    pub fn info(&mut self) -> Result<String> {
        self.raw(&P::info())
    }

    /// Contadores del dongle (dos líneas: HID y SD) o `reset`.
    pub fn stats(&mut self, reset: bool) -> Result<String> {
        self.raw(&P::stats(reset))
    }

    pub fn status(&mut self) -> Result<serde_json::Map<String, serde_json::Value>> {
        let r = self.raw(&P::status())?;
        P::parse_status(&r)
    }

    // ---------- teclado ----------
    pub fn key_down(&mut self, usage: i64) -> Result<()> {
        self.send(P::key_down(usage))?;
        self.done()
    }

    pub fn key_up(&mut self, usage: i64) -> Result<()> {
        self.send(P::key_up(usage))?;
        self.done()
    }

    pub fn release_all(&mut self) -> Result<()> {
        self.send(Ok(P::release_all_keys()))?;
        self.send(P::buttons_rel(0))?;
        self.send(P::buttons_abs(0))?;
        self.buttons = 0;
        self.done()
    }

    pub(crate) fn tap(&mut self, usage: u16, mods: u8) -> Result<()> {
        let mu = mods_to_usages(mods);
        for m in &mu {
            self.send(P::key_down(*m as i64))?;
        }
        self.send(P::key_down(usage as i64))?;
        self.send(P::key_up(usage as i64))?;
        for m in mu.iter().rev() {
            self.send(P::key_up(*m as i64))?;
        }
        Ok(())
    }

    /// Escribe `text` con el layout del destino (vía kd/ku, no con `type` del firmware).
    pub fn type_text(&mut self, text: &str, delay: f64) -> Result<()> {
        let strokes = self.layout.text_strokes(text)?; // valida todo antes de enviar nada
        let r = (|| -> Result<()> {
            for (usage, mods) in strokes {
                self.tap(usage, mods)?;
                if delay > 0.0 {
                    self.t.flush()?;
                    self.sleep(delay);
                }
            }
            Ok(())
        })();
        let f = self.t.flush();
        r.and(f)
    }

    /// `type` nativo del dongle (layout US fijo en el firmware, una línea por orden).
    pub fn type_text_firmware(&mut self, text: &str) -> Result<()> {
        let norm = text.replace("\r\n", "\n");
        let mut lines: Vec<&str> = norm.split('\n').collect();
        if lines.len() > 1 && lines.last() == Some(&"") {
            lines.pop(); // el '\n' final termina la última línea (no añade un Enter extra)
        }
        for line in lines {
            // el firmware admite 255 bytes por línea (con "type "): se trocea en caracteres enteros
            let mut chunk = String::new();
            for ch in line.chars() {
                if chunk.len() + ch.len_utf8() > P::TYPE_CHUNK {
                    self.send(P::type_text(&chunk))?;
                    chunk.clear();
                }
                chunk.push(ch);
            }
            if !chunk.is_empty() {
                self.send(P::type_text(&chunk))?;
            }
            self.send(P::key_down(0x28))?;
            self.send(P::key_up(0x28))?;
        }
        self.t.flush()
    }

    /// Pulsa y suelta una combinación (`ctrl+alt+del`). Libera siempre al acabar.
    pub fn press(&mut self, combo: &str, hold: f64) -> Result<()> {
        let keys = parse_combo(combo, self.layout)?;
        let mut downs: Vec<u16> = Vec::new();
        let r = (|| -> Result<()> {
            for (usage, mods) in keys {
                for m in mods_to_usages(mods) {
                    if !downs.contains(&m) {
                        self.send(P::key_down(m as i64))?;
                        downs.push(m);
                    }
                }
                if !downs.contains(&usage) {
                    self.send(P::key_down(usage as i64))?;
                    downs.push(usage);
                }
            }
            self.t.flush()?;
            self.sleep(hold);
            Ok(())
        })();
        for u in downs.iter().rev() {
            let _ = self.send(P::key_up(*u as i64));
        }
        let f = self.t.flush();
        r.and(f)
    }

    pub fn key_down_name(&mut self, combo: &str) -> Result<()> {
        for (usage, mods) in parse_combo(combo, self.layout)? {
            for m in mods_to_usages(mods) {
                self.send(P::key_down(m as i64))?;
            }
            self.send(P::key_down(usage as i64))?;
        }
        self.t.flush()
    }

    pub fn key_up_name(&mut self, combo: &str) -> Result<()> {
        for (usage, mods) in parse_combo(combo, self.layout)?.into_iter().rev() {
            self.send(P::key_up(usage as i64))?;
            for m in mods_to_usages(mods).into_iter().rev() {
                self.send(P::key_up(m as i64))?;
            }
        }
        self.t.flush()
    }

    // ---------- ratón ----------
    /// Movimiento relativo; trocea en pasos de ±127 (el informe HID es int8).
    pub fn move_rel(&mut self, dx: i64, dy: i64, wheel: i64) -> Result<()> {
        let (mut dx, mut dy, mut wheel) = (dx, dy, wheel);
        while dx != 0 || dy != 0 || wheel != 0 {
            let (sx, sy, sw) = (dx.clamp(-127, 127), dy.clamp(-127, 127), wheel.clamp(-127, 127));
            self.send(P::mouse_rel(sx, sy, sw))?;
            dx -= sx;
            dy -= sy;
            wheel -= sw;
        }
        self.done()
    }

    pub fn move_abs(&mut self, x: i64, y: i64) -> Result<()> {
        self.send(P::mouse_abs(x, y, 0))?;
        self.abs_pos = (x, y);
        self.done()
    }

    pub fn move_px(&mut self, px: f64, py: f64, width: i64, height: i64) -> Result<()> {
        let (x, y) = abs_from_pixels(px, py, width, height)?;
        self.move_abs(x, y)
    }

    fn set_buttons(&mut self, mask: u8, relative: bool) -> Result<()> {
        self.buttons = mask;
        self.send(if relative { P::buttons_rel(mask as i64) } else { P::buttons_abs(mask as i64) })
    }

    pub fn button_down(&mut self, name: &str, relative: bool) -> Result<()> {
        self.set_buttons(self.buttons | button_mask(name)?, relative)?;
        self.done()
    }

    pub fn button_up(&mut self, name: &str, relative: bool) -> Result<()> {
        self.set_buttons(self.buttons & !button_mask(name)?, relative)?;
        self.done()
    }

    pub fn click(&mut self, name: &str, count: i64, relative: bool, interval: f64) -> Result<()> {
        let mask = button_mask(name)?;
        let r = (|| -> Result<()> {
            for i in 0..count {
                self.set_buttons(self.buttons | mask, relative)?;
                self.set_buttons(self.buttons & !mask, relative)?;
                if interval > 0.0 && i < count - 1 {
                    self.t.flush()?;
                    self.sleep(interval);
                }
            }
            Ok(())
        })();
        let f = self.t.flush();
        r.and(f)
    }

    /// Rueda: positivo = arriba. Trocea en pasos de ±127.
    pub fn scroll(&mut self, amount: i64, relative: bool) -> Result<()> {
        let mut amount = amount;
        while amount != 0 {
            let s = amount.clamp(-127, 127);
            let line = if relative { P::mouse_rel(0, 0, s) } else { P::mouse_abs(self.abs_pos.0, self.abs_pos.1, s) };
            self.send(line)?;
            amount -= s;
        }
        self.done()
    }
}

/// Agrupa el movimiento del ratón entre "ticks" (p. ej. un fotograma de la GUI): el relativo se
/// acumula, el absoluto se queda con la última posición. Un `flush_into` por tick -> como mucho
/// una orden de movimiento por tick en vez de una por evento del SO.
#[derive(Debug, Default, Clone)]
pub struct MouseCoalescer {
    dx: i64,
    dy: i64,
    wheel: i64,
    abs: Option<(i64, i64)>,
}

impl MouseCoalescer {
    pub fn add_rel(&mut self, dx: i64, dy: i64) {
        self.dx += dx;
        self.dy += dy;
    }
    pub fn add_wheel(&mut self, w: i64) {
        self.wheel += w;
    }
    pub fn set_abs(&mut self, x: i64, y: i64) {
        self.abs = Some((x, y));
    }
    pub fn is_empty(&self) -> bool {
        self.dx == 0 && self.dy == 0 && self.wheel == 0 && self.abs.is_none()
    }
    /// Envía lo acumulado (sin esperar respuestas si `kvm.auto_flush` es false) y se vacía.
    pub fn flush_into(&mut self, kvm: &mut Kvm) -> Result<()> {
        let s = std::mem::take(self);
        if let Some((x, y)) = s.abs {
            kvm.move_abs(x, y)?;
        }
        if s.dx != 0 || s.dy != 0 || s.wheel != 0 {
            kvm.move_rel(s.dx, s.dy, s.wheel)?;
        }
        Ok(())
    }
}
