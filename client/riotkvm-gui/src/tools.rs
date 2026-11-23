//! Ventana «Herramientas»: flashear el firmware del dongle y preparar una microSD con Ventoy.
//!
//! Toda la lógica está en `riotkvm-flash`; aquí solo la UI. Cada trabajo largo corre en un hilo propio y manda
//! mensajes por canal (mismo patrón que la enumeración de cámaras de `app.rs`); la UI se repinta al recibirlos.
//! La cancelación es la de `riotkvm_flash::Cancel`: solo surte efecto ANTES de empezar a escribir.

use crate::theme;
use riotkvm_flash::disks::{self, Disk};
use riotkvm_flash::flash::{self, FlashOptions, ImageKind, Installed, Outcome};
use riotkvm_flash::release::{self, Availability, FirmwareOffer};
use riotkvm_flash::usb::{self, Port};
use riotkvm_flash::{ventoy, Cancel, Progress, Reporter};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Firmware,
    Sd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FwSource {
    #[default]
    Release,
    File,
}

enum Msg {
    FwInfo(Installed, Result<Availability, String>),
    Ports(Vec<Port>),
    Disks(Result<Vec<Disk>, String>),
    Progress(Progress),
    Log(String),
    /// Fin del trabajo: Ok(resumen) o Err(motivo).
    Done(Result<String, String>),
}

struct ChanRep {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Reporter for ChanRep {
    fn progress(&self, p: Progress) {
        let _ = self.tx.send(Msg::Progress(p));
        self.ctx.request_repaint();
    }
    fn log(&self, line: &str) {
        let _ = self.tx.send(Msg::Log(line.to_string()));
        self.ctx.request_repaint();
    }
}

struct Job {
    label: String,
    cancel: Cancel,
    progress: Option<Progress>,
    /// Ya está escribiendo: cancelar no surtiría efecto.
    writing: bool,
}

#[derive(Default)]
pub struct Tools {
    pub open: bool,
    env_checked: bool,
    tab: Tab,
    tx: Option<Sender<Msg>>,
    rx: Option<Receiver<Msg>>,
    // firmware
    checking: bool,
    installed: Option<Installed>,
    avail: Option<Result<Availability, String>>,
    source: FwSource,
    file: String,
    allow_downgrade: bool,
    dry_run: bool,
    ports: Vec<Port>,
    port: Option<String>,
    confirm_flash: bool,
    // sd
    disks: Option<Result<Vec<Disk>, String>>,
    disk_sel: Option<String>,
    confirm_text: String,
    /// Tema RiotKVM en la SD (por defecto sí; se pone a true al abrir la ventana la primera vez).
    riotkvm_theme: bool,
    // trabajo en curso y resultado
    job: Option<Job>,
    result: Option<Result<String, String>>,
    log: Vec<String>,
}

impl Tools {
    /// Un solo canal para todos los hilos (consultas y trabajos), creado la primera vez.
    fn chan(&mut self, ctx: &egui::Context) -> ChanRep {
        if self.tx.is_none() {
            let (tx, rx) = channel();
            self.tx = Some(tx);
            self.rx = Some(rx);
        }
        ChanRep { tx: self.tx.clone().unwrap(), ctx: ctx.clone() }
    }

    /// Versión instalada (por USB) y última publicada, en segundo plano.
    fn refresh_fw(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        self.checking = true;
        let rep = self.chan(ctx);
        std::thread::spawn(move || {
            let _ = rep.tx.send(Msg::Ports(usb::scan()));
            let inst = flash::detect_installed();
            let avail = release::check_latest().map_err(|e| e.to_string());
            let _ = rep.tx.send(Msg::FwInfo(inst, avail));
            rep.ctx.request_repaint();
        });
    }

    fn refresh_disks(&mut self, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let rep = self.chan(ctx);
        std::thread::spawn(move || {
            let _ = rep.tx.send(Msg::Disks(disks::list().map_err(|e| e.to_string())));
            rep.ctx.request_repaint();
        });
    }

    fn start(&mut self, ctx: &egui::Context, label: &str, work: impl FnOnce(&ChanRep, &Cancel) -> Result<String, String> + Send + 'static) {
        let rep = self.chan(ctx);
        let cancel = Cancel::new();
        self.job = Some(Job { label: label.into(), cancel: cancel.clone(), progress: None, writing: false });
        self.result = None;
        self.log.clear();
        std::thread::spawn(move || {
            let r = work(&rep, &cancel);
            let _ = rep.tx.send(Msg::Done(r));
            rep.ctx.request_repaint();
        });
    }

