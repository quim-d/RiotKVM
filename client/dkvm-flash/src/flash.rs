//! Flasheo del firmware con `espflash` (nativo, sin Python). Replica `tools/flash.py`:
//!
//! 1. Dongle QKVM por USB (VID 0x1209, cualquier modo: el CDC existe en A, B y C) -> `bootloader\n` a 115200.
//! 2. Aparece la ROM de descarga (VID 0x303A) -> `espflash` detecta el chip y la flash.
//! 3. Comprobaciones: el binario oficial exige ESP32-S3 con flash ≥ 16 MB (pines del T-Dongle-S3); un `.bin`
//!    propio exige que la cabecera de imagen diga el mismo chip que el detectado.
//! 4. Último punto de cancelación -> escribir el factory.bin en 0x0 (con verificación MD5 de espflash) ->
//!    reset por watchdog -> esperar a que vuelva el VID 0x1209 y leer `info` para confirmar `fw=X.Y.Z`.
//!
//! Si el dongle no responde (firmware roto o ajeno): «enchúfalo con BOOT pulsado» y se espera a la ROM.

use crate::usb::{self, Pick, Port, Role};
use crate::{stage, Cancel, Error, Progress, Reporter, Result};
use dkvm_core::FwVersion;
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::target::{Chip, ProgressCallbacks};
use std::io::Write;
use std::time::{Duration, Instant};

/// Flash mínima del binario oficial (`board_upload.flash_size = 16MB`, particiones `default_16MB.csv`).
pub const OFFICIAL_MIN_FLASH: u32 = 16 << 20;
/// `chip_id` de la cabecera de imagen ESP-IDF para el ESP32-S3.
pub const CHIP_ID_S3: u16 = 0x0009;

pub const BOOT_HELP: &str = "Desenchufa el dongle, mantén pulsado el botón BOOT y vuelve a enchufarlo (suéltalo al estar dentro): \
arranca en modo descarga (VID 303A) y el flasheo sigue solo.";

/// Qué se va a escribir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    /// Binario de la release (verificado por SHA-256) con su versión.
    Official(FwVersion),
    /// `.bin` elegido por el usuario: no se verifica su origen.
    Custom,
}

/// Lo que dice la cabecera de imagen ESP-IDF del primer segmento (el bootloader en 0x0 del factory.bin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub chip_id: u16,
    /// Tamaño de flash declarado (nibble alto del byte 3), si es un valor conocido.
    pub flash_bytes: Option<u32>,
}

pub fn parse_image_header(d: &[u8]) -> Result<ImageInfo> {
    if d.len() < 24 || d[0] != 0xE9 {
        return Err(Error::msg("no es una imagen de firmware ESP (falta la cabecera 0xE9 en el offset 0): ¿es un factory.bin para escribir en 0x0?"));
    }
    let flash_bytes = match d[3] >> 4 {
        n @ 0..=7 => Some((1u32 << 20) << n),
        _ => None,
    };
    Ok(ImageInfo { chip_id: u16::from_le_bytes([d[12], d[13]]), flash_bytes })
}

/// `chip_id` de la cabecera de imagen para cada chip (ESP-IDF `esp_chip_id_t`).
pub fn chip_image_id(c: Chip) -> Option<u16> {
    Some(match c {
        Chip::Esp32 => 0x0000,
        Chip::Esp32s2 => 0x0002,
        Chip::Esp32c3 => 0x0005,
        Chip::Esp32s3 => 0x0009,
        Chip::Esp32c2 => 0x000C,
        Chip::Esp32c6 => 0x000D,
        Chip::Esp32h2 => 0x0010,
        Chip::Esp32p4 => 0x0012,
        Chip::Esp32c61 => 0x0014,
        Chip::Esp32c5 => 0x0017,
        _ => return None,
    })
}

