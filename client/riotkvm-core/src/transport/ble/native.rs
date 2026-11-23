//! Conector BLE real con btleplug (WinRT en Windows, BlueZ en Linux, CoreBluetooth en macOS).
//!
//! Notas de plataforma (Windows/WinRT, ver client/README.md "BLE: trampas"):
//! - Se descubre por UUID de servicio con filtro por software (btleplug no pone el filtro de UUID en el SO
//!   porque algunos controladores pierden los anuncios de 128 bits). El UUID debe ir en el anuncio principal.
//! - No hace falta emparejar: `connect()` abre la sesión GATT sin pairing (Just Works del dongle).
//! - La MTU la negocia Windows sola (`GattSession.MaxPduSize`); btleplug la expone con `mtu()`. No se puede pedir.
//! - Windows cachea GATT por dirección: si el firmware cambia de servicios hay que forzar `Uncached`
//!   (btleplug ya descubre en modo `Uncached`) y, si falla, quitar el dispositivo en Ajustes.

use super::gatt::*;
use super::remember;
use crate::error::{Error, Result};
use crate::transport::LinkState;
use btleplug::api::{Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;
use uuid::Uuid;

pub fn svc() -> Uuid {
    Uuid::parse_str(SERVICE_UUID).unwrap()
}
fn chr(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn rt() -> Result<Arc<Runtime>> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("riotkvm-ble-rt")
        .build()
        .map(Arc::new)
        .map_err(|e| Error::Transport(format!("runtime BLE: {e}")))
}

fn ble_err(ctx: &str, e: btleplug::Error) -> Error {
    Error::Transport(format!("BLE {ctx}: {e}"))
}

async fn adapter() -> Result<Adapter> {
    let m = Manager::new().await.map_err(|e| ble_err("manager", e))?;
    let a = m.adapters().await.map_err(|e| ble_err("adaptadores", e))?;
    a.into_iter().next().ok_or_else(|| Error::DeviceNotFound("no hay adaptador Bluetooth (¿apagado o sin soporte LE?)".into()))
}

fn ours(services: &[Uuid]) -> bool {
    services.contains(&svc())
}

/// Escanea `window` y lista lo que anuncia (marca el dongle por su UUID de servicio). Para `riotkvm scan --ble`.
pub fn scan(window: Duration, all: bool) -> Result<Vec<Seen>> {
    let rt = rt()?;
    rt.block_on(async {
        let a = adapter().await?;
        a.start_scan(ScanFilter { services: if all { vec![] } else { vec![svc()] } }).await.map_err(|e| ble_err("scan", e))?;
        let mut events = a.events().await.map_err(|e| ble_err("eventos", e))?;
        let deadline = tokio::time::Instant::now() + window;
        let mut ids = Vec::new();
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.next()).await {
            if let CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) | CentralEvent::ServicesAdvertisement { id, .. } = ev {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        let _ = a.stop_scan().await;
        let mut out = Vec::new();
        for id in ids {
            if let Ok(p) = a.peripheral(&id).await {
                if let Ok(Some(pr)) = p.properties().await {
                    out.push(Seen {
                        id: id.to_string(),
                        name: pr.local_name.clone(),
                        rssi: pr.rssi,
                        services: pr.services.iter().map(|u| u.to_string()).collect(),
                        ours: ours(&pr.services),
                    });
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse((s.ours, s.rssi.unwrap_or(i16::MIN))));
        Ok(out)
    })
}

/// ¿Hay un dongle anunciando nuestro servicio? Sale en cuanto lo ve (o al vencer `window`).
pub fn probe(window: Duration) -> Result<bool> {
    let rt = rt()?;
    rt.block_on(async {
        let a = adapter().await?;
        a.start_scan(ScanFilter { services: vec![svc()] }).await.map_err(|e| ble_err("scan", e))?;
        let mut events = a.events().await.map_err(|e| ble_err("eventos", e))?;
        let deadline = tokio::time::Instant::now() + window;
        let mut found = false;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.next()).await {
            if let CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) | CentralEvent::ServicesAdvertisement { id, .. } = ev {
                if let Ok(p) = a.peripheral(&id).await {
                    if let Ok(Some(pr)) = p.properties().await {
                        if ours(&pr.services) {
                            found = true;
                            break;
                        }
                    }
                }
            }
        }
        let _ = a.stop_scan().await;
        Ok(found)
    })
}

pub struct NativeConnector {
    rt: Arc<Runtime>,
    adapter: Option<Adapter>,
    remembered: Option<String>,
    /// Ventana de escaneo por intento.
    pub scan_window: Duration,
}

impl NativeConnector {
    pub fn new() -> Result<NativeConnector> {
        Ok(NativeConnector { rt: rt()?, adapter: None, remembered: remember::load(), scan_window: Duration::from_millis(2500) })
    }
}

async fn find(a: &Adapter, remembered: &Option<String>, window: Duration) -> Result<Peripheral> {
    a.start_scan(ScanFilter { services: vec![svc()] }).await.map_err(|e| ble_err("scan", e))?;
    let mut events = a.events().await.map_err(|e| ble_err("eventos", e))?;
    let deadline = tokio::time::Instant::now() + window;
    let mut cands: Vec<(Peripheral, i16)> = Vec::new();
    let mut grace: Option<tokio::time::Instant> = None;
    loop {
        let until = grace.unwrap_or(deadline).min(deadline);
        let ev = match tokio::time::timeout_at(until, events.next()).await {
            Ok(Some(e)) => e,
            _ => break,
        };
        let id = match ev {
            CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) | CentralEvent::ServicesAdvertisement { id, .. } => id,
            _ => continue,
        };
        let Ok(p) = a.peripheral(&id).await else { continue };
        let Ok(Some(pr)) = p.properties().await else { continue };
        if !ours(&pr.services) {
            continue;
        }
        if !cands.iter().any(|(c, _)| c.id() == p.id()) {
            let is_mem = remembered.as_deref() == Some(p.id().to_string().as_str());
            cands.push((p, pr.rssi.unwrap_or(-100)));
            if is_mem {
                break; // el recordado manda: sin esperas
            }
            // primer candidato: 400 ms de gracia por si aparece el recordado u otro más fuerte
            grace.get_or_insert(tokio::time::Instant::now() + Duration::from_millis(400));
        }
    }
    let _ = a.stop_scan().await;
    let pick = cands
        .into_iter()
        .max_by_key(|(p, rssi)| (remembered.as_deref() == Some(p.id().to_string().as_str()), *rssi))
        .map(|(p, _)| p);
    pick.ok_or_else(|| Error::DeviceNotFound(format!("no se ve ningún dongle anunciando {SERVICE_UUID}")))
}

