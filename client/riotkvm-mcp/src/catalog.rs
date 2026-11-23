//! Lo que el modelo sabe del QKVM: instrucciones del servidor, la API tipada (en la descripción de
//! `execute`, como en Code Mode de Cloudflare) y el catálogo de documentación que consulta `search`.

pub const INSTRUCTIONS: &str = r#"QKVM = KVM de bolsillo para manejar un servidor/PC FÍSICO como si estuvieras delante, sin red ni software en él.
- Un dongle USB (ESP32-S3, "QKVM") va enchufado al servidor y se presenta como teclado + ratón USB (y opcionalmente como pendrive con la microSD/Ventoy).
- Este portátil le habla por Bluetooth LE (o por serie si está enchufado aquí) y ve la pantalla del servidor con una capturadora HDMI→USB.
- Sirve para lo que no se puede hacer por SSH/RDP: BIOS/UEFI, menú de arranque, instalar un SO desde ISO, consola de rescate, equipos sin red o colgados.

Dos herramientas (Code Mode): `search` consulta la documentación (escribe JS contra `catalog`/`find()`), y `execute` ejecuta JS en un sandbox con la API `qkvm` (teclado, ratón, captura de pantalla). Agrupa varias acciones en un mismo `execute` y verifica con `qkvm.screenshot()`.
Bucle recomendado: screenshot → decidir → actuar (type/key/click) → sleep breve → screenshot para verificar. Nunca asumas que algo funcionó sin verlo.
Es una máquina real: pide confirmación al usuario antes de acciones destructivas o irreversibles (borrar/particionar discos, guardar cambios en la BIOS, reinstalar, apagar)."#;

pub const SEARCH_DESC: &str = r#"Busca en la documentación del QKVM (qué es, topología, API qkvm, teclas, layouts, errores, recetas de BIOS/arranque/instalación). Escribe JavaScript que se ejecuta en un sandbox con:
  catalog: Array<{ id: string, kind: "doc"|"api"|"recipe"|"ref", title: string, tags: string[], text: string }>
  find(query: string, limit = 6): mismos objetos ordenados por relevancia (palabras clave, sin distinguir mayúsculas/acentos)
Devuelve el valor con `return` (o una expresión). Ejemplos:
  return find("bios arranque f12")
  return catalog.filter(e => e.kind === "api").map(e => e.title)
  return catalog.find(e => e.id === "keys").text
Úsalo antes de `execute` si no sabes cómo hacer algo, o ante un error del dongle."#;

pub const EXECUTE_DESC: &str = r#"Ejecuta JavaScript en un sandbox (QuickJS, sin red/ficheros) con la API `qkvm`, que controla el teclado y el ratón del servidor físico a través del dongle QKVM y captura su pantalla por HDMI. La conexión con el dongle es persistente entre llamadas (la 1.ª tarda unos segundos: BLE). Al terminar cada llamada se sueltan todas las teclas y botones.
Escribe un cuerpo de función con `return` (se admite `await`, aunque todas las llamadas son síncronas), una expresión suelta o `async () => { ... }`. `console.log` va al resultado. Las capturas se adjuntan como imágenes (máx. 6 por llamada).