/// Reglas de seguridad antes de escribir. Devuelve avisos (no bloqueantes) o el motivo del rechazo.
pub fn check_target(kind: ImageKind, img: &ImageInfo, image_len: usize, chip: Chip, flash: Option<u32>) -> Result<Vec<String>> {
    let mut warn = Vec::new();
    let mb = |b: u32| b >> 20;
    match kind {
        ImageKind::Official(_) => {
            if chip != Chip::Esp32s3 {
                return Err(Error::msg(format!("el firmware oficial es para ESP32-S3 (T-Dongle-S3) y este chip es {chip}: no se flashea")));
            }
            match flash {
                Some(f) if f >= OFFICIAL_MIN_FLASH => {}
                Some(f) => return Err(Error::msg(format!("el firmware oficial necesita flash de 16 MB y este ESP32-S3 tiene {} MB", mb(f)))),
                None => return Err(Error::msg("no se pudo leer el tamaño de la flash: el firmware oficial necesita 16 MB")),
            }
            if img.chip_id != CHIP_ID_S3 {
                return Err(Error::msg("el binario descargado no es de ESP32-S3 (cabecera): no se flashea"));
            }
            warn.push("el firmware oficial usa los pines del LilyGO T-Dongle-S3 (SD por SDMMC 12/16/14/17/21/18, LCD ST7735): en otra placa ESP32-S3 puede no funcionar".into());
        }
        ImageKind::Custom => {
            match chip_image_id(chip) {
                Some(id) if id != img.chip_id => {
                    return Err(Error::msg(format!("el .bin es para otro chip (chip_id {:#06x} en la cabecera) y el conectado es {chip}", img.chip_id)));
                }
                Some(_) => {}
                None => warn.push(format!("no sé comprobar si el .bin es para {chip}: compruébalo tú")),
            }
            warn.push("binario propio: no se verifica su origen ni su integridad".into());
        }
    }
    if let (Some(need), Some(have)) = (img.flash_bytes, flash) {
        if need > have {
            return Err(Error::msg(format!("el binario declara {} MB de flash y el chip tiene {} MB", mb(need), mb(have))));
        }
    }
    if let Some(have) = flash {
        if image_len as u64 > have as u64 {
            return Err(Error::msg(format!("el binario ({image_len} bytes) no cabe en la flash ({} MB)", mb(have))));
        }
    }
    Ok(warn)
}

#[derive(Debug, Clone)]
pub struct FlashOptions {
    /// Puerto concreto (`--port`); si no, autodetección (dongle QKVM o ROM Espressif).
    pub port: Option<String>,
    /// No abre ningún puerto: solo comprueba la imagen y describe lo que haría.
    pub dry_run: bool,
    /// Cuánto esperar a que aparezca la ROM si hay que enchufar con BOOT pulsado.
    pub wait_rom: Duration,
}

impl Default for FlashOptions {
    fn default() -> Self {
        FlashOptions { port: None, dry_run: false, wait_rom: Duration::from_secs(120) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    DryRun(Vec<String>),
    Done { chip: String, flash_mb: Option<u32>, version_after: Option<FwVersion>, warnings: Vec<String> },
}

/// Respuesta a `info` por el CDC del dongle (sirve en modos A, B y C). `None` si no responde.
pub fn read_info(port: &str) -> Option<String> {
    let t = dkvm_core::SerialTransport::open(Some(port)).ok()?;
    let mut kvm = dkvm_core::Kvm::new(Box::new(t), "us").ok()?;
    let r = kvm.info().ok();
    let _ = kvm.t.close();
    r
}

/// Versión instalada por USB. `None` si no responde o es un firmware anterior al versionado (`fw=etapa2`).
pub fn read_version(port: &str) -> Option<FwVersion> {
    read_info(port).and_then(|r| FwVersion::from_info_reply(&r))
}

/// Lo que se sabe del dongle conectado por USB.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    pub port: Option<Port>,
    pub info: Option<String>,
    pub version: Option<FwVersion>,
}

impl Installed {
    pub fn describe(&self) -> String {
        match (&self.port, &self.info, self.version) {
            (Some(p), _, Some(v)) => format!("{v} (USB {})", p.name),
            (Some(p), Some(i), None) => {
                let fw = i.split_whitespace().find(|t| t.starts_with("fw=")).unwrap_or("fw=?");
                format!("desconocida: firmware anterior al versionado ({fw}, USB {})", p.name)
            }
            (Some(p), None, None) if p.role == Role::Espressif => format!("desconocida: {} está en modo descarga", p.name),
            (Some(p), None, None) => format!("desconocida: {} no responde a info", p.name),
            (None, _, _) => "desconocida: no hay dongle por USB".into(),
        }
    }
}

/// Dongle conectado por USB (si hay exactamente uno) y su versión.
pub fn detect_installed() -> Installed {
    match usb::pick(&usb::scan()) {
        Pick::Qkvm(p) => {
            let info = read_info(&p.name);
            let version = info.as_deref().and_then(FwVersion::from_info_reply);
            Installed { port: Some(p), info, version }
        }
        Pick::Rom(p) => Installed { port: Some(p), ..Default::default() },
        _ => Installed::default(),
    }
}

fn send_bootloader(port: &str) -> Result<()> {
    let mut p = serialport::new(port, 115200)
        .timeout(Duration::from_millis(500))
        .dtr_on_open(true)
        .open()
        .map_err(|e| Error::msg(format!("no se pudo abrir {port}: {e} (¿lo tiene abierto la GUI por serie u otro programa?)")))?;
    p.write_all(b"bootloader\n").map_err(|e| Error::msg(format!("{port}: {e}")))?;
    let _ = p.flush();
    std::thread::sleep(Duration::from_millis(400));
    Ok(())
}

fn wait_for<F: Fn(&[Port]) -> Option<Port>>(what: &str, timeout: Duration, rep: &dyn Reporter, cancel: &Cancel, f: F) -> Result<Option<Port>> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        cancel.check()?;
        if let Some(p) = f(&usb::scan()) {
            return Ok(Some(p));
        }
        rep.progress(Progress { stage: what.to_string(), done: t0.elapsed().as_secs(), total: timeout.as_secs() });
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(None)
}