impl GattConnector for NativeConnector {
    fn connect(&mut self, inbox: Sender<Inbox>, status: &mut dyn FnMut(LinkState)) -> Result<Box<dyn GattLink>> {
        status(LinkState::Searching);
        let rt = self.rt.clone();
        if self.adapter.is_none() {
            self.adapter = Some(rt.block_on(adapter())?);
        }
        let a = self.adapter.clone().unwrap();
        let p = rt.block_on(find(&a, &self.remembered, self.scan_window))?;
        status(LinkState::Connecting);
        let id = p.id().to_string();
        let r: Result<(Characteristic, Characteristic, Vec<tokio::task::JoinHandle<()>>)> = rt.block_on(async {
            p.connect_with_timeout(Duration::from_secs(6)).await.map_err(|e| ble_err("conectar", e))?;
            p.discover_services_with_timeout(Duration::from_secs(6)).await.map_err(|e| ble_err("descubrir servicios", e))?;
            let chars = p.characteristics();
            let find = |u: &str| chars.iter().find(|c| c.uuid == chr(u)).cloned().ok_or_else(|| Error::Transport(format!("el dispositivo no tiene la característica {u} (¿firmware o caché GATT de Windows antiguos?)")));
            let cmd = find(CMD_UUID)?;
            let evt = find(EVT_UUID)?;
            p.subscribe(&evt).await.map_err(|e| ble_err("suscribir EVT", e))?;
            let mut notif = p.notifications().await.map_err(|e| ble_err("notificaciones", e))?;
            let mut tasks = Vec::new();
            let tx = inbox.clone();
            tasks.push(tokio::spawn(async move {
                while let Some(n) = notif.next().await {
                    if n.uuid == chr(EVT_UUID) {
                        let _ = tx.send(Inbox::Evt(n.value));
                    }
                }
                let _ = tx.send(Inbox::Lost("flujo de notificaciones cerrado".into()));
            }));
            let mut events = a.events().await.map_err(|e| ble_err("eventos", e))?;
            let (tx2, pid) = (inbox.clone(), p.id());
            tasks.push(tokio::spawn(async move {
                while let Some(ev) = events.next().await {
                    if let CentralEvent::DeviceDisconnected(i) = ev {
                        if i == pid {
                            let _ = tx2.send(Inbox::Lost("dispositivo desconectado".into()));
                            break;
                        }
                    }
                }
            }));
            Ok((cmd, evt, tasks))
        });
        match r {
            Err(e) => {
                let _ = rt.block_on(p.disconnect());
                Err(e)
            }
            Ok((cmd, _evt, tasks)) => {
                remember::save(&id);
                let name = rt.block_on(p.properties()).ok().flatten().and_then(|x| x.local_name).unwrap_or_else(|| ADV_NAME.into());
                let mtu = (p.mtu() as usize).max(23);
                #[cfg(windows)]
                let keep = prefer_fast_interval(p.address().into_inner());
                #[cfg(not(windows))]
                let keep: Option<Box<dyn std::any::Any + Send>> = None;
                Ok(Box::new(NativeLink { rt, p, cmd, id, name, mtu, tasks, _keep: keep }))
            }
        }
    }
}