declare const qkvm: {
  connection(): { connected: boolean; link: string; port: string; layout: string; video: string | null; coordSpace: [number, number] | null }; // no conecta
  status(): Record<string, string | number | boolean>; // conecta si hace falta. host=servidor enumeró el USB, hidready, proto (boot = BIOS), leds (bit0 NumLock, bit1 CapsLock), mode (A/B/C), link
  info(): string;                                   // versión de firmware del dongle
  screenshot(o?: { maxWidth?: number /*1280*/; format?: "jpeg" | "png"; quality?: number /*75*/; settleMs?: number /*250: espera antes de capturar*/ }):
    { width: number; height: number; sourceWidth: number; sourceHeight: number; source: string; image: number };
  type(text: string, o?: { enter?: boolean; delay?: number /*s entre caracteres*/; fw?: boolean }): { typed: number }; // usa el layout del SERVIDOR (ñ, @, acentos OK)
  key(...combos: string[]): void;                   // "enter", "ctrl+alt+del", "win+r", "f2", "alt+f4", "shift+tab". Último arg opcional {hold: s}
  keyDown(combo: string): void; keyUp(combo: string): void; releaseAll(): void;
  mouse: {
    move(x: number, y: number): void;               // PÍXELES de la última captura enviada (el escalado se hace solo)
    click(x?: number, y?: number, o?: { button?: "left" | "right" | "middle"; count?: number }): void; // también click("right") o click({x, y, button})
    doubleClick(x: number, y: number): void;
    down(button?: string): void; up(button?: string): void; // arrastrar: move, down, move, up
    scroll(amount: number): void;                   // positivo = arriba
    moveAbs(x: number, y: number): void;            // 0..32767 sin captura
    moveRel(dx: number, dy: number): void;          // ratón relativo (-127..127 por paso)
  };
  setLayout(name: "es" | "us" | "gb" | "de" | "fr" | "it" | "pt" | "latam"): string; // layout de teclado del SERVIDOR (defecto es)
  sleep(seconds: number): void;                     // también sleep(ms) global y setTimeout síncrono
  raw(line: string): string;                        // orden CDC cruda (diagnóstico); bootloader/mode/reboot bloqueadas
  disconnect(): void;                               // libera el BLE (para que la GUI dkvm-gui pueda reconectar)
  video: { devices(): Array<{ index: number; name: string; captureCard: boolean }>; stop(): void };
};

Ejemplo:
  qkvm.key("win+r"); qkvm.sleep(0.5);
  qkvm.type("cmd", { enter: true }); qkvm.sleep(1);
  return qkvm.screenshot();
Si no sabes cómo hacer algo (BIOS, menú de arranque, Ventoy, errores `bootproto`/`nohost`), consulta antes `search`."#;

pub const SEARCH_PRELUDE: &str = r#"
const __norm = s => String(s).toLowerCase().normalize("NFD").replace(/[̀-ͯ]/g, "");
const find = (query, limit = 6) => {
  const words = __norm(query).split(/[^a-z0-9+_.-]+/).filter(w => w.length > 1);
  return catalog
    .map(e => {
      const t = __norm(e.title), g = __norm(e.tags.join(" ")), x = __norm(e.text);
      let s = 0;
      for (const w of words) { if (t.includes(w)) s += 5; if (g.includes(w)) s += 3; if (x.includes(w)) s += 1 + Math.min(3, x.split(w).length - 2); if (e.id === w) s += 10; }
      return [s, e];
    })
    .filter(([s]) => s > 0).sort((a, b) => b[0] - a[0]).slice(0, limit).map(([, e]) => e);
};
globalThis.find = find;
"#;

