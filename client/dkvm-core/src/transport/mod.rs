//! Interfaz de transporte. Serie CDC hoy, BLE mañana, mock para pruebas.
//!
//! Contrato (síncrono desde el punto de vista del llamante, una línea de texto por orden):
//! - `send(line, wait=true)`  -> respuesta (`ok`, `host=1 ...`); `err ...` -> `Error::Device`.
//! - `send(line, wait=false)` -> encola y vuelve ya (no espera el `ok`). El primer `err` de los
//!   envíos sin espera se devuelve en el siguiente `flush()`.
//! - `flush()` espera las respuestas pendientes.
//!
//! Un transporte BLE implementará lo mismo; si el protocolo BLE es binario, la traducción
//! línea -> trama vive dentro de ese transporte, sin tocar `input`/CLI.

pub mod ble;
pub mod events;
pub mod io;
pub mod mock;
pub mod select;
pub mod serial;
pub mod trace;

use crate::error::Result;
use std::sync::Arc;
use std::time::Duration;

pub use io::{IoConfig, IoTransport, Link};
pub use mock::{MockHandle, MockTransport};
pub use serial::{find_dongle_in, list_ports, PortInfo, SerialTransport, PID, VID};
pub use events::{LinkState, TEvent};
pub use trace::Trace;

pub trait Transport: Send {
    fn name(&self) -> &str;
    fn send(&mut self, line: &str, wait: bool) -> Result<Option<String>>;
    fn flush(&mut self) -> Result<()>;
    /// Error diferido (p. ej. un ERR del dongle) sin esperar a que salga nada: para el hilo de entrada interactivo.
    fn poll_error(&mut self) -> Result<()> {
        Ok(())
    }
    fn close(&mut self) -> Result<()>;
    /// Instrumentación de latencia (timestamps por orden), si el transporte la ofrece.
    fn trace(&self) -> Option<Arc<Trace>> {
        None
    }
    /// Eventos asíncronos del transporte (estado del enlace, estado del dongle empujado por EVT...).
    fn poll_events(&mut self) -> Vec<TEvent> {
        Vec::new()
    }
    /// `true` si el transporte se reconecta solo (BLE): un fallo de enlace NO exige abrir otro transporte.
    fn self_healing(&self) -> bool {
        false
    }
    /// Estado actual del enlace (`None` si el transporte no distingue estados: serie/mock).
    fn link_state(&self) -> Option<LinkState> {
        None
    }
    /// RTT de un ping con secuencia (BLE) o, por defecto, de un `status`.
    fn ping(&mut self) -> Result<Duration> {
        let t = std::time::Instant::now();
        self.send(&crate::protocol::status(), true)?;
        Ok(t.elapsed())
    }
    /// Lanza un ping SIN esperar el pong (sesión interactiva: no frena la entrada un RTT cada segundo). El RTT llega
    /// como `TEvent::Rtt` en `poll_events`. Por defecto (serie/mock) equivale a `ping`.
    fn ping_nowait(&mut self) -> Result<()> {
        self.ping().map(|_| ())
    }
}
