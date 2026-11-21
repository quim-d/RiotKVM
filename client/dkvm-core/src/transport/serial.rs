//! Transporte por el CDC del dongle.
//!
//! - Autodescubrimiento por VID 0x1209 / PID 0x0001 (modo normal del dongle).
//! - Sobre `IoTransport`: hilo de E/S dedicado, escritura por lotes, hasta 16 órdenes en vuelo.
//! - DTR se deja ACTIVO al abrir: el CDC nativo de ESP32 solo escribe con DTR activo.

use super::io::{IoConfig, IoTransport, Link};
use crate::error::{Error, Result};
use serde::Serialize;
use serialport::SerialPort;
use std::io;
use std::time::{Duration, Instant};

pub const VID: u16 = 0x1209;
pub const PID: u16 = 0x0001;

#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    pub port: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub description: String,
    pub serial_number: Option<String>,
    pub is_dongle: bool,
}

pub fn list_ports() -> Vec<PortInfo> {
    let ports = serialport::available_ports().unwrap_or_default();
    ports
        .into_iter()
        .map(|p| {
            use serialport::SerialPortType::*;
            let (vid, pid, description, serial_number) = match p.port_type {
                UsbPort(u) => (
                    Some(u.vid),
                    Some(u.pid),
                    match (u.product, u.manufacturer) {
                        (Some(p), Some(m)) => format!("{p} ({m})"),
                        (Some(p), None) => p,
                        (None, Some(m)) => m,
                        _ => "USB".into(),
                    },
                    u.serial_number,
                ),
                BluetoothPort => (None, None, "Bluetooth".into(), None),
                PciPort => (None, None, "PCI".into(), None),
                Unknown => (None, None, "n/a".into(), None),
            };
            PortInfo {
                is_dongle: vid == Some(VID) && pid == Some(PID),
                port: p.port_name,
                vid,
                pid,
                description,
                serial_number,
            }
        })
        .collect()
}

pub fn find_dongle_in(ports: &[PortInfo]) -> Result<String> {
    let found: Vec<&str> = ports.iter().filter(|p| p.is_dongle).map(|p| p.port.as_str()).collect();
    match found.len() {
        0 => Err(Error::DeviceNotFound(
            "no hay ningún dongle (VID 1209:0001) conectado; usa --port o 'dkvm scan'".into(),
        )),
        1 => Ok(found[0].to_string()),
        _ => Err(Error::DeviceNotFound(format!("varios dongles ({}); elige con --port", found.join(", ")))),
    }
}

pub struct SerialLink {
    port: Box<dyn SerialPort>,
    write_deadline: Duration,
}

impl Link for SerialLink {
    fn write_all(&mut self, mut data: &[u8]) -> io::Result<()> {
        // El timeout del puerto es corto (para sondear lecturas): reintentar escrituras hasta el plazo.
        let start = Instant::now();
        while !data.is_empty() {
            match self.port.write(data) {
                Ok(0) => {}
                Ok(n) => data = &data[n..],
                Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(e) => return Err(e),
            }
            if !data.is_empty() && start.elapsed() > self.write_deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "timeout de escritura"));
            }
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }
}

pub struct SerialTransport;

impl SerialTransport {
    /// Abre `port` (o autodetecta el dongle) y arranca el hilo de E/S.
    pub fn open(port: Option<&str>) -> Result<IoTransport> {
        let name = match port {
            Some(p) => p.to_string(),
            None => find_dongle_in(&list_ports())?,
        };
        let p = serialport::new(&name, 115200)
            .timeout(Duration::from_millis(2))
            .dtr_on_open(true)
            .open()
            .map_err(|e| Error::DeviceNotFound(format!("no se pudo abrir {name}: {e}")))?;
        let _ = p.clear(serialport::ClearBuffer::Input);
        let link = SerialLink { port: p, write_deadline: Duration::from_secs(1) };
        Ok(IoTransport::new(Box::new(link), IoConfig { name, ..IoConfig::default() }))
    }
}