/// Tipo de escritura en CMD. macOS: CON respuesta. Medido el 22/11/2026: tras una ráfaga de movimiento, btleplug 0.13
/// encola la escritura SIN respuesta esperando `peripheralIsReadyToSendWriteWithoutResponse` y ese aviso no llega: el
/// enlace se queda mudo (clics y teclas encolados que nunca salen). La escritura con respuesta no usa esa cola.
/// El firmware acepta las dos (`BLE_GATT_CHR_F_WRITE_NO_RSP | BLE_GATT_CHR_F_WRITE`). `RIOTKVM_BLE_WRITE=norsp|rsp` fuerza una.
fn write_type() -> WriteType {
    static T: std::sync::OnceLock<WriteType> = std::sync::OnceLock::new();
    *T.get_or_init(|| match crate::config::env("BLE_WRITE").as_deref() {
        Some("rsp") => WriteType::WithResponse,
        Some("norsp") => WriteType::WithoutResponse,
        _ if cfg!(target_os = "macos") => WriteType::WithResponse,
        _ => WriteType::WithoutResponse,
    })
}

/// Plazo de una escritura GATT (sin respuesta lo normal es < 1 ms; con respuesta, un par de intervalos de conexión).
const WRITE_TIMEOUT: Duration = Duration::from_millis(1000);

pub struct NativeLink {
    rt: Arc<Runtime>,
    p: Peripheral,
    cmd: Characteristic,
    id: String,
    name: String,
    mtu: usize,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Windows: la petición de parámetros de conexión debe seguir viva mientras dure el enlace.
    _keep: Option<Box<dyn std::any::Any + Send>>,
}

