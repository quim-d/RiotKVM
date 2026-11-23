//! `riotkvm fw ...` (firmware del dongle) y `riotkvm sd ...` (microSD con Ventoy). La lógica vive en `riotkvm-flash`.

use super::{put, App};
use clap::Subcommand;
use riotkvm_core::{Error, FwVersion, Result};
use riotkvm_flash::flash::{self, FlashOptions, ImageKind, Outcome};
use riotkvm_flash::release::{self, Availability};
use riotkvm_flash::{disks, rawdev, ventoy, Cancel, Progress, Reporter};
use serde_json::json;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Subcommand, Debug)]
pub enum FwCmd {
    /// Versión instalada (por USB o por el enlace) frente a la última release publicada.
    Check,
    /// Flashea la última release (verificada por SHA-256) o un .bin propio (--file). Puerto: --port o autodetección.
    Flash {
        /// factory.bin propio para escribir en 0x0 (no se verifica su origen).
        #[arg(long, value_name = "BIN")]
        file: Option<PathBuf>,
        /// No abre ningún puerto: comprueba la imagen y describe lo que haría.
        #[arg(long)]
        dry_run: bool,
        /// Permite instalar una versión más antigua que la del dongle.
        #[arg(long)]
        allow_downgrade: bool,
        /// Segundos de espera si hay que enchufar el dongle con BOOT pulsado.
        #[arg(long, default_value_t = 120)]
        wait: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum SdCmd {
    /// Discos externos/extraíbles candidatos (el dongle en modo A/C sale como «RiotKVM SD Ventoy»).
    List,
    /// BORRA el disco y escribe Ventoy (MBR: arranca en BIOS legacy y UEFI) con la partición exFAT «Ventoy», el
    /// tema RiotKVM, ventoy.json (menú en árbol) y la carpeta /Tools vacía. Las ISO las copias tú.
    Ventoy {
        /// Disco (`disk6` en macOS, `sdb` en Linux; ver `riotkvm sd list`).
        #[arg(long, required_unless_present = "image", conflicts_with = "image")]
        disk: Option<String>,
        /// (pruebas) escribe en este fichero imagen en vez de en un disco. Debe existir con el tamaño del «disco».
        #[arg(long, value_name = "FICHERO")]
        image: Option<PathBuf>,
        /// No pedir que se teclee el identificador del disco para confirmar.
        #[arg(long)]
        yes: bool,
    },
}

/// Avances a stderr: una línea por etapa y porcentaje cada 10 %.
struct CliRep {
    err: super::SharedWriter,
    last: Mutex<(String, u64, Instant)>,
}

impl CliRep {
    fn new(app: &App) -> CliRep {
        CliRep { err: app.env.stderr.clone(), last: Mutex::new((String::new(), u64::MAX, Instant::now())) }
    }
}

impl Reporter for CliRep {
    fn progress(&self, p: Progress) {
        let mut l = self.last.lock().unwrap();
        let pct = if p.total > 0 { p.done * 100 / p.total } else { u64::MAX };
        if p.stage != l.0 {
            *l = (p.stage.clone(), pct, Instant::now());
            put(&self.err, &format!("· {}", p.stage));
        } else if p.total > 0 && pct / 10 != l.1 / 10 {
            l.1 = pct;
            put(&self.err, &format!("  {pct:3} %"));
        }
    }
    fn log(&self, line: &str) {
        put(&self.err, line);
    }
}

fn ferr(e: riotkvm_flash::Error) -> Error {
    Error::Transport(e.to_string())
}

/// Puerto serie explícito para flashear: `--port` salvo los valores de transporte (ble, mock, auto).
fn flash_port(app: &App) -> Option<String> {
    app.cli.port.clone().filter(|p| !matches!(p.to_ascii_lowercase().as_str(), "ble" | "auto" | "mock" | "ble-mock"))
}

/// Versión instalada: primero por USB (sin BLE); si no hay dongle por USB, por el transporte normal.
fn installed(app: &mut App) -> (Option<FwVersion>, String) {
    let i = flash::detect_installed();
    if i.port.is_some() {
        return (i.version, i.describe());
    }
    match app.with_kvm(|_, k| k.fw_version()) {
        Ok(Some(v)) => (Some(v), format!("{v} (por el enlace)")),
        Ok(None) => (None, "desconocida: firmware anterior al versionado".into()),
        Err(e) => (None, format!("desconocida: sin conexión ({e})")),
    }
}

pub fn fw(app: &mut App, cmd: &FwCmd) -> Result<i32> {
    match cmd {
        FwCmd::Check => {
            let (cur, how) = installed(app);
            let avail = release::check_latest().map_err(ferr)?;
            match avail {
                Availability::Offer(o) => {
                    let verdict = release::compare(cur, o.version);
                    let text = format!(
                        "instalada: {how}\ndisponible: {} ({}, {})\n{}",
                        o.version,
                        o.tag,
                        o.asset,
                        match verdict {
                            release::Verdict::Upgrade => "hay una versión nueva: riotkvm fw flash",
                            release::Verdict::Same => "al día",
                            release::Verdict::Downgrade => "la instalada es más nueva que la publicada",
                            release::Verdict::Unknown => "no se sabe la instalada: riotkvm fw flash instala la publicada",
                        }
                    );
                    app.emit(json!({"installed": cur.map(|v| v.to_string()), "available": o.version.to_string(), "tag": o.tag, "asset": o.asset, "sha256": o.sha256, "verdict": format!("{verdict:?}").to_lowercase()}), Some(text));
                }
                Availability::None { tag, reason } => {
                    app.emit(json!({"installed": cur.map(|v| v.to_string()), "available": null, "tag": tag, "reason": reason}), Some(format!("instalada: {how}\ndisponible: ninguna ({reason})")));
                }
            }
            Ok(0)
        }
        FwCmd::Flash { file, dry_run, allow_downgrade, wait } => {
            let rep = CliRep::new(app);
            let cancel = Cancel::new();
            let (image, kind) = match file {
                Some(f) => {
                    let d = std::fs::read(f).map_err(|e| Error::Usage(format!("{}: {e}", f.display())))?;
                    put(&app.env.stderr, &format!("aviso: {} es un binario propio, no se verifica (SHA-256 {})", f.display(), riotkvm_flash::sha256_hex(&d)));
                    (d, ImageKind::Custom)
                }
                None => match release::check_latest().map_err(ferr)? {
                    Availability::Offer(o) => {
                        if !dry_run {
                            let (cur, _) = installed(app);
                            release::may_flash(release::compare(cur, o.version), *allow_downgrade).map_err(|e| Error::Usage(e.to_string()))?;
                        }
                        (release::fetch_firmware(&o, &rep, &cancel).map_err(ferr)?, ImageKind::Official(o.version))
                    }
                    Availability::None { reason, .. } => return Err(Error::DeviceNotFound(format!("no hay firmware publicado: {reason}. Usa --file"))),
                },
            };
            let opts = FlashOptions { port: flash_port(app), dry_run: *dry_run, wait_rom: std::time::Duration::from_secs(*wait) };
            match flash::run(&image, kind, &opts, &rep, &cancel).map_err(ferr)? {
                Outcome::DryRun(plan) => {
                    app.emit(json!({"dry_run": true, "plan": plan}), Some(plan.join("\n")));
                }
                Outcome::Done { chip, flash_mb, version_after, warnings } => {
                    let v = version_after.map(|v| v.to_string());
                    app.emit(
                        json!({"chip": chip, "flash_mb": flash_mb, "version": v, "warnings": warnings}),
                        Some(format!("flasheado ({chip}, {} MB); el dongle dice fw={}", flash_mb.unwrap_or(0), v.as_deref().unwrap_or("? (no respondió a info)"))),
                    );
                }
            }
            Ok(0)
        }
    }
}

fn find_disk(id: &str) -> Result<disks::Disk> {
    let all = disks::list().map_err(ferr)?;
    all.into_iter().find(|d| d.id == id || d.raw_path == id || format!("/dev/{}", d.id) == id).ok_or_else(|| Error::Usage(format!("{id}: no es un disco externo/extraíble (riotkvm sd list)")))
}

pub fn sd(app: &mut App, cmd: &SdCmd) -> Result<i32> {
    match cmd {
        SdCmd::List => {
            let all = disks::list().map_err(ferr)?;
            let def = disks::default_choice(&all);
            if app.cli.json {
                let v: Vec<_> = all
                    .iter()
                    .enumerate()
                    .map(|(i, d)| json!({"id": d.id, "raw": d.raw_path, "size": d.size, "model": d.model, "bus": d.bus, "riotkvm": d.is_riotkvm(), "card_reader": d.is_card_reader(), "default": def == Some(i), "usable": disks::safety(d).is_ok(), "why_not": disks::safety(d).err().map(|e| e.to_string()), "contents": d.contents()}))
                    .collect();
                app.emit(json!({"disks": v}), None);
            } else if all.is_empty() {
                put(&app.env.stderr, "no hay discos externos ni lectores de tarjetas");
            } else {
                for (i, d) in all.iter().enumerate() {
                    let mark = if def == Some(i) { '*' } else { ' ' };
                    let why = disks::safety(d).err().map(|e| format!("  [NO: {e}]")).unwrap_or_default();
                    put(&app.env.stdout, &format!("{mark} {}{why}", d.summary()));
                    for c in d.contents() {
                        put(&app.env.stdout, &format!("      {c}"));
                    }
                }
            }
            Ok(0)
        }
        SdCmd::Ventoy { disk, image, yes } => {
            let files = ventoy::theme::sd_files();
            let rep = CliRep::new(app);
            let cancel = Cancel::new();
            let mut dev: rawdev::FileDev;
            let target_desc;
            if let Some(img) = image {
                dev = rawdev::FileDev::open_image(img).map_err(ferr)?;
                target_desc = img.display().to_string();
                ventoy::plan(dev_size(&dev)).map_err(ferr)?;
            } else {
                let id = disk.as_deref().unwrap_or_default();
                let d = find_disk(id)?;
                disks::safety(&d).map_err(|e| Error::Usage(e.to_string()))?;
                ventoy::plan(d.size).map_err(ferr)?;
                put(&app.env.stderr, &format!("disco: {}", d.summary()));
                for c in d.contents() {
                    put(&app.env.stderr, &format!("  {c}"));
                }
                put(&app.env.stderr, "SE BORRARÁ TODO su contenido.");
                if !d.is_card_reader() && !d.is_riotkvm() {
                    put(&app.env.stderr, "aviso: no parece ni el dongle ni un lector de tarjetas");
                }
                if d.is_riotkvm() {
                    put(&app.env.stderr, "aviso: a través del dongle se escribe a ~1 MB/s (unos 70 s entre escribir y verificar); copiar ISOs luego es mucho más rápido con un lector de tarjetas");
                }
                if !yes {
                    put(&app.env.stderr, &format!("escribe «{}» para confirmar:", d.id));
                    let mut line = String::new();
                    std::io::BufReader::new(&mut app.env.stdin).read_line(&mut line).map_err(|e| Error::Usage(format!("stdin: {e}")))?;
                    if !disks::confirmed(&d, &line) {
                        return Err(Error::Usage("confirmación incorrecta: no se ha tocado nada".into()));
                    }
                }
                let d = disks::recheck(&d).map_err(ferr)?;
                // paquete antes de desmontar/pedir permisos: si la descarga falla, el disco queda intacto
                let pkg = ventoy::package::load(&rep, &cancel).map_err(ferr)?;
                disks::unmount(&d).map_err(ferr)?;
                dev = rawdev::open_device(&d.raw_path, d.size).map_err(ferr)?;
                target_desc = d.id.clone();
                return write_ventoy(app, &mut dev, &pkg, &files, &target_desc, &rep, &cancel);
            }
            let pkg = ventoy::package::load(&rep, &cancel).map_err(ferr)?;
            write_ventoy(app, &mut dev, &pkg, &files, &target_desc, &rep, &cancel)
        }
    }
}

fn dev_size(d: &rawdev::FileDev) -> u64 {
    use riotkvm_flash::rawdev::BlockDev;
    d.size()
}

fn write_ventoy(app: &mut App, dev: &mut rawdev::FileDev, pkg: &ventoy::Package, files: &[(String, Vec<u8>)], target: &str, rep: &CliRep, cancel: &Cancel) -> Result<i32> {
    let rnd = ventoy::Randomness::new().map_err(ferr)?;
    let p = ventoy::install(dev, pkg, &rnd, files, rep, cancel).map_err(ferr)?;
    ventoy::verify(dev, pkg, &rnd, files, rep).map_err(ferr)?;
    app.emit(
        json!({"target": target, "ventoy": ventoy::VERSION, "part1": [p.part1_start, p.part1_end], "part2": [p.part2_start, p.part2_end], "cluster_bytes": p.cluster_sectors * 512, "verified": true}),
        Some(format!("Ventoy {} escrito y verificado en {target} (exFAT «Ventoy» sectores {}-{}, VTOYEFI {}-{}). Copia tus ISOs a la SD: instaladores en la raíz, herramientas en /Tools.", ventoy::VERSION, p.part1_start, p.part1_end, p.part2_start, p.part2_end)),
    );
    Ok(0)
}