pub const EXEC_PRELUDE: &str = r#"
const __call = (op, args) => {
  const r = JSON.parse(__host(op, JSON.stringify(args === undefined ? {} : args)));
  if (!r.ok) throw new Error(r.e);
  return r.v;
};
const __fmt = a => a.map(x => typeof x === "string" ? x : (() => { try { return JSON.stringify(x); } catch (e) { return String(x); } })()).join(" ");
globalThis.console = {
  log: (...a) => __call("log", { text: __fmt(a) }),
  info: (...a) => __call("log", { text: __fmt(a) }),
  debug: (...a) => __call("log", { text: __fmt(a) }),
  warn: (...a) => __call("log", { text: "[warn] " + __fmt(a) }),
  error: (...a) => __call("log", { text: "[error] " + __fmt(a) }),
};
const __clickArgs = (a, b, c) => {
  if (typeof a === "number") return Object.assign({ x: a, y: b }, c || {});
  if (typeof a === "string") return Object.assign({ button: a }, b || {});
  return a || {};
};
const qkvm = {
  connection: () => __call("connection"),
  status: () => __call("status"),
  info: () => __call("info"),
  disconnect: () => __call("disconnect"),
  setLayout: name => __call("setLayout", { name }),
  type: (text, o) => __call("type", Object.assign({ text: String(text) }, o || {})),
  key: (...combos) => {
    let hold = 0;
    if (combos.length && typeof combos[combos.length - 1] === "object" && !Array.isArray(combos[combos.length - 1])) hold = combos.pop().hold || 0;
    return __call("key", { combos: combos.flat().map(String), hold });
  },
  keyDown: combo => __call("keyDown", { combo }),
  keyUp: combo => __call("keyUp", { combo }),
  releaseAll: () => __call("release"),
  mouse: Object.freeze({
    move: (x, y) => __call("move", { x, y }),
    moveAbs: (x, y) => __call("moveAbs", { x, y }),
    moveRel: (dx, dy) => __call("moveRel", { dx, dy }),
    click: (a, b, c) => __call("click", __clickArgs(a, b, c)),
    doubleClick: (x, y, o) => __call("click", Object.assign({ x, y, count: 2 }, o || {})),
    rightClick: (x, y) => __call("click", { x, y, button: "right" }),
    down: (button, o) => __call("down", Object.assign({ button: button || "left" }, o || {})),
    up: (button, o) => __call("up", Object.assign({ button: button || "left" }, o || {})),
    scroll: (amount, o) => __call("scroll", Object.assign({ amount }, o || {})),
  }),
  screenshot: o => __call("screenshot", o || {}),
  sleep: secs => __call("sleep", { secs: Number(secs) }),
  raw: line => __call("raw", { line: String(line) }),
  video: Object.freeze({ devices: () => __call("videoDevices"), stop: () => __call("stopVideo") }),
};
globalThis.qkvm = Object.freeze(qkvm);
globalThis.sleep = ms => qkvm.sleep(ms / 1000);
globalThis.setTimeout = (fn, ms) => { qkvm.sleep((ms || 0) / 1000); if (typeof fn === "function") fn(); return 0; };
"#;

