//! Capa GATT mínima (síncrona) sobre la que corre `BleTransport`.
//!
//! Dos implementaciones: `native` (btleplug: WinRT en Windows, BlueZ en Linux, CoreBluetooth en macOS)
//! y `mockble` (par simulado para pruebas sin hardware).

use crate::error::Result;
use crate::transport::LinkState;
use std::sync::mpsc::Sender;

/// Servicio y características del contrato BLE (fijados por el coordinador; ver docs/PROTOCOL.md "BLE v1").
pub const SERVICE_UUID: &str = "7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0001";
/// CMD: write SIN respuesta, cliente -> dongle.
pub const CMD_UUID: &str = "7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0002";
/// EVT: notify, dongle -> cliente.
pub const EVT_UUID: &str = "7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0003";
/// INFO: read (versión, modo, capacidades).
pub const INFO_UUID: &str = "7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0004";
/// Nombre anunciado (solo informativo: se descubre por UUID de servicio, no por nombre).
/// Nombre de anuncio actual (visible). El descubrimiento es por UUID de servicio, así que también se conecta a firmware
/// anterior que anuncia `QKVM` o `QuimKVM` (ver `ADV_NAMES_ACCEPTED`).
pub const ADV_NAME: &str = "RiotKVM";
pub const ADV_NAMES_ACCEPTED: [&str; 3] = ["RiotKVM", "QKVM", "QuimKVM"];

/// Mensajes hacia el hilo del enlace (lo que escriben quien envía y los receptores del enlace).
pub enum Inbox {
    /// Orden de alto nivel (línea de texto) a codificar y escribir.
    Out { line: String, reply: Option<Sender<Result<String>>>, t: std::time::Instant },
    /// Esperar a que todo lo anterior esté escrito.
    Flush(Sender<Result<()>>),
    /// Ping con secuencia.
    Ping(Sender<Result<std::time::Duration>>),
    /// Notificación EVT cruda del dongle.
    Evt(Vec<u8>),
    /// El enlace se perdió (desconexión, error de escritura).
    Lost(String),
    Close,
}

/// Un dongle encontrado al escanear.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Seen {
    pub id: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub services: Vec<String>,
    /// Anuncia nuestro servicio.
    pub ours: bool,
}

/// Enlace GATT establecido con el dongle.
pub trait GattLink: Send {
    fn id(&self) -> String;
    fn name(&self) -> String;
    /// MTU ATT negociada (la carga útil de una escritura es `mtu - 3`).
    fn mtu(&self) -> usize;
    /// Escritura SIN respuesta en CMD.
    fn write_cmd(&mut self, data: &[u8]) -> Result<()>;
    fn read_info(&mut self) -> Result<Vec<u8>>;
    fn disconnect(&mut self);
}

/// Busca y conecta con el dongle. Bloqueante; `BleTransport` lo llama desde su hilo.
pub trait GattConnector: Send {
    /// Un intento completo: escanear por UUID de servicio -> conectar -> descubrir -> MTU -> suscribirse a EVT.
    /// Las notificaciones y las pérdidas de enlace se publican en `inbox`. `status` informa de las fases.
    fn connect(&mut self, inbox: Sender<Inbox>, status: &mut dyn FnMut(LinkState)) -> Result<Box<dyn GattLink>>;
}