    fn drain(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        let mut done = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::FwInfo(i, a) => {
                    self.installed = Some(i);
                    self.avail = Some(a);
                    self.checking = false;
                }
                Msg::Ports(p) => self.ports = p,
                Msg::Disks(d) => {
                    if let Ok(v) = &d {
                        let keep = self.disk_sel.as_ref().is_some_and(|s| v.iter().any(|x| &x.id == s && disks::safety(x).is_ok()));
                        if !keep {
                            self.disk_sel = disks::default_choice(v).map(|i| v[i].id.clone());
                            self.confirm_text.clear();
                        }
                    }
                    self.disks = Some(d);
                }
                Msg::Progress(p) => {
                    if let Some(j) = &mut self.job {
                        j.writing |= p.stage.starts_with("escribiendo") || p.stage.starts_with("formateando");
                        j.progress = Some(p);
                    }
                }
                Msg::Log(l) => self.log.push(l),
                Msg::Done(r) => done = Some(r),
            }
        }
        if let Some(r) = done {
            let was_sd = self.job.as_ref().is_some_and(|j| j.label.contains("Ventoy"));
            self.job = None;
            self.result = Some(r);
            self.confirm_flash = false;
            self.confirm_text.clear();
            if was_sd {
                self.refresh_disks(ctx);
            }
        }
    }

    /// Dibuja la ventana si está abierta.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.env_checked {
            // (pruebas/capturas) RIOTKVM_GUI_TOOLS=fw|sd abre la ventana al arrancar
            self.env_checked = true;
            self.riotkvm_theme = true;
            match riotkvm_core::config::env("GUI_TOOLS").as_deref() {
                Some("fw") => self.open = true,
                Some("sd") => {
                    self.open = true;
                    self.tab = Tab::Sd;
                    self.refresh_disks(ctx);
                }
                _ => {}
            }
        }
        self.drain(ctx);
        if !self.open {
            return;
        }
        if self.installed.is_none() && !self.checking && self.job.is_none() {
            self.refresh_fw(ctx);
        }
        // arrastrar un .bin a la ventana lo elige como fichero propio
        if let Some(p) = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| !p.as_os_str().is_empty())) {
            self.file = p.display().to_string();
            self.source = FwSource::File;
            self.tab = Tab::Firmware;
        }
        let mut open = self.open;
        egui::Window::new("Tools").open(&mut open).default_width(560.0).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let busy = self.job.is_some();
                ui.add_enabled_ui(!busy, |ui| {
                    if ui.selectable_value(&mut self.tab, Tab::Firmware, "Firmware del dongle").clicked() {
                        self.result = None;
                    }
                    if ui.selectable_value(&mut self.tab, Tab::Sd, "microSD con Ventoy").clicked() {
                        self.result = None;
                        if self.disks.is_none() {
                            self.refresh_disks(ctx);
                        }
                    }
                });
            });
            ui.separator();
            // contenido con scroll que ocupa todo el alto: la ventana se puede estirar en vertical
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match self.tab {
                    Tab::Firmware => self.ui_firmware(ui, ctx),
                    Tab::Sd => self.ui_sd(ui, ctx),
                }
                self.ui_job(ui);
                ui.separator();
                crate::settings::repo_link(ui);
            });
        });
        // no se cierra con un trabajo en marcha
        self.open = open || self.job.is_some();
    }

    fn ui_job(&mut self, ui: &mut egui::Ui) {
        if let Some(j) = &self.job {
            ui.separator();
            ui.label(egui::RichText::new(&j.label).strong());
            match &j.progress {
                Some(p) if p.total > 0 => {
                    ui.add(egui::ProgressBar::new(p.done as f32 / p.total as f32).text(format!("{} · {}", p.stage, pct(p))));
                }
                Some(p) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(&p.stage);
                    });
                }
                None => {
                    ui.spinner();
                }
            }
            if j.writing {
                ui.colored_label(theme::AMBAR, "escribiendo: no desenchufes nada (ya no se puede cancelar)");
            } else if ui.button("Cancelar").clicked() {
                j.cancel.cancel();
            }
        }
        if let Some(r) = &self.result {
            ui.separator();
            match r {
                Ok(m) => ui.colored_label(theme::VERDE, m),
                Err(e) => ui.colored_label(theme::ROJO, e),
            };
        }
        if !self.log.is_empty() {
            egui::CollapsingHeader::new(format!("detalles ({})", self.log.len())).default_open(self.job.is_some()).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(140.0).stick_to_bottom(true).show(ui, |ui| {
                    for l in &self.log {
                        ui.label(egui::RichText::new(l).small());
                    }
                });
            });
        }
    }

    fn offer(&self) -> Option<&FirmwareOffer> {
        match &self.avail {
            Some(Ok(Availability::Offer(o))) => Some(o),
            _ => None,
        }
    }

    fn ui_firmware(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.job.is_some();
        egui::Grid::new("fw").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Instalada:");
            ui.label(match &self.installed {
                Some(i) => i.describe(),
                None if self.checking => "comprobando...".into(),
                None => "?".into(),
            });
            ui.end_row();
            ui.label("Publicada:");
            match &self.avail {
                None => ui.label(if self.checking { "consultando GitHub..." } else { "?" }),
                Some(Err(e)) if e.to_string().ends_with("(404)") => ui.colored_label(theme::GRIS, "aún no hay ninguna release publicada"),
                Some(Err(e)) => ui.colored_label(theme::ROJO, format!("no se pudo consultar: {e}")),
                Some(Ok(Availability::None { reason, .. })) => ui.colored_label(theme::AMBAR, reason),
                Some(Ok(Availability::Offer(o))) => ui.label(format!("{} ({}, {}, SHA-256 {:?})", o.version, o.tag, o.asset, o.sha_source)),
            };
            ui.end_row();
        });
        ui.add_enabled_ui(!busy && !self.checking, |ui| {
            if ui.small_button("Comprobar otra vez").clicked() {
                self.refresh_fw(ctx);
            }
        });
        ui.separator();

        ui.add_enabled_ui(!busy, |ui| {
            ui.label(egui::RichText::new("qué flashear").color(theme::GRIS));
            ui.radio_value(&mut self.source, FwSource::Release, "la última release (verificada por SHA-256)");
            ui.radio_value(&mut self.source, FwSource::File, "un factory.bin propio (se escribe en 0x0)");
            if self.source == FwSource::File {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.file).hint_text("ruta del .bin (o arrástralo a la ventana)").desired_width(400.0));
                });
                ui.colored_label(theme::AMBAR, "Binario propio: NO se verifica su origen. Se comprueba que la cabecera sea del mismo chip que el conectado; en un ESP32-S3 que no sea el T-Dongle-S3 solo tiene sentido con tu propio firmware.");
            }
            ui.horizontal(|ui| {
                ui.label("puerto:");
                let sel = self.port.clone().unwrap_or_else(|| "automático (dongle RiotKVM o ROM 303A)".into());
                egui::ComboBox::from_id_salt("fw-port").selected_text(sel).width(330.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.port, None, "automático (dongle RiotKVM o ROM 303A)");
                    for p in &self.ports {
                        ui.selectable_value(&mut self.port, Some(p.name.clone()), p.label());
                    }
                });
                if ui.small_button("↻").on_hover_text("volver a buscar puertos").clicked() {
                    self.ports = usb::scan();
                }
            });
            ui.checkbox(&mut self.allow_downgrade, "permitir bajar de versión");
            ui.checkbox(&mut self.dry_run, "simulación (no abre el puerto ni escribe nada)");
        });

        // comprobaciones previas en la UI (el hilo las repite)
        let mut blocker: Option<String> = None;
        if self.source == FwSource::Release {
            match self.offer() {
                None => blocker = Some("no hay firmware publicado para flashear: elige un .bin propio".into()),
                Some(o) => {
                    let cur = self.installed.as_ref().and_then(|i| i.version);
                    let v = release::compare(cur, o.version);
                    match v {
                        release::Verdict::Upgrade => {
                            ui.colored_label(theme::VERDE, format!("hay versión nueva: {} → {}", cur.map(|c| c.to_string()).unwrap_or_default(), o.version));
                        }
                        release::Verdict::Same => {
                            ui.label("ya tienes esa versión (se puede reinstalar)");
                        }
                        release::Verdict::Downgrade => {
                            ui.colored_label(theme::AMBAR, "la publicada es MÁS ANTIGUA que la instalada");
                        }
                        release::Verdict::Unknown => {
                            ui.label("versión instalada desconocida: se instalará la publicada");
                        }
                    }
                    if let Err(e) = release::may_flash(v, self.allow_downgrade) {
                        blocker = Some(e.to_string());
                    }
                }
            }
        } else if self.file.trim().is_empty() {
            blocker = Some("elige el fichero .bin".into());
        }
        if let Some(b) = &blocker {
            ui.colored_label(theme::GRIS, b);
        }
        ui.label(egui::RichText::new(flash::BOOT_HELP).small().color(theme::GRIS)).on_hover_text("si el dongle no responde a la orden bootloader (firmware roto o ajeno)");

        ui.add_enabled_ui(!busy && blocker.is_none(), |ui| {
            if !self.confirm_flash {
                if ui.button(if self.dry_run { "Simular flasheo" } else { "Flashear..." }).clicked() {
                    if self.dry_run {
                        self.launch_flash(ctx);
                    } else {
                        self.confirm_flash = true;
                    }
                }
            } else {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::AMBAR, "El dongle se reiniciará en modo descarga y se reescribirá su flash entera.");
                });
                ui.horizontal(|ui| {
                    if ui.button("Sí, flashear").clicked() {
                        self.launch_flash(ctx);
                    }
                    if ui.button("No").clicked() {
                        self.confirm_flash = false;
                    }
                });
            }
        });
    }

    fn launch_flash(&mut self, ctx: &egui::Context) {
        self.confirm_flash = false;
        let opts = FlashOptions { port: self.port.clone(), dry_run: self.dry_run, ..Default::default() };
        let src = self.source;
        let file = PathBuf::from(self.file.trim());
        let offer = self.offer().cloned();
        let cur = self.installed.as_ref().and_then(|i| i.version);
        let allow = self.allow_downgrade;
        self.start(ctx, if self.dry_run { "Simulación del flasheo" } else { "Flasheando el firmware" }, move |rep, cancel| {
            let e = |e: riotkvm_flash::Error| e.to_string();
            let (image, kind) = match src {
                FwSource::File => {
                    let d = std::fs::read(&file).map_err(|x| format!("{}: {x}", file.display()))?;
                    rep.log(&format!("{}: {} bytes, SHA-256 {}", file.display(), d.len(), riotkvm_flash::sha256_hex(&d)));
                    (d, ImageKind::Custom)
                }
                FwSource::Release => {
                    let o = offer.ok_or("no hay firmware publicado")?;
                    release::may_flash(release::compare(cur, o.version), allow).map_err(e)?;
                    (release::fetch_firmware(&o, rep, cancel).map_err(e)?, ImageKind::Official(o.version))
                }
            };
            match flash::run(&image, kind, &opts, rep, cancel).map_err(e)? {
                Outcome::DryRun(plan) => {
                    for l in &plan {
                        rep.log(l);
                    }
                    Ok("simulación correcta: no se ha tocado el dongle (detalles abajo)".into())
                }
                Outcome::Done { chip, flash_mb, version_after, .. } => Ok(match version_after {
                    Some(v) => format!("flasheado ({chip}, {} MB): el dongle arranca con fw={v}", flash_mb.unwrap_or(0)),
                    None => format!("flasheado ({chip}); el dongle no ha confirmado la versión (¿desenchufar y enchufar?)"),
                }),
            }
        });
    }

    fn ui_sd(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.job.is_some();
        ui.label(format!(
            "Escribe Ventoy {} en modo MBR (arranca en BIOS legacy —HP Gen8/N40L— y en UEFI) con ventoy.json (menú en árbol) y la carpeta /Tools en la partición exFAT «Ventoy». BORRA todo el disco.",
            ventoy::VERSION
        ));
        ui.colored_label(theme::AMBAR, "A través del dongle la SD va a ~1 MB/s: prepárala y copia las ISO mejor con un lector de tarjetas.");
        if cfg!(windows) {
            ui.colored_label(theme::ROJO, "Aún no soportado en Windows: usa macOS o Linux (o `riotkvm sd ventoy`).");
            return;
        }
        ui.add_enabled_ui(!busy, |ui| {
            if ui.small_button("Actualizar lista de discos").clicked() {
                self.refresh_disks(ctx);
            }
        });
        let list = match &self.disks {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(theme::ROJO, e);
                return;
            }
            Some(Ok(v)) => v.clone(),
        };
        if list.is_empty() {
            ui.label("No hay discos externos ni lectores de tarjetas. Pon el dongle en modo A o C (o usa un lector) y actualiza.");
            return;
        }
        ui.add_enabled_ui(!busy, |ui| {
            for d in &list {
                let why = disks::safety(d).err().map(|e| e.to_string());
                ui.add_enabled_ui(why.is_none(), |ui| {
                    let r = ui.radio(self.disk_sel.as_deref() == Some(d.id.as_str()), d.summary());
                    if r.clicked() {
                        self.disk_sel = Some(d.id.clone());
                        self.confirm_text.clear();
                    }
                    if let Some(w) = &why {
                        r.on_disabled_hover_text(w.as_str());
                    }
                });
                if let Some(w) = why {
                    ui.label(egui::RichText::new(format!("    no se puede usar: {w}")).small().color(theme::GRIS));
                }
            }
        });
        let Some(d) = self.disk_sel.as_ref().and_then(|s| list.iter().find(|d| &d.id == s)).cloned() else {
            ui.label("Elige un disco.");
            return;
        };
        ui.separator();
        ui.colored_label(theme::HUESO, format!("Contenido actual de {} (se perderá):", d.id));
        for c in d.contents() {
            ui.label(format!("  {c}"));
        }
        if !d.is_riotkvm() && !d.is_card_reader() {
            ui.colored_label(theme::AMBAR, "Ojo: no parece ni el dongle ni un lector de tarjetas.");
        }
        if let Err(e) = ventoy::plan(d.size) {
            ui.colored_label(theme::ROJO, e.to_string());
            return;
        }
        ui.add_enabled_ui(!busy, |ui| {
            ui.checkbox(&mut self.riotkvm_theme, "Tema RiotKVM (calavera y verde fósforo; legacy y UEFI)");
        });
        ui.label(egui::RichText::new("Copia tus ISOs a la SD: instaladores en la raíz, herramientas en /Tools.").color(theme::CIAN));
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Para confirmar escribe «{}»:", d.id));
                ui.add(egui::TextEdit::singleline(&mut self.confirm_text).desired_width(90.0));
            });
            let ok = disks::confirmed(&d, &self.confirm_text);
            if ui.add_enabled(ok, egui::Button::new(format!("Borrar {} y escribir Ventoy", d.id))).clicked() {
                self.launch_ventoy(ctx, d.clone());
            }
            ui.label(egui::RichText::new("Pedirá la contraseña de administrador una vez (macOS: authopen; Linux: udisks2/polkit). Al terminar se relee todo para verificar.").small().color(theme::GRIS));
        });
    }

    fn launch_ventoy(&mut self, ctx: &egui::Context, d: Disk) {
        let files = ventoy::theme::sd_files(self.riotkvm_theme);
        self.start(ctx, &format!("Ventoy en {}", d.id), move |rep, cancel| {
            let e = |e: riotkvm_flash::Error| e.to_string();
            // paquete primero: si la descarga falla, el disco queda intacto
            let pkg = ventoy::package::load(rep, cancel).map_err(e)?;
            cancel.check().map_err(e)?;
            let d = disks::recheck(&d).map_err(e)?;
            riotkvm_flash::stage(rep, format!("desmontando {}", d.id));
            disks::unmount(&d).map_err(e)?;
            riotkvm_flash::stage(rep, "pidiendo permiso para escribir el disco");
            let mut dev = riotkvm_flash::rawdev::open_device(&d.raw_path, d.size).map_err(e)?;
            let rnd = ventoy::Randomness::new().map_err(e)?;
            let p = ventoy::install(&mut dev, &pkg, &rnd, &files, rep, cancel).map_err(e)?;
            ventoy::verify(&mut dev, &pkg, &rnd, &files, rep).map_err(e)?;
            drop(dev);
            Ok(format!(
                "Ventoy {} escrito y verificado en {} (exFAT «Ventoy» de {}). Copia tus ISOs: instaladores en la raíz, herramientas en /Tools.",
                ventoy::VERSION,
                d.id,
                riotkvm_flash::human_size(p.part1_sectors() * 512)
            ))
        });
    }
}

fn pct(p: &Progress) -> String {
    if p.total > 1 << 20 {
        format!("{:.1}/{:.1} MB", p.done as f64 / 1e6, p.total as f64 / 1e6)
    } else {
        format!("{} %", p.done * 100 / p.total.max(1))
    }
}