/// Windows 11: pide intervalo de conexión corto (BluetoothLEPreferredConnectionParameters::ThroughputOptimized,
/// ~7,5 ms en vez de los ~15 ms de "Balanced"). Es una PETICIÓN: el sistema decide. Desactivada por defecto (ver abajo).
/// Devuelve lo que hay que mantener vivo (dispositivo + petición).
#[cfg(windows)]
fn prefer_fast_interval(addr_be: [u8; 6]) -> Option<Box<dyn std::any::Any + Send>> {
    use windows::Devices::Bluetooth::{BluetoothLEDevice, BluetoothLEPreferredConnectionParameters};
    // MEDIDO (2026-11-21): ThroughputOptimized pide 12-12 (x1,25) = 15 ms FIJOS y encima de lo que pide el firmware (7,5 ms),
    // así que empeora (RTT p50 30 ms). Sin petición Windows acepta los 7,5 ms del dongle (RTT p50 15 ms). Por eso por defecto
    // NO se pide nada; `RIOTKVM_BLE_PARAMS=throughput` la activa solo para pruebas.
    if crate::config::env("BLE_PARAMS").map(|v| v != "throughput" && v != "drop").unwrap_or(true) {
        return None;
    }
    let addr = addr_be.iter().fold(0u64, |a, b| (a << 8) | *b as u64);
    let r = (|| -> windows::core::Result<_> {
        let dev = BluetoothLEDevice::FromBluetoothAddressAsync(addr)?.join()?;
        let req = dev.RequestPreferredConnectionParameters(&BluetoothLEPreferredConnectionParameters::ThroughputOptimized()?)?;
        let st = req.Status()?;
        if crate::config::env_os("BLE_DEBUG").is_some() {
            let pr = BluetoothLEPreferredConnectionParameters::ThroughputOptimized()?;
            eprintln!("BLE: ThroughputOptimized pide intervalo {}-{} (x1,25 ms), latencia {}, timeout {}", pr.MinConnectionInterval()?, pr.MaxConnectionInterval()?, pr.ConnectionLatency()?, pr.LinkTimeout()?);
            for (n, p) in [("Balanced", BluetoothLEPreferredConnectionParameters::Balanced()?), ("PowerOptimized", BluetoothLEPreferredConnectionParameters::PowerOptimized()?)] {
                eprintln!("BLE: {n}: {}-{} lat {} timeout {}", p.MinConnectionInterval()?, p.MaxConnectionInterval()?, p.ConnectionLatency()?, p.LinkTimeout()?);
            }
            let d2 = dev.clone();
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                let mut last = 0u16;
                while t0.elapsed() < std::time::Duration::from_secs(12) {
                    if let Ok(cp) = d2.GetConnectionParameters() {
                        let i = cp.ConnectionInterval().unwrap_or(0);
                        if i != last {
                            last = i;
                            eprintln!("BLE: t={:.2}s intervalo real de Windows = {} ms (lat {})", t0.elapsed().as_secs_f64(), i as f64 * 1.25, cp.ConnectionLatency().unwrap_or(0));
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            });
        }
        Ok((dev, req, st))
    })();
    match r {
        Ok((dev, req, st)) => {
            if crate::config::env_os("BLE_DEBUG").is_some() {
                eprintln!("BLE: petición ThroughputOptimized -> estado {st:?}");
            }
            if crate::config::env("BLE_PARAMS").map(|v| v == "drop").unwrap_or(false) {
                drop(req);
                drop(dev); // prueba: soltar la petición (Windows debería volver al perfil por defecto)
                return None;
            }
            Some(Box::new((dev, req)))
        }
        Err(e) => {
            if crate::config::env_os("BLE_DEBUG").is_some() {
                eprintln!("BLE: no se pudo pedir intervalo corto: {e}");
            }
            None
        }
    }
}

impl GattLink for NativeLink {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn mtu(&self) -> usize {
        self.mtu
    }
    fn write_cmd(&mut self, data: &[u8]) -> Result<()> {
        // macOS (btleplug/CoreBluetooth): si el periférico no está listo, la escritura sin respuesta se encola hasta el aviso
        // `peripheralIsReadyToSendWriteWithoutResponse`, que no llega nunca si el dongle se ha ido. Sin plazo, el hilo BLE
        // se quedaba bloqueado para siempre (y al cerrar la GUI, `Session::drop` -> `join` colgaba el proceso).
        match self.rt.block_on(async { tokio::time::timeout(WRITE_TIMEOUT, self.p.write(&self.cmd, data, write_type())).await }) {
            Ok(r) => r.map_err(|e| ble_err("escribir CMD", e)),
            Err(_) => Err(Error::Transport(format!("escribir CMD: sin respuesta de Bluetooth en {} ms (¿dongle desconectado?)", WRITE_TIMEOUT.as_millis()))),
        }
    }
    fn read_info(&mut self) -> Result<Vec<u8>> {
        let chars = self.p.characteristics();
        let info = chars.iter().find(|c| c.uuid == chr(INFO_UUID)).cloned().ok_or_else(|| Error::Transport("sin característica INFO".into()))?;
        match self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(3), self.p.read(&info)).await }) {
            Ok(r) => r.map_err(|e| ble_err("leer INFO", e)),
            Err(_) => Err(Error::Transport("leer INFO: sin respuesta de Bluetooth en 3 s".into())),
        }
    }
    fn disconnect(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), self.p.disconnect()).await });
    }
}