/// Catálogo para `search` (JSON). Cada entrada: id, kind, title, tags, text.
pub const CATALOG: &str = r#"[
{"id":"what-is-qkvm","kind":"doc","title":"Qué es el QKVM y para qué sirve","tags":["qkvm","kvm","intro","topologia","para que sirve","uso"],
"text":"QKVM es un KVM de bolsillo: permite manejar un servidor o PC físico (teclado, ratón y ver su pantalla) sin red, sin agente y sin software instalado en él.\n\nPiezas:\n- Dongle QKVM (LilyGO T-Dongle-S3, ESP32-S3) enchufado a un USB del SERVIDOR. El servidor lo ve como un teclado y ratón USB normales (HID) y, según el modo, como un pendrive (la microSD del dongle, típicamente con Ventoy e ISOs).\n- El PORTÁTIL (donde corre este MCP) habla con el dongle por Bluetooth LE (nombre QKVM). Alternativa: puerto serie CDC si el dongle está enchufado al portátil (solo para depurar).\n- Capturadora HDMI→USB (MS2109, 'USB Video') enchufada al portátil y a la salida HDMI del servidor: es como se VE la pantalla. qkvm.screenshot() lee de ella.\n\nPara qué: entrar en BIOS/UEFI, elegir dispositivo de arranque, instalar un SO desde una ISO de la microSD (Ventoy), usar una consola de rescate, recuperar equipos sin red o colgados, configurar servidores nuevos antes de tener red/SSH, pulsar Ctrl+Alt+Supr en una pantalla de login. Es un 'RDP de bolsillo' a nivel de hardware.\n\nLimitaciones: no hay audio; no hay alimentación remota (no puede encender/apagar el equipo salvo por teclado/SO); la pantalla solo se ve si el servidor saca vídeo por la salida HDMI conectada a la capturadora; el BLE es abierto (sin PIN)."},
{"id":"workflow","kind":"doc","title":"Cómo trabajar con el QKVM desde un LLM (bucle ver-actuar-verificar)","tags":["flujo","buenas practicas","verificar","screenshot","loop"],
"text":"1. qkvm.connection() / qkvm.status(): comprueba enlace (BLE), host=1 (el servidor ha enumerado el USB) y proto (boot = está en BIOS: sin ratón).\n2. qkvm.screenshot(): mira qué hay en pantalla ANTES de actuar.\n3. Actúa en lote dentro de un mismo execute: key/type/mouse con qkvm.sleep(0.3..2) entre pasos que abren ventanas o cambian de pantalla.\n4. Termina el execute con qkvm.screenshot() para verificar. No des por hecho que un paso funcionó.\n5. Coordenadas del ratón: en píxeles de la ÚLTIMA captura que recibiste (si pediste maxWidth 1280, la imagen mide 1280 de ancho y esas son las coordenadas). Prefiere el teclado cuando exista atajo: es más fiable que el ratón.\n6. Texto largo: qkvm.type(texto) usa el layout del servidor (defecto es). Si salen símbolos cambiados (@, /, -, ñ) el layout no coincide: qkvm.setLayout('us') y repite.\n7. Antes de algo destructivo (formatear, borrar particiones, guardar BIOS con cambios importantes, apagar) pide confirmación al usuario.\n8. Si la pantalla está negra: puede estar en reposo (pulsa 'shift' o mueve el ratón), apagada, o sin señal HDMI (la capturadora muestra negro o barras)."},
{"id":"hardware-modes","kind":"doc","title":"Modos del dongle A/B/C (USB): teclado/ratón, pendrive microSD, ambos","tags":["modo","mode","msc","sd","pendrive","ventoy","hid","boton boot"],
"text":"El dongle tiene 3 modos USB, guardados en su memoria y cambiables con el botón BOOT (pulsación corta):\n- A 'CARGO': solo almacenamiento (la microSD como pendrive USB). Los comandos de teclado/ratón fallan (NO_HID).\n- B 'BOARDING': solo teclado + ratón.\n- C 'FULL': ambos (por defecto). Sirve para arrancar el servidor desde la microSD (Ventoy) y a la vez manejar el instalador.\nqkvm.status().mode dice el modo actual. Cambiar de modo reinicia el dongle y corta el BLE; desde el MCP está bloqueado: pídeselo al usuario (botón BOOT) o que use `dkvm raw mode c` por cable."},
{"id":"coordinates","kind":"doc","title":"Coordenadas del ratón: píxeles de la captura, absoluto vs relativo","tags":["raton","mouse","coordenadas","pixeles","absoluto","relativo","click","escala"],
"text":"El dongle tiene dos ratones: ABSOLUTO (posición 0..32767 en cada eje, como una tableta: va exactamente al punto) y RELATIVO (desplazamientos -127..127, como un ratón normal; aceleración del SO incluida, impreciso).\nqkvm.mouse.move/click(x, y) usan el absoluto con x,y en PÍXELES de la última captura enviada; el MCP escala a 0..32767. Como la captura es la pantalla entera del servidor, la proporción es la misma aunque la resolución del servidor sea distinta.\nSin captura previa: qkvm.mouse.moveAbs(x, y) con 0..32767 (centro = 16384,16384).\nEn BIOS/UEFI en 'boot protocol' el ratón NO funciona (error bootproto): usa el teclado. Algunas BIOS gráficas sí aceptan el ratón absoluto; si no se mueve, prueba relativo: qkvm.mouse.moveRel(dx, dy) o qkvm.mouse.click({relative: true}).\nVarios monitores en el servidor: la captura solo ve la salida HDMI conectada; el absoluto cubre el escritorio virtual entero en Windows (puede no coincidir)."},
{"id":"layouts","kind":"ref","title":"Layouts de teclado del servidor (es, us, gb, de, fr, it, pt, latam)","tags":["layout","distribucion","teclado","idioma","simbolos","arroba","ñ","acentos"],
"text":"qkvm.type() convierte texto a pulsaciones según la distribución de teclado configurada EN EL SERVIDOR (no en el portátil). Por defecto 'es' (España). Disponibles: es, us, gb, de, fr (AZERTY), it, pt, latam.\nSíntoma de layout equivocado: '@' sale como '\"' o 'ñ' como ';', '-' como '/' o 'z' e 'y' cambiadas (de). Arreglo: qkvm.setLayout('us') (persistente en la sesión).\nEn BIOS/UEFI y en instaladores que aún no tienen idioma elegido el layout suele ser US: usa qkvm.setLayout('us') ahí y vuelve a 'es' después.\nqkvm.type(texto, {fw: true}) usa el 'type' del firmware (siempre US, solo ASCII): útil para descartar problemas del layout.\nTeclas muertas (acentos ´ ` ^ ¨) y AltGr (@ # € [ ] { } \\ | ~ en es) se generan solas."},
{"id":"keys","kind":"ref","title":"Nombres de teclas y combinaciones para qkvm.key()","tags":["teclas","key","combo","atajos","ctrl","alt","win","f1","enter","esc","nombres"],
"text":"Combos con '+': 'ctrl+alt+del', 'win+r', 'alt+f4', 'ctrl+shift+esc', 'shift+f10', 'alt+tab'. Varias en una llamada: qkvm.key('down', 'down', 'enter'). Mantener: qkvm.key('f12', {hold: 2}) o keyDown/keyUp.\nModificadores: ctrl/control/lctrl, shift/lshift, alt/lalt, win/gui/super/meta/cmd/lwin, rctrl, rshift, ralt/altgr, rwin.\nEspeciales: enter/return, esc/escape, backspace/bksp, tab, space, capslock/caps, printscreen/prtsc/sysrq, scrolllock, pause, insert/ins, home, pageup/pgup, delete/del, end, pagedown/pgdn, up, down, left, right, numlock, menu/app.\nFunción: f1..f24. Letras a-z y dígitos 0-9 (fila superior).\nCtrl+Alt+Supr: qkvm.key('ctrl+alt+del'). Máximo 6 teclas no modificadoras pulsadas a la vez (rollover).\nLinux: cambiar a TTY con 'ctrl+alt+f2'."},
{"id":"errors","kind":"ref","title":"Errores del dongle y del enlace: nohost, bootproto, busy, rollover, NO_HID, sin dongle","tags":["error","nohost","bootproto","busy","rollover","no_hid","fallo","ble","no conecta","troubleshooting"],
"text":"Los errores llegan como excepciones con prefijo:\n[sin-dongle] no se encontró el dongle: ¿está encendido (enchufado al servidor) y anunciando 'QKVM' por BLE? ¿Bluetooth del portátil activo y con permiso para la app? ¿Otra app (dkvm-gui) lo tiene? (el dongle admite un solo cliente: el último que conecta echa al anterior).\n[enlace] se cortó el BLE o timeout: el MCP reconecta en la siguiente llamada; repite.\n[dongle] nohost: el servidor no ha enumerado el USB (apagado, puerto sin datos, USB deshabilitado en BIOS). En el arranque muy temprano también pasa unos segundos.\n[dongle] bootproto: el servidor está en BIOS/boot protocol: el ratón no va; usa solo teclado.\n[dongle] busy: informe perdido (servidor suspendido que no despierta): repite; pulsa una tecla para despertar.\n[dongle] rollover: más de 6 teclas a la vez: llama a qkvm.releaseAll().\n[dongle] NO_HID: el dongle está en modo A (solo SD): hay que cambiar a modo C con el botón BOOT.\n[uso] argumento mal (tecla desconocida, layout...).\nSin vídeo: no hay capturadora conectada al portátil o no recibe señal HDMI; el teclado y ratón siguen funcionando."},
{"id":"connection","kind":"doc","title":"Conexión: BLE, serie, un solo cliente, convivencia con dkvm-gui","tags":["ble","bluetooth","serie","cdc","puerto","gui","conexion","reconectar","DKVM_PORT"],
"text":"Por defecto el MCP busca el dongle por BLE (servicio 7d0d1000-...-0001, nombre QKVM) y si no anuncia prueba el puerto serie (VID 1209:0001). La primera llamada que toca el dongle tarda ~2-8 s; luego la conexión se mantiene entre llamadas.\nEl dongle acepta UN cliente BLE: si la GUI dkvm-gui está conectada, el MCP se la quita (y al revés). qkvm.disconnect() libera el BLE para que el usuario vuelva a la GUI. La capturadora sí se puede compartir con la GUI en macOS.\nVariables de entorno del servidor MCP: DKVM_PORT (ble | serial | /dev/cu.usbmodemX | ble-mock | mock), DKVM_LAYOUT, DKVM_BLE_TIMEOUT (s), DKVM_MCP_VIDEO (capture | pattern | nombre de dispositivo), DKVM_MCP_RES (1920x1080@30), DKVM_MCP_VIDEO_IDLE (s sin capturas antes de apagar la capturadora, 120).\nSeguridad: el BLE es abierto a propósito (sin PIN); cualquiera cerca podría teclear. Úsalo en entornos de confianza."},
{"id":"bios-keys","kind":"recipe","title":"Entrar en BIOS/UEFI y menú de arranque: teclas por fabricante","tags":["bios","uefi","setup","boot menu","menu de arranque","f2","f12","f11","del","supr","f9","f10","dell","hp","hpe","lenovo","supermicro","asus","intel nuc"],
"text":"Hay que pulsar la tecla repetidamente justo al encender/reiniciar (antes del logo del SO). Patrón: reinicia (ctrl+alt+del desde el SO o el usuario pulsa el botón), y en un execute: for (let i=0;i<30;i++){ qkvm.key('f2'); qkvm.sleep(0.3); } y luego screenshot.\nTeclas (Setup / Menú de arranque):\n- Dell (PowerEdge/OptiPlex/Latitude): F2 / F12 (PowerEdge: F11 boot manager, F10 Lifecycle Controller).\n- HP/HPE ProLiant: F9 System Utilities (RBSU) / F11 boot menu; HP PCs: F10 setup, F9 boot menu, Esc menú de inicio.\n- Lenovo ThinkSystem/ThinkCentre: F1 / F12; ThinkPad: F1 o Enter y luego F1/F12.\n- Supermicro: Del / F11.\n- ASUS: Del o F2 / F8. MSI: Del / F11. Gigabyte: Del / F12. ASRock: F2 o Del / F11.\n- Intel NUC: F2 / F10. Fujitsu: F2 / F12. Acer: F2 / F12. VMware/VM no aplica.\n- Windows ya arrancado: shift + clic en Reiniciar → Solucionar problemas → Opciones avanzadas → Configuración de firmware UEFI. Linux: 'systemctl reboot --firmware-setup'.\nEn la BIOS: navega con flechas/Enter/Esc; F10 suele ser Guardar y salir (confirma con el usuario antes de guardar cambios). El ratón puede no funcionar (bootproto). Layout típico en BIOS: US."},
{"id":"boot-ventoy","kind":"recipe","title":"Arrancar e instalar un SO desde la microSD del dongle (Ventoy)","tags":["ventoy","iso","instalar","sistema operativo","arranque","usb","pendrive","sd","proxmox","windows","ubuntu","debian"],
"text":"1. El dongle debe estar en modo C (teclado+ratón+SD) y la microSD con Ventoy y las ISOs. qkvm.status().sd debe indicar SD presente.\n2. Reinicia el servidor y abre el menú de arranque (ver receta bios-keys, p. ej. F11/F12). Elige el dispositivo USB tipo 'Espressif'/'QKVM'/'USB Mass Storage' (UEFI si el SO lo soporta).\n3. En Ventoy: flechas + Enter sobre la ISO; si pregunta modo, 'Boot in normal mode'.\n4. Instalador: muchos arrancan en layout US hasta elegir idioma (qkvm.setLayout('us')). Captura tras cada pantalla.\n5. Elegir disco de destino y borrar particiones es DESTRUCTIVO: confirma con el usuario el disco exacto.\n6. La microSD por USB es lenta (lectura ~10-20 MB/s): la carga de ISOs grandes tarda minutos; espera con sleeps largos y capturas espaciadas."},
{"id":"windows-recipes","kind":"recipe","title":"Recetas Windows: login, ejecutar, consola, administrador, reinicio","tags":["windows","login","ctrl+alt+del","cmd","powershell","ejecutar","win+r","administrador","reiniciar"],
"text":"- Pantalla de bloqueo: qkvm.key('ctrl+alt+del') o 'space'; luego qkvm.type(password, {enter:true}) SOLO si el usuario te ha dado la contraseña.\n- Ejecutar: qkvm.key('win+r'); sleep(0.6); qkvm.type('cmd', {enter:true}).\n- PowerShell admin: win+x y luego 'a' (Windows 10/11), o win+r → 'powershell' → ctrl+shift+enter → alt+y en UAC (en UAC 'alt+s' en español = Sí).\n- Administrador de tareas: ctrl+shift+esc. Cerrar ventana: alt+f4. Escritorio: win+d.\n- Reinicio a opciones avanzadas: en un cmd 'shutdown /r /o /t 0'.\n- Instalador de Windows: shift+f10 abre una consola (diskpart, bypass de requisitos 'oobe\\bypassnro')."},
{"id":"linux-recipes","kind":"recipe","title":"Recetas Linux: TTY, login de consola, GRUB, rescate","tags":["linux","tty","consola","grub","rescate","single","proxmox","ubuntu","debian"],
"text":"- Cambiar a consola de texto: qkvm.key('ctrl+alt+f2') (o f3..f6); volver al gráfico: ctrl+alt+f1 o f7.\n- Login: qkvm.type('usuario', {enter:true}); sleep(0.5); qkvm.type(password, {enter:true}) solo con credenciales dadas por el usuario.\n- GRUB: pulsa 'shift' (BIOS) o 'esc' (UEFI) repetidamente al arrancar; 'e' edita la entrada; añade 'systemd.unit=rescue.target' o 'init=/bin/bash' a la línea linux; ctrl+x arranca.\n- Comandos largos: qkvm.type(cmd, {enter:true}); comprueba con captura. La consola de Linux en TTY usa el keymap del sistema (en servidores recién instalados suele ser us).\n- Proxmox: la consola muestra la URL https://IP:8006 tras el arranque; útil para saber la IP."},
{"id":"screen-reading","kind":"doc","title":"Capturas de pantalla: resolución, calidad y fiabilidad","tags":["screenshot","captura","pantalla","video","capturadora","ms2109","negro","resolucion","ocr"],
"text":"qkvm.screenshot() abre la capturadora la primera vez (~1-3 s) y la mantiene encendida hasta 120 s sin uso. Espera settleMs (250 por defecto) y devuelve el primer cuadro posterior, reescalado a maxWidth (1280 por defecto) en JPEG calidad 75.\nTexto pequeño (BIOS, consola 1080p): usa maxWidth 1920 o format 'png' para leerlo mejor. Para ahorrar contexto: maxWidth 800.\nHasta 6 capturas por execute. Si necesitas ver la evolución de una carga larga, mejor varias llamadas a execute con sleeps.\nNegro total: servidor apagado, en reposo, sin señal en esa salida HDMI, o resolución no soportada por la capturadora. Barras de colores: la capturadora no recibe señal. 'patrón de prueba' en source: no es la pantalla real (DKVM_MCP_VIDEO=pattern)."},
{"id":"api-overview","kind":"api","title":"API qkvm (resumen)","tags":["api","qkvm","funciones","referencia"],
"text":"connection(), status(), info(), screenshot({maxWidth, format, quality, settleMs}), type(text, {enter, delay, fw}), key(...combos, {hold}), keyDown(c), keyUp(c), releaseAll(), mouse.move(x,y), mouse.click(x?,y?,{button,count}), mouse.doubleClick(x,y), mouse.rightClick(x,y), mouse.down(b), mouse.up(b), mouse.scroll(n), mouse.moveAbs(x,y), mouse.moveRel(dx,dy), setLayout(name), sleep(s), raw(line), disconnect(), video.devices(), video.stop(). Globales: console.log, sleep(ms), setTimeout (síncrono). Todo es síncrono; await es opcional."},
{"id":"api-type","kind":"api","title":"qkvm.type(text, {enter, delay, fw})","tags":["api","type","escribir","texto"],
"text":"Escribe texto en el servidor usando el layout configurado (setLayout). enter:true añade Enter al final. delay: segundos entre caracteres (útil en BIOS o consolas lentas que pierden teclas: 0.02-0.05). fw:true usa el tecleo del firmware (US, solo ASCII). Saltos de línea '\\n' = Enter, '\\t' = Tab. Devuelve {typed}."},
{"id":"api-key","kind":"api","title":"qkvm.key(...combos, {hold}) / keyDown / keyUp / releaseAll","tags":["api","key","pulsar","combo","mantener"],
"text":"key('ctrl+alt+del') pulsa y suelta. key('down','down','enter') en secuencia. key('f12', {hold: 3}) mantiene 3 s (para menús que exigen mantener). keyDown('shift'); mouse.click(...); keyUp('shift') para combinaciones con ratón. releaseAll() suelta todo (también se hace solo al acabar cada execute)."},
{"id":"api-mouse","kind":"api","title":"qkvm.mouse.* (move, click, doubleClick, drag, scroll)","tags":["api","mouse","raton","click","drag","arrastrar","scroll"],
"text":"move(x,y) en píxeles de la última captura. click(x,y) mueve y hace clic izquierdo; click(x,y,{button:'right', count:2}); click('right') clic sin mover; click({relative:true}) usa el ratón relativo (BIOS que no aceptan absoluto). Arrastrar: move(x1,y1); down(); move(x2,y2); up(). scroll(-5) baja. moveAbs(16384,16384) centro sin captura previa. moveRel(dx,dy) relativo (cada paso ±127)."},
{"id":"api-screenshot","kind":"api","title":"qkvm.screenshot({maxWidth, format, quality, settleMs})","tags":["api","screenshot","captura","imagen"],
"text":"Captura la pantalla del servidor (capturadora HDMI) y la adjunta al resultado como imagen. Devuelve {width, height, sourceWidth, sourceHeight, source, image}. Las coordenadas del ratón pasan a ser las de esta imagen. Opciones: maxWidth 160..1920 (1280), format 'jpeg'|'png', quality 30..95 (75), settleMs 0..5000 (250)."},
{"id":"api-status","kind":"api","title":"qkvm.status() / connection() / info() / raw()","tags":["api","status","estado","info","raw","diagnostico","leds","capslock"],
"text":"connection(): sin tocar el dongle, dice si hay conexión, el enlace, layout, vídeo y espacio de coordenadas. status(): conecta y devuelve el estado del dongle: host (servidor enumeró el USB), hidready, susp (servidor suspendido), proto (boot = BIOS/boot protocol, report = SO), leds (bit0 NumLock, bit1 CapsLock: útil para saber si CapsLock está activo antes de escribir contraseñas), sd, mode (A/B/C), link, layout. info(): firmware. raw('stats'): orden CDC cruda de diagnóstico (bootloader/mode/reboot bloqueadas)."}
]"#;
