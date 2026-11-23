//! Par BLE simulado: un "dongle" en memoria que recibe escrituras CMD y empuja notificaciones EVT.
//! Permite probar reconexión, agrupación por MTU y respuestas sin hardware ni conocer el formato de trama.

use super::gatt::*;
use crate::error::{Error, Result};
use crate::transport::LinkState;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

type Responder = Box<dyn FnMut(&[u8]) -> Vec<Vec<u8>> + Send>;

#[derive(Default)]
struct World {
    /// Escrituras CMD recibidas (una entrada por escritura GATT).
    writes: Vec<Vec<u8>>,
    connects: u32,
    /// Intentos de conexión que fallan antes de dejar conectar.
    fail_connects: u32,
    /// Fuerza fallo en la próxima escritura.
    fail_write: bool,
    mtu: usize,
    inbox: Option<Sender<Inbox>>,
    info: Vec<u8>,
    /// Si está, INFO se genera en cada lectura (el simulador refleja su estado, p. ej. la luz).
    info_fn: Option<Box<dyn Fn() -> Vec<u8> + Send>>,
    responder: Option<Responder>,
}

/// Asa para inspeccionar y manejar el dongle simulado.
#[derive(Clone)]
pub struct MockDongle(Arc<Mutex<World>>);

impl MockDongle {
    pub fn new() -> MockDongle {
        MockDongle(Arc::new(Mutex::new(World { mtu: 247, info: vec![1, 0, 1, 2, 0b10_0111, 0, 0, 0], ..World::default() })))
    }
    pub fn connector(&self) -> Box<dyn GattConnector> {
        Box::new(MockConnector(self.0.clone()))
    }
    pub fn set_info(&self, info: &[u8]) {
        self.0.lock().unwrap().info = info.to_vec();
    }
    pub fn set_info_fn(&self, f: impl Fn() -> Vec<u8> + Send + 'static) {
        self.0.lock().unwrap().info_fn = Some(Box::new(f));
    }
    pub fn set_mtu(&self, mtu: usize) {
        self.0.lock().unwrap().mtu = mtu;
    }
    pub fn fail_next_connects(&self, n: u32) {
        self.0.lock().unwrap().fail_connects = n;
    }
    pub fn fail_next_write(&self) {
        self.0.lock().unwrap().fail_write = true;
    }
    /// Respuesta automática: cada escritura CMD puede producir notificaciones EVT.
    pub fn set_responder(&self, f: impl FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static) {
        self.0.lock().unwrap().responder = Some(Box::new(f));
    }
    pub fn writes(&self) -> Vec<Vec<u8>> {
        self.0.lock().unwrap().writes.clone()
    }
    pub fn connects(&self) -> u32 {
        self.0.lock().unwrap().connects
    }
    /// Empuja una notificación EVT.
    pub fn notify(&self, bytes: &[u8]) {
        if let Some(tx) = &self.0.lock().unwrap().inbox {
            let _ = tx.send(Inbox::Evt(bytes.to_vec()));
        }
    }
    /// Corta el enlace (el dongle se va / se reinicia al flashear).
    pub fn drop_link(&self, why: &str) {
        let tx = self.0.lock().unwrap().inbox.clone();
        if let Some(tx) = tx {
            let _ = tx.send(Inbox::Lost(why.into()));
        }
    }
}

impl Default for MockDongle {
    fn default() -> Self {
        Self::new()
    }
}

struct MockConnector(Arc<Mutex<World>>);

impl GattConnector for MockConnector {
    fn connect(&mut self, inbox: Sender<Inbox>, status: &mut dyn FnMut(LinkState)) -> Result<Box<dyn GattLink>> {
        status(LinkState::Searching);
        {
            let mut w = self.0.lock().unwrap();
            if w.fail_connects > 0 {
                w.fail_connects -= 1;
                drop(w);
                std::thread::sleep(std::time::Duration::from_millis(5));
                return Err(Error::DeviceNotFound("el dongle no se ve (simulado)".into()));
            }
        }
        status(LinkState::Connecting);
        let mut w = self.0.lock().unwrap();
        w.connects += 1;
        w.inbox = Some(inbox);
        Ok(Box::new(MockLink(self.0.clone())))
    }
}

struct MockLink(Arc<Mutex<World>>);

impl GattLink for MockLink {
    fn id(&self) -> String {
        "mock:00".into()
    }
    fn name(&self) -> String {
        ADV_NAME.into()
    }
    fn mtu(&self) -> usize {
        self.0.lock().unwrap().mtu
    }
    fn write_cmd(&mut self, data: &[u8]) -> Result<()> {
        let mut w = self.0.lock().unwrap();
        if w.fail_write {
            w.fail_write = false;
            return Err(Error::Transport("escritura GATT fallida (simulada)".into()));
        }
        assert!(data.len() <= w.mtu.saturating_sub(3).max(20), "escritura mayor que la MTU");
        w.writes.push(data.to_vec());
        let replies = match w.responder.as_mut() {
            Some(f) => f(data),
            None => vec![],
        };
        let tx = w.inbox.clone();
        drop(w);
        if let Some(tx) = tx {
            for r in replies {
                let _ = tx.send(Inbox::Evt(r));
            }
        }
        Ok(())
    }
    fn read_info(&mut self) -> Result<Vec<u8>> {
        let w = self.0.lock().unwrap();
        Ok(match &w.info_fn {
            Some(f) => f(),
            None => w.info.clone(),
        })
    }
    fn disconnect(&mut self) {}
}
