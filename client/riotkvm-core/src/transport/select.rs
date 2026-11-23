//! Selección de transporte: BLE (si hay dongle anunciando) o serie, mock, o BLE simulado.

use super::ble::{mockble::MockDongle, BleOptions, BleTransport};
use super::{MockTransport, SerialTransport, Transport};
use crate::ble_v1::{sim::Sim, BleV1};
use crate::error::{Error, Result};
use std::time::Duration;

/// Qué transporte quiere el usuario (`--port`/`--ble`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// BLE si hay un dongle anunciando; si no, serie.
    Auto,
    Ble,
    /// Puerto concreto (o `None` = autodetectar el dongle por VID/PID).
    Serial(Option<String>),
    Mock,
    /// Dongle BLE v1 simulado en memoria (sin hardware).
    BleMock,
}

impl Choice {
    pub fn parse(port: Option<&str>, ble: bool) -> Choice {
        match (port, ble) {
            (Some("mock"), _) => Choice::Mock,
            (Some("ble-mock"), _) => Choice::BleMock,
            (Some("ble"), _) | (None, true) => Choice::Ble,
            (Some("auto"), _) | (None, false) => Choice::Auto,
            (Some("serial"), _) => Choice::Serial(None),
            (Some(p), _) => Choice::Serial(Some(p.into())),
        }
    }
}

/// BLE simulado completo: transporte + handle del dongle simulado (estado HID) y del enlace (cortes).
pub fn ble_sim(opts: BleOptions) -> (BleTransport, Sim, MockDongle) {
    let link = MockDongle::new();
    let sim = Sim::new();
    let s2 = sim.clone();
    let l2 = link.clone();
    link.set_responder(move |w| s2.handle(w));
    let s3 = sim.clone();
    link.set_info_fn(move || s3.info_bytes());
    // al suscribirse el dongle real envía STATE: lo imitamos al conectar
    let t = BleTransport::spawn(link.connector(), Box::new(BleV1::new()), opts);
    // se espera por CONDICIÓN (enlace conectado, hasta 20 s), no un sleep fijo: en un equipo cargado 30 ms no bastan
    let t0 = std::time::Instant::now();
    while !t.link_state().map(|s| s.is_connected()).unwrap_or(true) && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(2));
    }
    l2.notify(&sim.state_evt());
    (t, sim, l2)
}

#[cfg(feature = "ble")]
pub fn ble_connector() -> Result<Box<dyn super::ble::GattConnector>> {
    Ok(Box::new(super::ble::native::NativeConnector::new()?))
}

#[cfg(not(feature = "ble"))]
pub fn ble_connector() -> Result<Box<dyn super::ble::GattConnector>> {
    Err(Error::Usage("este binario se compiló sin soporte BLE (feature `ble`)".into()))
}

/// BLE real, sin esperar enlace (GUI): el estado se sigue con `link_state`/`poll_events`.
pub fn spawn_ble() -> Result<BleTransport> {
    Ok(BleTransport::spawn(ble_connector()?, Box::new(BleV1::new()), BleOptions::default()))
}

/// BLE real esperando el enlace hasta `timeout` (CLI).
pub fn open_ble(timeout: Duration) -> Result<BleTransport> {
    BleTransport::open(ble_connector()?, Box::new(BleV1::new()), BleOptions::default(), timeout)
}

/// ¿Hay un dongle anunciando el servicio? (escaneo corto, con salida anticipada al verlo)
#[cfg(feature = "ble")]
pub fn ble_advertising(window: Duration) -> bool {
    super::ble::native::probe(window).unwrap_or(false)
}
#[cfg(not(feature = "ble"))]
pub fn ble_advertising(_window: Duration) -> bool {
    false
}

/// Abre según la elección. `ble_timeout`: espera de enlace para BLE bloqueante.
pub fn open(choice: &Choice, ble_timeout: Duration) -> Result<Box<dyn Transport>> {
    match choice {
        Choice::Mock => Ok(Box::new(MockTransport::new())),
        Choice::BleMock => Ok(Box::new(ble_sim(BleOptions::default()).0)),
        Choice::Serial(p) => Ok(Box::new(SerialTransport::open(p.as_deref())?)),
        Choice::Ble => Ok(Box::new(open_ble(ble_timeout)?)),
        Choice::Auto => {
            if ble_advertising(Duration::from_millis(1200)) {
                if let Ok(t) = open_ble(ble_timeout) {
                    return Ok(Box::new(t));
                }
            }
            SerialTransport::open(None).map(|t| Box::new(t) as Box<dyn Transport>).map_err(|e| match e {
                Error::DeviceNotFound(_) => Error::DeviceNotFound("no hay dongle: ni anunciando por BLE ni por serie (VID 1209:0001); usa --port COMx o --ble".into()),
                o => o,
            })
        }
    }
}