fn only_espressif(ports: &[Port]) -> Option<Port> {
    let e: Vec<&Port> = ports.iter().filter(|p| p.role == Role::Espressif).collect();
    (e.len() == 1).then(|| e[0].clone())
}

struct Cb<'a> {
    rep: &'a dyn Reporter,
    total: usize,
}

impl ProgressCallbacks for Cb<'_> {
    fn init(&mut self, _addr: u32, total: usize) {
        self.total = total;
        self.rep.progress(Progress { stage: "escribiendo la flash".into(), done: 0, total: total as u64 });
    }
    fn update(&mut self, current: usize) {
        self.rep.progress(Progress { stage: "escribiendo la flash".into(), done: current as u64, total: self.total as u64 });
    }
    fn verifying(&mut self) {
        stage(self.rep, "verificando (MD5 de la flash)");
    }
    fn finish(&mut self, _skipped: bool) {}
}

fn usb_info(name: &str) -> Result<serialport::UsbPortInfo> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .find_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) if p.port_name == name => Some(u),
            _ => None,
        })
        .ok_or_else(|| Error::msg(format!("{name} no es un puerto USB (o ha desaparecido)")))
}

/// Flujo completo. `image` ya viene verificado si es oficial ([`crate::release::fetch_firmware`]).
pub fn run(image: &[u8], kind: ImageKind, opts: &FlashOptions, rep: &dyn Reporter, cancel: &Cancel) -> Result<Outcome> {
    let img = parse_image_header(image)?;
    if let ImageKind::Official(_) = kind {
        if img.chip_id != CHIP_ID_S3 {
            return Err(Error::msg("el binario oficial no es de ESP32-S3 (cabecera)"));
        }
    }
    let ports = usb::scan();
    if opts.dry_run {
        let mut plan = vec![format!("imagen: {} bytes, chip_id {:#06x}, flash declarada {}", image.len(), img.chip_id, img.flash_bytes.map(|b| format!("{} MB", b >> 20)).unwrap_or("?".into()))];
        for p in &ports {
            plan.push(format!("puerto: {}", p.label()));
        }
        plan.push(match (&opts.port, usb::pick(&ports)) {
            (Some(p), _) => format!("se usaría el puerto {p} tal cual (reset DTR/RTS de espflash)"),
            (None, Pick::Qkvm(p)) => format!("se mandaría `bootloader` a {} y se esperaría la ROM (VID 303A)", p.name),
            (None, Pick::Rom(p)) => format!("se flashearía directamente por {} (ROM)", p.name),
            (None, Pick::Nothing) => format!("no hay dongle: {BOOT_HELP}"),
            (None, Pick::Ambiguous(v)) => format!("varios candidatos ({}): habría que elegir con --port", v.join(", ")),
        });
        plan.push("después: escribir en 0x0 con verificación, reset por watchdog y leer `info`".into());
        return Ok(Outcome::DryRun(plan));
    }

    // 1-2. llegar a la ROM
    let port: Port = match &opts.port {
        Some(name) => ports.iter().find(|p| &p.name == name).cloned().unwrap_or(Port { name: name.clone(), vid: None, pid: None, product: String::new(), role: Role::Other }),
        None => match usb::pick(&ports) {
            Pick::Ambiguous(v) => return Err(Error::msg(format!("hay varios dispositivos candidatos ({}): elige uno", v.join(", ")))),
            Pick::Rom(p) => p,
            Pick::Qkvm(p) => {
                stage(rep, format!("pidiendo modo descarga a {}", p.name));
                cancel.check()?;
                send_bootloader(&p.name)?;
                match wait_for("esperando la ROM de descarga (VID 303A)", Duration::from_secs(10), rep, cancel, only_espressif)? {
                    Some(r) => r,
                    None => {
                        rep.log(&format!("el dongle no entró en modo descarga. {BOOT_HELP}"));
                        wait_for("esperando el dongle con BOOT pulsado (VID 303A)", opts.wait_rom, rep, cancel, only_espressif)?.ok_or_else(|| Error::msg(format!("no aparece el modo descarga (VID 303A). {BOOT_HELP}")))?
                    }
                }
            }
            Pick::Nothing => {
                rep.log(&format!("no hay ningún dongle RiotKVM ni ROM Espressif. {BOOT_HELP}"));
                wait_for("esperando el dongle con BOOT pulsado (VID 303A)", opts.wait_rom, rep, cancel, only_espressif)?.ok_or_else(|| Error::msg(format!("no aparece el modo descarga (VID 303A). {BOOT_HELP}")))?
            }
        },
    };
    if let Role::Qkvm(_) = port.role {
        // --port apuntando al dongle en marcha: pasarlo a la ROM primero
        send_bootloader(&port.name)?;
        return Err(Error::msg(format!("{} era el dongle en marcha: se le pidió el modo descarga; vuelve a lanzar sin --port (o con el puerto nuevo VID 303A)", port.name)));
    }
    std::thread::sleep(Duration::from_millis(500)); // como flash.py: dar tiempo a la enumeración de la ROM
    cancel.check()?;

    // 3. conectar y comprobar el chip
    stage(rep, format!("conectando con {}", port.name));
    let usb_native = port.role == Role::Espressif;
    let serial = serialport::new(&port.name, 115_200)
        .flow_control(serialport::FlowControl::None)
        .open_native()
        .map_err(|e| Error::msg(format!("no se pudo abrir {}: {e}", port.name)))?;
    let after = if usb_native { ResetAfterOperation::WatchdogReset } else { ResetAfterOperation::HardReset };
    let conn = Connection::new(serial, usb_info(&port.name)?, after, ResetBeforeOperation::DefaultReset, 115_200);
    let baud = if usb_native { None } else { Some(460_800) };
    let mut f = Flasher::connect(conn, true, true, false, None, baud).map_err(|e| Error::msg(format!("espflash no conecta con {}: {e}", port.name)))?;
    let di = f.device_info().map_err(|e| Error::msg(format!("no se pudo leer el chip: {e}")))?;
    let flash = Some(di.flash_size.size());
    rep.log(&format!("chip {} rev {:?}, flash {} MB, MAC {}", di.chip, di.revision, di.flash_size.size() >> 20, di.mac_address.clone().unwrap_or_default()));
    let warnings = check_target(kind, &img, image.len(), di.chip, flash)?;
    for w in &warnings {
        rep.log(&format!("aviso: {w}"));
    }

    // 4. último punto de cancelación: a partir de aquí no se interrumpe
    if cancel.is_cancelled() {
        let _ = f.connection().reset();
        return Err(Error::Cancelled);
    }
    stage(rep, "escribiendo la flash (no desenchufes)");
    let mut cb = Cb { rep, total: image.len() };
    f.write_bin_to_flash(0, image, &mut cb).map_err(|e| Error::msg(format!("falló la escritura: {e}. Vuelve a intentarlo (con BOOT pulsado si el dongle ya no arranca)")))?;
    stage(rep, "reiniciando");
    let chip = di.chip;
    if let Err(e) = f.connection().reset_after(true, chip) {
        rep.log(&format!("reset tras flashear: {e}"));
    }
    drop(f);

    // 5. confirmar
    let back = wait_for("esperando a que vuelva el dongle (VID 1209)", Duration::from_secs(30), rep, &Cancel::new(), |ps| ps.iter().find(|p| matches!(p.role, Role::Qkvm(_))).cloned())?;
    let version_after = match back {
        Some(p) => {
            std::thread::sleep(Duration::from_millis(800));
            (0..3).find_map(|_| read_version(&p.name).or_else(|| {
                std::thread::sleep(Duration::from_millis(700));
                None
            }))
        }
        None => {
            rep.log("el dongle no ha vuelto en 30 s: desenchúfalo y vuelve a enchufarlo");
            None
        }
    };
    if let (ImageKind::Official(want), Some(got)) = (kind, version_after) {
        if want != got {
            return Err(Error::msg(format!("flasheado, pero el dongle dice fw={got} y se esperaba {want}")));
        }
    }
    Ok(Outcome::Done { chip: chip.to_string(), flash_mb: flash.map(|b| b >> 20), version_after, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(chip_id: u16, size_nibble: u8) -> Vec<u8> {
        let mut d = vec![0u8; 64];
        d[0] = 0xE9;
        d[1] = 3;
        d[3] = (size_nibble << 4) | 0x0F;
        d[12..14].copy_from_slice(&chip_id.to_le_bytes());
        d
    }

    #[test]
    fn header() {
        let i = parse_image_header(&hdr(9, 4)).unwrap();
        assert_eq!(i, ImageInfo { chip_id: 9, flash_bytes: Some(16 << 20) });
        assert_eq!(parse_image_header(&hdr(5, 2)).unwrap().flash_bytes, Some(4 << 20));
        assert_eq!(parse_image_header(&hdr(5, 0xF)).unwrap().flash_bytes, None);
        let mut bad = hdr(9, 4);
        bad[0] = 0;
        assert!(parse_image_header(&bad).is_err());
        assert!(parse_image_header(&[0xE9; 10]).is_err());
    }

    #[test]
    fn official_rules() {
        let v = ImageKind::Official(FwVersion::new(1, 1, 0));
        let s3 = ImageInfo { chip_id: 9, flash_bytes: Some(16 << 20) };
        assert!(check_target(v, &s3, 1 << 20, Chip::Esp32s3, Some(16 << 20)).is_ok());
        assert!(check_target(v, &s3, 1 << 20, Chip::Esp32s3, Some(32 << 20)).is_ok());
        assert!(check_target(v, &s3, 1 << 20, Chip::Esp32s3, Some(8 << 20)).is_err(), "S3 con 8 MB");
        assert!(check_target(v, &s3, 1 << 20, Chip::Esp32s3, None).is_err());
        assert!(check_target(v, &s3, 1 << 20, Chip::Esp32c3, Some(16 << 20)).is_err(), "otro chip");
        assert!(check_target(v, &s3, 1 << 20, Chip::Esp32, Some(16 << 20)).is_err());
        let not_s3 = ImageInfo { chip_id: 5, flash_bytes: None };
        assert!(check_target(v, &not_s3, 1 << 20, Chip::Esp32s3, Some(16 << 20)).is_err());
    }

    #[test]
    fn custom_rules() {
        let c3 = ImageInfo { chip_id: 5, flash_bytes: Some(4 << 20) };
        let w = check_target(ImageKind::Custom, &c3, 100_000, Chip::Esp32c3, Some(4 << 20)).unwrap();
        assert!(w.iter().any(|w| w.contains("no se verifica")));
        assert!(check_target(ImageKind::Custom, &c3, 100_000, Chip::Esp32s3, Some(16 << 20)).is_err(), "cabecera de C3 en un S3");
        let s3_8 = ImageInfo { chip_id: 9, flash_bytes: Some(8 << 20) };
        assert!(check_target(ImageKind::Custom, &s3_8, 100_000, Chip::Esp32s3, Some(8 << 20)).is_ok(), "S3 de 8 MB con su propio bin");
        assert!(check_target(ImageKind::Custom, &s3_8, 100_000, Chip::Esp32s3, Some(4 << 20)).is_err(), "declara más flash de la que hay");
        let s3 = ImageInfo { chip_id: 9, flash_bytes: None };
        assert!(check_target(ImageKind::Custom, &s3, 5 << 20, Chip::Esp32s3, Some(4 << 20)).is_err(), "no cabe");
    }

    #[test]
    fn dry_run_touches_nothing() {
        let o = run(&hdr(9, 4), ImageKind::Official(FwVersion::new(1, 1, 0)), &FlashOptions { dry_run: true, ..Default::default() }, &crate::Silent, &Cancel::new()).unwrap();
        let Outcome::DryRun(plan) = o else { panic!() };
        assert!(plan[0].contains("chip_id 0x0009"));
        assert!(run(&hdr(5, 4), ImageKind::Official(FwVersion::new(1, 1, 0)), &FlashOptions { dry_run: true, ..Default::default() }, &crate::Silent, &Cancel::new()).is_err());
    }
}
