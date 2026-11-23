//! Idioma de la GUI: català, castellano o English, en caliente.
//!
//! `t("clave")` devuelve el texto en el idioma actual; `tf!("clave", a, b)` sustituye `{0}`, `{1}`... de la
//! plantilla traducida (cada idioma puede ordenarlos a su manera). El idioma vive en un atómico global y se guarda en
//! `lang.txt` (directorio de estado de `riotkvm_core::config`). Sin fichero: el del sistema si es ca/es/en; si no, es.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::OnceLock;

const FILE: &str = "lang.txt";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Ca = 0,
    Es = 1,
    En = 2,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::Ca, Lang::Es, Lang::En];

    pub fn code(self) -> &'static str {
        match self {
            Lang::Ca => "ca",
            Lang::Es => "es",
            Lang::En => "en",
        }
    }

    /// Nombre del idioma en ese mismo idioma (botones del selector).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Ca => "Català",
            Lang::Es => "Castellano",
            Lang::En => "English",
        }
    }

    /// `ca`, `es-ES`, `en_US.UTF-8`, `ca-ES@valencia`... -> idioma, si es uno de los tres.
    pub fn from_tag(tag: &str) -> Option<Lang> {
        let base = tag.trim().split(['-', '_', '.', '@']).next().unwrap_or("").to_ascii_lowercase();
        match base.as_str() {
            "ca" => Some(Lang::Ca),
            "es" => Some(Lang::Es),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

static CUR: AtomicU8 = AtomicU8::new(Lang::Es as u8);

/// Al arrancar: lo guardado > el idioma del sistema (si es ca/es/en) > castellano.
pub fn init() {
    let l = riotkvm_core::config::load(FILE).and_then(|t| Lang::from_tag(&t)).or_else(system).unwrap_or(Lang::Es);
    CUR.store(l as u8, Relaxed);
}

/// Primer idioma preferido del sistema que sea uno de los tres (macOS: AppleLanguages; Unix: LC_ALL/LANG...).
fn system() -> Option<Lang> {
    sys_locale::get_locales().find_map(|l| Lang::from_tag(&l))
}

pub fn lang() -> Lang {
    match CUR.load(Relaxed) {
        0 => Lang::Ca,
        2 => Lang::En,
        _ => Lang::Es,
    }
}

/// Cambia el idioma (en caliente) y lo guarda.
pub fn set(l: Lang) {
    CUR.store(l as u8, Relaxed);
    riotkvm_core::config::save(FILE, l.code());
}

fn table() -> &'static HashMap<&'static str, [&'static str; 3]> {
    static T: OnceLock<HashMap<&'static str, [&'static str; 3]>> = OnceLock::new();
    T.get_or_init(|| TEXTS.iter().map(|(k, v)| (*k, *v)).collect())
}

/// Texto de `key` en el idioma actual (la propia clave si no existe: el test lo impide).
pub fn t(key: &'static str) -> &'static str {
    t_or(key)
}

/// Como `t`, pero para textos que pueden no tener traducción (nombres de teclas): si no hay entrada, el propio texto.
pub fn t_or(key: &'static str) -> &'static str {
    table().get(key).map(|v| v[lang() as usize]).unwrap_or(key)
}

/// Sustituye `{0}`, `{1}`... en `tpl`. Lo que no sea `{número}` se copia tal cual.
pub fn fill(tpl: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}').map(|j| (j, after[..j].parse::<usize>())) {
            Some((j, Ok(n))) if n < args.len() => {
                out.push_str(&args[n].to_string());
                rest = &after[j + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `tf!("clave", a, b)`: plantilla traducida con `{0}`, `{1}`... sustituidos.
#[macro_export]
macro_rules! tf {
    ($key:literal $(, $arg:expr)* $(,)?) => {
        $crate::i18n::fill($crate::i18n::t($key), &[$(&$arg as &dyn std::fmt::Display),*])
    };
}

/// (clave, [català, castellano, English]).
static TEXTS: &[(&str, [&str; 3])] = &[
    // ---- ventana principal y barra superior
    ("app.title", ["RiotKVM - KVM de butxaca", "RiotKVM - KVM de bolsillo", "RiotKVM - pocket KVM"]),
    ("top.release", ["[ Allibera ]", "[ Liberar ]", "[ Release ]"]),
    ("top.capture", ["[ Captura ]", "[ Capturar ]", "[ Capture ]"]),
    ("top.mouse", ["ratolí", "ratón", "mouse"]),
    ("top.settings", ["Configuració", "Ajustes", "Settings"]),
    ("top.tools", ["Tools", "Tools", "Tools"]),
    ("top.tools_tip", ["Grava el firmware del dongle · prepara una microSD amb Ventoy", "Flashear el firmware del dongle · preparar una microSD con Ventoy", "Flash the dongle firmware · prepare a microSD with Ventoy"]),
    // menú Vídeo
    ("vid.menu", ["Vídeo", "Vídeo", "Video"]),
    ("vid.source", ["font", "fuente", "source"]),
    ("vid.auto", ["Automàtica (capturadora si n'hi ha)", "Automática (capturadora si hay)", "Automatic (capture card if any)"]),
    ("vid.card", [" (capturadora)", " (capturadora)", " (capture card)"]),
    ("vid.none", ["Cap (sense vídeo)", "Ninguna (sin vídeo)", "None (no video)"]),
    ("vid.refresh", ["Actualitza la llista", "Actualizar lista", "Refresh list"]),
    ("vid.refresh_tip", [
        "Cerca càmeres i capturadores (no n'encén cap); si s'havien aturat els reintents, torna a obrir la font",
        "Buscar cámaras y capturadoras (no enciende ninguna); si se detuvieron los reintentos, vuelve a abrir la fuente",
        "Look for cameras and capture cards (none is turned on); if retries had stopped, reopens the source",
    ]),
    ("vid.res", ["resolució / fps", "resolución / fps", "resolution / fps"]),
    ("vid.low_lat", [" (baixa latència)", " (baja latencia)", " (low latency)"]),
    ("vid.m1080", ["1080p30 · nitidesa", "1080p30 · nitidez", "1080p30 · sharp"]),
    ("vid.m720_60", ["720p60 · fluïdesa", "720p60 · fluidez", "720p60 · smooth"]),
    ("vid.m720_30", ["720p30 · lleuger", "720p30 · ligero", "720p30 · light"]),
    ("vid.mode_missing", ["el mode {0} no existeix en aquesta font: s'usa {1}", "el modo {0} no existe en esta fuente: se usa {1}", "mode {0} isn't available on this source: using {1}"]),
    ("vid.none_err", [
        "Sense vídeo: {0}\n(si diu 'recursos': un altre programa o finestra fa servir la capturadora; es reintenta cada 2 s)",
        "Sin vídeo: {0}\n(si dice 'recursos': otro programa o ventana usa la capturadora; se reintenta cada 2 s)",
        "No video: {0}\n(if it says 'recursos': another program or window is using the capture card; retrying every 2 s)",
    ]),
    ("vid.waiting", ["Esperant el vídeo...", "Esperando vídeo...", "Waiting for video..."]),
    // menú Enviar teclas
    ("keys.menu", ["Envia tecles", "Enviar teclas", "Send keys"]),
    ("keys.system", ["Sistema", "Sistema", "System"]),
    ("keys.fn", ["Funcions / BIOS", "Funciones / BIOS", "Function keys / BIOS"]),
    ("keys.custom", ["Combinació personalitzada (com a riotkvm key):", "Combinación personalizada (como riotkvm key):", "Custom combo (same as riotkvm key):"]),
    ("keys.custom_hint", ["ctrl+alt+del   o   alt+f4 enter", "ctrl+alt+del   o   alt+f4 enter", "ctrl+alt+del   or   alt+f4 enter"]),
    ("keys.send", ["Envia", "Enviar", "Send"]),
    ("keys.repeat", ["Mantenir premuda / repetir (entrar a la BIOS):", "Mantener pulsada / repetir (entrar en la BIOS):", "Hold / repeat (enter the BIOS):"]),
    ("keys.stop", ["Atura", "Parar", "Stop"]),
    ("keys.start_repeat", ["Inicia la repetició", "Iniciar repetición", "Start repeating"]),
    // nombres de teclas que cambian según el idioma (el resto se muestran tal cual)
    ("Ctrl+Alt+Supr / Del", ["Ctrl+Alt+Supr / Del", "Ctrl+Alt+Supr / Del", "Ctrl+Alt+Del"]),
    ("Impr Pant", ["Impr Pant", "Impr Pant", "Print Screen"]),
    ("Alt+SysRq (luego la tecla de orden)", ["Alt+SysRq (després la tecla d'ordre)", "Alt+SysRq (luego la tecla de orden)", "Alt+SysRq (then the command key)"]),
    ("Ctrl+Alt+Retroceso", ["Ctrl+Alt+Retrocés", "Ctrl+Alt+Retroceso", "Ctrl+Alt+Backspace"]),
    ("Supr", ["Supr", "Supr", "Del"]),
    ("RePág", ["RePàg", "RePág", "PgUp"]),
    ("AvPág", ["AvPàg", "AvPág", "PgDn"]),
    // menú Pegar
    ("paste.menu", ["Enganxa", "Pegar", "Paste"]),
    ("paste.clip", ["Enganxa el porta-retalls ({0})", "Pegar portapapeles ({0})", "Paste clipboard ({0})"]),
    ("paste.type", ["Escriu un text...", "Escribir texto...", "Type text..."]),
    ("paste.by_layout", ["text (segons el layout del servidor)", "texto (por layout del servidor)", "text (server layout)"]),
    ("paste.physical", ["tecles físiques (posició US, per a la BIOS)", "teclas físicas (posición US, para BIOS)", "physical keys (US positions, for BIOS)"]),
    ("paste.ms", ["ms per tecla", "ms por tecla", "ms per key"]),
    ("paste.confirm", ["confirma sempre (si no, només > {0} caràcters)", "confirmar siempre (si no, solo > {0} caracteres)", "always confirm (otherwise only > {0} characters)"]),
    // avisos de la barra de estado
    ("n.busy", ["ja hi ha una feina en curs (Esc la cancel·la)", "ya hay un trabajo en curso (Esc lo cancela)", "a job is already running (Esc cancels it)"]),
    ("n.empty", ["no hi ha cap text per teclejar", "no hay texto que teclear", "nothing to type"]),
    ("n.bad_chars", ["AVÍS: es saltaran {0} caràcters que el layout {1} no pot representar: {2}", "AVISO: {0} caracteres no representables en el layout {1} se saltarán: {2}", "WARNING: {0} characters not available in layout {1} will be skipped: {2}"]),
    ("n.typing", ["teclejant {0} caràcters (Esc cancel·la)...", "tecleando {0} caracteres (Esc cancela)...", "typing {0} characters (Esc cancels)..."]),
    ("n.bad_combo", ["combinació no vàlida: {0}", "combinación no válida: {0}", "invalid key combo: {0}"]),
    ("n.sending", ["enviant: {0}", "enviando: {0}", "sending: {0}"]),
    ("n.repeating", ["repetint {0} cada {1} ms durant {2} s (Esc / Atura ho para)", "repitiendo {0} cada {1} ms durante {2} s (Esc / Parar lo detiene)", "repeating {0} every {1} ms for {2} s (Esc / Stop ends it)"]),
    ("n.layout", ["distribució del teclat del servidor: {0}", "distribución del teclado del servidor: {0}", "server keyboard layout: {0}"]),
    ("n.target", ["equip controlat: {0} (clics {1})", "equipo controlado: {0} (clics {1})", "controlled machine: {0} (clicks {1})"]),
    ("n.via_rel", ["pel ratolí relatiu", "por el ratón relativo", "via the relative mouse"]),
    ("n.via_abs", ["per l'informe absolut", "por el informe absoluto", "via the absolute report"]),
    ("job.paste", ["enganxar", "pegar", "paste"]),
    ("job.repeat", ["repetir {0}", "repetir {0}", "repeat {0}"]),
    // barras inferiores
    ("bar.kbd_full", ["teclat complet", "teclado total", "full keyboard"]),
    ("bar.kbd_limited", ["teclat limitat", "teclado limitado", "limited keyboard"]),
    ("bar.captured", ["CAPTURAT ({0}, ratolí {1}) · per ALLIBERAR prem {2} (o canvia de finestra)", "CAPTURADO ({0}, ratón {1}) · para SOLTAR pulsa {2} (o cambia de ventana)", "CAPTURED ({0}, {1} mouse) · to RELEASE press {2} (or switch windows)"]),
    ("bar.bios", ["BIOS (sense ratolí)", "BIOS (ratón no disponible)", "BIOS (no mouse)"]),
    ("bar.stats", [
        "vídeo {0}x{1} · edat {2}→{3} ms · dec {4} ms · descartats {5}\nenviat: tecles {6} · clics {7} · mov {8} · err {9} · hook {10} (n'ha vist {11})",
        "vídeo {0}x{1} · edad {2}→{3} ms · dec {4} ms · descartados {5}\nenviado: teclas {6} · clics {7} · mov {8} · err {9} · hook {10} (vio {11})",
        "video {0}x{1} · age {2}→{3} ms · dec {4} ms · dropped {5}\nsent: keys {6} · clicks {7} · moves {8} · err {9} · hook {10} (saw {11})",
    ]),
    ("bar.ev_write", ["esdeveniment→write", "evento→write", "event→write"]),
    ("bar.click_to_capture", ["clic per capturar", "clic para capturar", "click to capture"]),
    ("bar.cancel_esc", ["Cancel·la (Esc)", "Cancelar (Esc)", "Cancel (Esc)"]),
    // ventanas Escribir texto y Confirmar pegado
    ("pw.title", ["Escriu un text al servidor", "Escribir texto en el servidor", "Type text on the server"]),
    ("pw.layout", ["Es tecleja amb el layout {0} ({1}).", "Se teclea con el layout {0} ({1}).", "Typed with layout {0} ({1})."]),
    ("pw.us_physical", ["us (tecles físiques)", "us (teclas físicas)", "us (physical keys)"]),
    ("pw.server", ["servidor", "servidor", "server"]),
    ("pw.chars", ["{0} caràcters", "{0} caracteres", "{0} characters"]),
    ("pw.type", ["Tecleja-ho al servidor", "Teclear en el servidor", "Type on the server"]),
    ("pw.clear", ["Esborra", "Limpiar", "Clear"]),
    ("cp.title", ["Confirma l'enganxada", "Confirmar pegado", "Confirm paste"]),
    ("cp.summary", ["{0} caràcters, {1} línies, layout {2}; temps estimat ~{3} s (+ esperes de l'enllaç).", "{0} caracteres, {1} líneas, layout {2}; tiempo estimado ~{3} s (+ esperas del enlace).", "{0} characters, {1} lines, layout {2}; about ~{3} s (+ link waits)."]),
    ("cp.bad", ["{0} caràcters NO representables amb aquest layout (es saltaran): {1}", "{0} caracteres NO representables en este layout (se saltarán): {1}", "{0} characters NOT available in this layout (will be skipped): {1}"]),
    ("cp.type", ["Tecleja", "Teclear", "Type"]),
    ("w.cancel", ["Cancel·la", "Cancelar", "Cancel"]),
    // ---- Ajustes
    ("s.tab_target", ["Servidor", "Servidor", "Server"]),
    ("s.tab_keyboard", ["Teclat", "Teclado", "Keyboard"]),
    ("s.tab_light", ["Llum", "Luz", "Light"]),
    ("s.tab_lang", ["Idioma", "Idioma", "Language"]),
    ("s.tab_info", ["Informació", "Información", "About"]),
    ("s.os_title", ["Sistema del servidor", "Sistema del servidor", "Server OS"]),
    ("s.os_hint", ["Si el clic no respon, canvia el sistema.", "Si el clic no responde, cambia el sistema.", "If clicks don't work, change the OS."]),
    ("s.advanced", ["Avançat", "Avanzado", "Advanced"]),
    ("s.clicks_via", ["Clics per:", "Clics por:", "Clicks via:"]),
    ("s.abs", ["absolut", "absoluto", "absolute"]),
    ("s.rel", ["relatiu", "relativo", "relative"]),
    ("s.rel_macos", ["Ratolí en rel: a macOS el clic no funciona, fes servir abs.", "Ratón en rel: en macOS el clic no funciona, usa abs.", "Mouse in rel: clicks don't work on macOS, use abs."]),
    ("s.rel_na", ["Ratolí en rel: aquest ajust no s'aplica.", "Ratón en rel: este ajuste no se aplica.", "Mouse in rel: this setting doesn't apply."]),
    ("s.env_note", ["RIOTKVM_ABS_BUTTONS ha fixat l'arrencada.", "RIOTKVM_ABS_BUTTONS fijó el arranque.", "RIOTKVM_ABS_BUTTONS set the startup value."]),
    ("s.manual", ["manual", "manual", "manual"]),
    ("s.auto", ["Automàtica", "Automático", "Auto"]),
    ("s.auto_tip", ["Color segons el mode, BLE i SD", "Color según modo, BLE y SD", "Color by mode, BLE and SD"]),
    ("s.color", ["Color", "Color", "Color"]),
    ("s.off", ["Apagada", "Apagado", "Off"]),
    ("s.bright", ["Brillantor", "Brillo", "Brightness"]),
    ("s.no_led", ["Firmware sense control de la llum: actualitza'l.", "Firmware sin control de la luz: actualízalo.", "Firmware has no light control: update it."]),
    ("c.green", ["verd", "verde", "green"]),
    ("c.red", ["vermell", "rojo", "red"]),
    ("c.orange", ["taronja", "naranja", "orange"]),
    ("c.blue", ["blau", "azul", "blue"]),
    ("c.purple", ["lila", "lila", "purple"]),
    ("c.yellow", ["groc", "amarillo", "yellow"]),
    ("c.white", ["blanc", "blanco", "white"]),
    ("s.kbd_title", ["Idioma del teclat del servidor", "Idioma del teclado del servidor", "Server keyboard layout"]),
    ("s.lang_title", ["Idioma de l'aplicació", "Idioma de la aplicación", "App language"]),
    ("s.info_mouse", ["ratolí: {0}", "ratón: {0}", "mouse: {0}"]),
    ("s.rel_bios", ["relatiu (BIOS)", "relativo (BIOS)", "relative (BIOS)"]),
    ("s.info_release", ["alliberar la captura: {0}", "soltar la captura: {0}", "release capture: {0}"]),
    ("s.info_paste", ["enganxar el porta-retalls: {0}", "pegar el portapapeles: {0}", "paste clipboard: {0}"]),
    ("tr.ble_mock", ["BLE simulat", "BLE simulado", "simulated BLE"]),
    ("tr.serial", ["sèrie", "serie", "serial"]),
    ("h.no_chord", ["(sense drecera)", "(sin atajo)", "(no shortcut)"]),
    // ---- portapapeles
    ("cb.open", ["no s'ha pogut obrir el porta-retalls (el fa servir un altre programa?)", "no se pudo abrir el portapapeles (¿lo está usando otro programa?)", "couldn't open the clipboard (is another program using it?)"]),
    ("cb.files", ["el porta-retalls conté fitxers: només es tecleja text", "el portapapeles contiene archivos: solo se teclea texto", "the clipboard holds files: only text can be typed"]),
    ("cb.image", ["el porta-retalls conté una imatge: només es tecleja text", "el portapapeles contiene una imagen: solo se teclea texto", "the clipboard holds an image: only text can be typed"]),
    ("cb.no_text", ["el porta-retalls no conté text", "el portapapeles no contiene texto", "the clipboard has no text"]),
    ("cb.read", ["no s'ha pogut llegir el porta-retalls", "no se pudo leer el portapapeles", "couldn't read the clipboard"]),
    ("cb.lock", ["no s'ha pogut bloquejar el porta-retalls", "no se pudo bloquear el portapapeles", "couldn't lock the clipboard"]),
    ("cb.windows_only", [
        "només es pot llegir el porta-retalls directament a Windows: fes Ctrl+V amb la finestra sense capturar",
        "la lectura directa del portapapeles solo existe en Windows: usa Ctrl+V con la ventana sin capturar",
        "reading the clipboard directly only works on Windows: press Ctrl+V with the window not captured",
    ]),
    // ---- Tools
    ("tl.tab_fw", ["Firmware del dongle", "Firmware del dongle", "Dongle firmware"]),
    ("tl.tab_sd", ["microSD amb Ventoy", "microSD con Ventoy", "microSD with Ventoy"]),
    ("tl.writing", ["escrivint: no desconnectis res (ja no es pot cancel·lar)", "escribiendo: no desenchufes nada (ya no se puede cancelar)", "writing: don't unplug anything (can't cancel now)"]),
    ("tl.details", ["detalls ({0})", "detalles ({0})", "details ({0})"]),
    ("tl.installed", ["Instal·lada:", "Instalada:", "Installed:"]),
    ("tl.name", ["Nom:", "Nombre:", "Name:"]),
    ("tl.name_offline", ["sense connexió", "sin conexión", "not connected"]),
    ("tl.name_old_fw", ["actualitza el firmware (1.3.0+) per canviar-li el nom", "actualiza el firmware (1.3.0+) para renombrarlo", "update the firmware (1.3.0+) to rename it"]),
    ("tl.name_save", ["Desa", "Guardar", "Save"]),
    ("tl.name_rules", ["A-Z 0-9 . _ - (màx. 20)", "A-Z 0-9 . _ - (máx. 20)", "A-Z 0-9 . _ - (max. 20)"]),
    ("tl.checking", ["comprovant...", "comprobando...", "checking..."]),
    ("tl.published", ["Publicada:", "Publicada:", "Published:"]),
    ("tl.querying", ["consultant GitHub...", "consultando GitHub...", "querying GitHub..."]),
    ("tl.no_release", ["encara no hi ha cap release publicada", "aún no hay ninguna release publicada", "no release published yet"]),
    ("tl.query_failed", ["no s'ha pogut consultar: {0}", "no se pudo consultar: {0}", "couldn't check: {0}"]),
    ("tl.check_again", ["Torna a comprovar", "Comprobar otra vez", "Check again"]),
    ("tl.what", ["què cal gravar", "qué flashear", "what to flash"]),
    ("tl.src_release", ["l'última release (verificada per SHA-256)", "la última release (verificada por SHA-256)", "latest release (SHA-256 verified)"]),
    ("tl.src_file", ["un factory.bin propi (s'escriu a 0x0)", "un factory.bin propio (se escribe en 0x0)", "your own factory.bin (written at 0x0)"]),
    ("tl.file_hint", ["ruta del .bin (o arrossega'l a la finestra)", "ruta del .bin (o arrástralo a la ventana)", ".bin path (or drop it on the window)"]),
    ("tl.file_warn", [
        "Binari propi: NO se'n verifica l'origen. Es comprova que la capçalera sigui del mateix xip que el connectat; en un ESP32-S3 que no sigui el T-Dongle-S3 només té sentit amb el teu propi firmware.",
        "Binario propio: NO se verifica su origen. Se comprueba que la cabecera sea del mismo chip que el conectado; en un ESP32-S3 que no sea el T-Dongle-S3 solo tiene sentido con tu propio firmware.",
        "Custom binary: its origin is NOT verified. Only the header is checked against the connected chip; on an ESP32-S3 other than the T-Dongle-S3 it only makes sense with your own firmware.",
    ]),
    ("tl.port", ["port:", "puerto:", "port:"]),
    ("tl.port_auto", ["automàtic (dongle RiotKVM o ROM 303A)", "automático (dongle RiotKVM o ROM 303A)", "automatic (RiotKVM dongle or ROM 303A)"]),
    ("tl.rescan_tip", ["torna a cercar ports", "volver a buscar puertos", "rescan ports"]),
    ("tl.downgrade", ["permet baixar de versió", "permitir bajar de versión", "allow downgrade"]),
    ("tl.dry_run", ["simulació (no obre el port ni escriu res)", "simulación (no abre el puerto ni escribe nada)", "dry run (doesn't open the port or write anything)"]),
    ("tl.no_fw_pick_bin", ["no hi ha cap firmware publicat per gravar: tria un .bin propi", "no hay firmware publicado para flashear: elige un .bin propio", "no published firmware to flash: pick your own .bin"]),
    ("tl.upgrade", ["hi ha una versió nova: {0} → {1}", "hay versión nueva: {0} → {1}", "new version: {0} → {1}"]),
    ("tl.same", ["ja tens aquesta versió (es pot reinstal·lar)", "ya tienes esa versión (se puede reinstalar)", "you already have this version (can reinstall)"]),
    ("tl.older", ["la publicada és MÉS ANTIGA que la instal·lada", "la publicada es MÁS ANTIGUA que la instalada", "the published one is OLDER than the installed one"]),
    ("tl.unknown", ["versió instal·lada desconeguda: s'instal·larà la publicada", "versión instalada desconocida: se instalará la publicada", "installed version unknown: the published one will be installed"]),
    ("tl.pick_bin", ["tria el fitxer .bin", "elige el fichero .bin", "pick the .bin file"]),
    ("tl.boot_tip", ["si el dongle no respon a l'ordre bootloader (firmware trencat o aliè)", "si el dongle no responde a la orden bootloader (firmware roto o ajeno)", "if the dongle ignores the bootloader command (broken or foreign firmware)"]),
    ("tl.simulate", ["Simula la gravació", "Simular flasheo", "Simulate flash"]),
    ("tl.flash", ["Grava...", "Flashear...", "Flash..."]),
    ("tl.flash_warn", ["El dongle es reiniciarà en mode descàrrega i se'n reescriurà tota la flash.", "El dongle se reiniciará en modo descarga y se reescribirá su flash entera.", "The dongle will reboot into download mode and its whole flash will be rewritten."]),
    ("tl.yes_flash", ["Sí, grava", "Sí, flashear", "Yes, flash"]),
    ("tl.no", ["No", "No", "No"]),
    ("tl.job_dry", ["Simulació de la gravació", "Simulación del flasheo", "Flash dry run"]),
    ("tl.job_flash", ["Gravant el firmware", "Flasheando el firmware", "Flashing the firmware"]),
    ("tl.no_fw", ["no hi ha cap firmware publicat", "no hay firmware publicado", "no published firmware"]),
    ("tl.dry_ok", ["simulació correcta: no s'ha tocat el dongle (detalls a sota)", "simulación correcta: no se ha tocado el dongle (detalles abajo)", "dry run OK: the dongle wasn't touched (details below)"]),
    ("tl.flashed", ["gravat ({0}, {1} MB): el dongle arrenca amb fw={2}", "flasheado ({0}, {1} MB): el dongle arranca con fw={2}", "flashed ({0}, {1} MB): the dongle boots with fw={2}"]),
    ("tl.flashed_nover", ["gravat ({0}); el dongle no ha confirmat la versió (cal desconnectar-lo i tornar-lo a connectar?)", "flasheado ({0}); el dongle no ha confirmado la versión (¿desenchufar y enchufar?)", "flashed ({0}); the dongle didn't confirm the version (unplug and replug?)"]),
    ("tl.sd_intro", [
        "Escriu Ventoy {0} en mode MBR (arrenca en BIOS legacy —HP Gen8/N40L— i en UEFI) amb ventoy.json (menú en arbre) i la carpeta /Tools a la partició exFAT «Ventoy». ESBORRA tot el disc.",
        "Escribe Ventoy {0} en modo MBR (arranca en BIOS legacy —HP Gen8/N40L— y en UEFI) con ventoy.json (menú en árbol) y la carpeta /Tools en la partición exFAT «Ventoy». BORRA todo el disco.",
        "Writes Ventoy {0} in MBR mode (boots on legacy BIOS —HP Gen8/N40L— and UEFI) with ventoy.json (tree menu) and the /Tools folder on the exFAT partition \"Ventoy\". ERASES the whole disk.",
    ]),
    ("tl.sd_slow", [
        "A través del dongle la SD va a ~1 MB/s: val més preparar-la i copiar-hi les ISO amb un lector de targetes.",
        "A través del dongle la SD va a ~1 MB/s: prepárala y copia las ISO mejor con un lector de tarjetas.",
        "Through the dongle the SD runs at ~1 MB/s: better prepare it and copy the ISOs with a card reader.",
    ]),
    ("tl.sd_windows", ["Encara no és compatible amb Windows: fes servir macOS o Linux (o `riotkvm sd ventoy`).", "Aún no soportado en Windows: usa macOS o Linux (o `riotkvm sd ventoy`).", "Not supported on Windows yet: use macOS or Linux (or `riotkvm sd ventoy`)."]),
    ("tl.refresh_disks", ["Actualitza la llista de discos", "Actualizar lista de discos", "Refresh disk list"]),
    ("tl.no_disks", [
        "No hi ha discos externs ni lectors de targetes. Posa el dongle en mode A o C (o fes servir un lector) i actualitza.",
        "No hay discos externos ni lectores de tarjetas. Pon el dongle en modo A o C (o usa un lector) y actualiza.",
        "No external disks or card readers. Put the dongle in mode A or C (or use a reader) and refresh.",
    ]),
    ("tl.unusable", ["no es pot fer servir: {0}", "no se puede usar: {0}", "can't be used: {0}"]),
    ("tl.pick_disk", ["Tria un disc.", "Elige un disco.", "Pick a disk."]),
    ("tl.contents", ["Contingut actual de {0} (es perdrà):", "Contenido actual de {0} (se perderá):", "Current contents of {0} (will be lost):"]),
    ("tl.odd_disk", ["Compte: no sembla ni el dongle ni un lector de targetes.", "Ojo: no parece ni el dongle ni un lector de tarjetas.", "Careful: this doesn't look like the dongle or a card reader."]),
    ("tl.copy_isos", ["Copia les teves ISO a la SD: instal·ladors a l'arrel, eines a /Tools.", "Copia tus ISOs a la SD: instaladores en la raíz, herramientas en /Tools.", "Copy your ISOs to the SD: installers at the root, tools in /Tools."]),
    ("tl.confirm", ["Per confirmar, escriu «{0}»:", "Para confirmar escribe «{0}»:", "To confirm, type \"{0}\":"]),
    ("tl.erase", ["Esborra {0} i escriu-hi Ventoy", "Borrar {0} y escribir Ventoy", "Erase {0} and write Ventoy"]),
    ("tl.admin", [
        "Demanarà la contrasenya d'administrador un cop (macOS: authopen; Linux: udisks2/polkit). En acabar es torna a llegir tot per verificar-ho.",
        "Pedirá la contraseña de administrador una vez (macOS: authopen; Linux: udisks2/polkit). Al terminar se relee todo para verificar.",
        "You'll be asked for the admin password once (macOS: authopen; Linux: udisks2/polkit). Everything is read back at the end to verify.",
    ]),
    ("tl.job_ventoy", ["Ventoy a {0}", "Ventoy en {0}", "Ventoy on {0}"]),
    ("tl.unmounting", ["desmuntant {0}", "desmontando {0}", "unmounting {0}"]),
    ("tl.asking", ["demanant permís per escriure al disc", "pidiendo permiso para escribir el disco", "asking for permission to write the disk"]),
    ("tl.ventoy_done", [
        "Ventoy {0} escrit i verificat a {1} (exFAT «Ventoy» de {2}). Copia les teves ISO: instal·ladors a l'arrel, eines a /Tools.",
        "Ventoy {0} escrito y verificado en {1} (exFAT «Ventoy» de {2}). Copia tus ISOs: instaladores en la raíz, herramientas en /Tools.",
        "Ventoy {0} written and verified on {1} ({2} exFAT \"Ventoy\"). Copy your ISOs: installers at the root, tools in /Tools.",
    ]),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Marcadores `{n}` de una plantilla, ordenados.
    fn marks(s: &str) -> Vec<usize> {
        let mut v = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            rest = &rest[i + 1..];
            if let Some(j) = rest.find('}') {
                if let Ok(n) = rest[..j].parse() {
                    v.push(n);
                }
            }
        }
        v.sort();
        v
    }

    #[test]
    fn every_key_has_three_languages_and_same_placeholders() {
        let mut seen = std::collections::HashSet::new();
        for (k, v) in TEXTS {
            assert!(seen.insert(*k), "clave repetida: {k}");
            for (i, s) in v.iter().enumerate() {
                assert!(!s.trim().is_empty(), "{k}: falta el texto en {}", Lang::ALL[i].code());
                assert_eq!(marks(s), marks(v[1]), "{k}: marcadores distintos en {}", Lang::ALL[i].code());
            }
        }
    }

    /// Toda clave usada con `t("...")` o `tf!("...")` en el código de la GUI existe en la tabla.
    #[test]
    fn every_used_key_exists() {
        let sources = [include_str!("app.rs"), include_str!("settings.rs"), include_str!("tools.rs"), include_str!("clip.rs"), include_str!("hook.rs"), include_str!("main.rs")];
        let mut n = 0;
        for src in sources {
            for pat in ["t(\"", "tf!(\""] {
                let mut from = 0;
                while let Some(i) = src[from..].find(pat).map(|i| i + from) {
                    from = i + pat.len();
                    let prev = src[..i].chars().next_back();
                    if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        continue; // hint_text(", .on_hover_text("... no son llamadas a t()
                    }
                    let key = &src[from..from + src[from..].find('"').unwrap()];
                    assert!(table().contains_key(key), "falta la clave {key:?} en i18n::TEXTS");
                    n += 1;
                }
            }
        }
        assert!(n > 100, "solo {n} usos de t()/tf!: ¿ha cambiado la sintaxis?");
    }

    #[test]
    fn fill_and_tags() {
        assert_eq!(fill("{1} y {0} {x} {", &[&"a", &2]), "2 y a {x} {");
        assert_eq!(fill("{10}-{1}", &[&0, &1, &2, &3, &4, &5, &6, &7, &8, &9, &"diez"]), "diez-1");
        assert_eq!(Lang::from_tag("ca-ES@valencia"), Some(Lang::Ca));
        assert_eq!(Lang::from_tag("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::from_tag("es"), Some(Lang::Es));
        assert_eq!(Lang::from_tag("fr-FR"), None);
        for l in Lang::ALL {
            assert_eq!(Lang::from_tag(l.code()), Some(l));
        }
    }
}
