// Dongle KVM - modo C: microSD (MSC asincrono) + teclado + raton rel/abs (HID) + CDC serie
// Etapa 1 (correcciones de la revision fase 3). Orden de arranque: Serial -> SD init -> MSC -> HID -> USB.begin -> pantalla.
#include <Arduino.h>
#include "USB.h"
#include "soc/rtc_cntl_reg.h"
#include "esp_timer.h"
#include "tusb.h"
#include "hid_dev.h"
#include "sd_msc.h"
#include "kvm_mode.h"
#include "ble_link.h"
#include "version.h"
#include "ui_lcd.h"   // LCD/LED: dueno lcd-dev (src/ui_lcd.*), tarea propia
#include "esp_system.h"
#include "esp_core_dump.h"
#include "esp_heap_caps.h"

// ---- eventos (contexto de la tarea arduino_usb_events: solo banderas, sin E/S ni trazas) ----
static USBCDC* cdc = nullptr;
static TaskHandle_t loopTask = nullptr;
static volatile int64_t rxEventUs = 0;
static volatile bool rxEventPending = false;
static uint32_t bootSdMs = 0, bootUsbMs = 0;
// guardia contra bucles de reinicio: si los 2 ultimos arranques acabaron en panic/WDT, no se inicia BLE
static RTC_NOINIT_ATTR uint32_t crashMagic, crashCount;
static esp_reset_reason_t rstReason = ESP_RST_UNKNOWN;
static bool safeMode = false, bleStarted = false;
// registro de trazas ESP_LOG en RTC (sobrevive a un panic): comando "log" (depuracion)
static RTC_NOINIT_ATTR char logBuf[1536]; static RTC_NOINIT_ATTR uint32_t logLen, logMagic;
static int logVprintf(const char* fmt, va_list ap) {
  char t[160]; int n = vsnprintf(t, sizeof t, fmt, ap);
  if (n > (int)sizeof t - 1) n = sizeof t - 1;
  for (int i = 0; i < n && logLen < sizeof logBuf - 1; i++) logBuf[logLen++] = t[i];
  logBuf[logLen] = 0;
  return n;
}
#define BLE_START_DELAY_MS 3000     // BLE arranca desde loop() tras 3 s: el CDC queda accesible aunque BLE falle

static inline void wakeLoop() { if (loopTask) xTaskNotifyGive(loopTask); }

static void usbEvent(void*, esp_event_base_t base, int32_t id, void*) {
  if (base != ARDUINO_USB_EVENTS) return;
  if (id == ARDUINO_USB_STARTED_EVENT || id == ARDUINO_USB_STOPPED_EVENT ||
      id == ARDUINO_USB_SUSPEND_EVENT || id == ARDUINO_USB_RESUME_EVENT) {
    kvmhid::requestReleaseAll();  // el estado del host ya no es fiable: soltar todo
    wakeLoop();
  }
}
static void cdcEvent(void*, esp_event_base_t, int32_t id, void* d) {
  if (id == ARDUINO_USB_CDC_RX_EVENT) {
    if (!rxEventPending) { rxEventUs = esp_timer_get_time(); rxEventPending = true; }
    wakeLoop();
  } else if (id == ARDUINO_USB_CDC_LINE_STATE_EVENT) {
    if (!((arduino_usb_cdc_event_data_t*)d)->line_state.dtr) { kvmhid::requestReleaseAll(); wakeLoop(); }  // cliente cerro el puerto
  }
}

// ---- protocolo de texto por CDC (prueba; la base del protocolo BLE). Ver docs/PROTOCOL.md ----
static void reply(const char* s) {
  char b[360]; int n = snprintf(b, sizeof b, "%s\r\n", s);
  if (n > 0) cdc->write((const uint8_t*)b, (size_t)n < sizeof b ? n : sizeof b - 1);
}
static void replyf(const char* fmt, ...) __attribute__((format(printf, 1, 2)));
static void replyf(const char* fmt, ...) {
  char b[360]; va_list ap; va_start(ap, fmt); int n = vsnprintf(b, sizeof b - 2, fmt, ap); va_end(ap);
  if (n < 0) return;
  if ((size_t)n > sizeof b - 3) n = sizeof b - 3;
  b[n++] = '\r'; b[n++] = '\n'; cdc->write((const uint8_t*)b, n);
}

// enteros separados por espacios; devuelve cuantos o -1 si hay basura / demasiados
static int parseNums(const char* p, long* out, int maxn, int base = 10) {
  int n = 0;
  for (;;) {
    while (*p == ' ' || *p == '\t') p++;
    if (!*p) return n;
    if (n == maxn) return -1;
    char* e; long v = strtol(p, &e, base);
    if (e == p || (*e && *e != ' ' && *e != '\t')) return -1;
    out[n++] = v; p = e;
  }
}

struct Lat { uint32_t n; uint64_t sum; uint32_t max; } lat;
static void latRecord(int64_t evUs) {
  if (!evUs) return;
  int64_t d = esp_timer_get_time() - evUs; if (d < 0) d = 0;
  lat.n++; lat.sum += d; if ((uint32_t)d > lat.max) lat.max = (uint32_t)d;
}

static void cmd(char* l, size_t len, int64_t evUs) {
  char* a = (char*)memchr(l, ' ', len);
  const char* args = a ? a + 1 : l + len;
  if (a) *a = 0;
  const char* c = l;
  long v[3];
  auto num = [&](int minn, int maxn, int base = 10) { int n = parseNums(args, v, maxn, base); return (n < minn) ? -1 : n; };
  kvmhid::Res r;

  if (!strcmp(c, "type")) {
    uint32_t skipped = 0;
    r = kvmhid::kbdType(args, strlen(args), &skipped);
    if (r != kvmhid::H_OK) reply(kvmhid::resStr(r));
    else if (skipped) replyf("err nonascii skipped=%lu", (unsigned long)skipped);
    else reply("ok");
  } else if (!strcmp(c, "kd") || !strcmp(c, "ku")) {
    if (num(1, 1, 0) != 1) { reply("err args"); return; }
    if (!((v[0] >= 1 && v[0] <= 0xA4) || (v[0] >= 0xE0 && v[0] <= 0xE7))) { reply("err range"); return; }
    r = c[1] == 'd' ? kvmhid::kbdPressRaw((uint8_t)v[0]) : kvmhid::kbdReleaseRaw((uint8_t)v[0]);
    reply(kvmhid::resStr(r)); latRecord(evUs);
  } else if (!strcmp(c, "kall")) {
    kvmhid::releaseAllNow(); reply("ok"); latRecord(evUs);
  } else if (!strcmp(c, "mm")) {
    int n = num(2, 3); if (n < 0) { reply("err args"); return; }
    r = kvmhid::mouseRel((int8_t)constrain(v[0], -127L, 127L), (int8_t)constrain(v[1], -127L, 127L), n > 2 ? (int8_t)constrain(v[2], -127L, 127L) : 0);
    reply(kvmhid::resStr(r)); latRecord(evUs);
  } else if (!strcmp(c, "ma")) {
    int n = num(2, 3); if (n < 0) { reply("err args"); return; }
    r = kvmhid::mouseAbs((int16_t)constrain(v[0], 0L, 32767L), (int16_t)constrain(v[1], 0L, 32767L), n > 2 ? (int8_t)constrain(v[2], -127L, 127L) : 0);
    reply(kvmhid::resStr(r)); latRecord(evUs);
  } else if (!strcmp(c, "mbr") || !strcmp(c, "mba")) {
    if (num(1, 1) != 1) { reply("err args"); return; }
    if (v[0] < 0 || v[0] > 0x1F) { reply("err range"); return; }
    r = c[2] == 'r' ? kvmhid::mouseRelButtons((uint8_t)v[0]) : kvmhid::mouseAbsButtons((uint8_t)v[0]);
    reply(kvmhid::resStr(r)); latRecord(evUs);
  } else if (!strcmp(c, "mode")) {
    char m = *args ? (*args | 0x20) : 0;
    if (!m) { replyf("mode=%c", kvmmode::letter()); return; }
    if (m < 'a' || m > 'c' || args[1]) { reply("err range"); return; }
    reply("ok"); delay(20); kvmmode::request(m - 'a');   // reinicia y re-enumera con otro PID
  } else if (!strcmp(c, "log")) {
    for (uint32_t i = 0; i < logLen; i += 200) { char t[205]; uint32_t k = logLen - i > 200 ? 200 : logLen - i; memcpy(t, logBuf + i, k); t[k] = 0; for (char* q = t; *q; q++) if (*q < 32) *q = 0x7c; reply(t); }
    if (!logLen) reply("(vacio)");
  } else if (!strcmp(c, "heap")) {
    replyf("free=%lu min=%lu maxblock=%lu dma_free=%lu dma_maxblock=%lu", (unsigned long)heap_caps_get_free_size(MALLOC_CAP_INTERNAL), (unsigned long)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL),
           (unsigned long)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL), (unsigned long)heap_caps_get_free_size(MALLOC_CAP_DMA), (unsigned long)heap_caps_get_largest_free_block(MALLOC_CAP_DMA));
  } else if (!strcmp(c, "coredump")) {   // resumen del ultimo volcado (panic/WDT) guardado en flash; decodificar con addr2line
    if (esp_core_dump_image_check() != ESP_OK) { reply("none"); return; }
    esp_core_dump_summary_t* sm = (esp_core_dump_summary_t*)malloc(sizeof *sm);
    if (!sm) { reply("err mem"); return; }
    if (esp_core_dump_get_summary(sm) == ESP_OK) {
      replyf("task=%s pc=0x%08lx cause=%lu depth=%lu", sm->exc_task, (unsigned long)sm->exc_pc, (unsigned long)sm->ex_info.exc_cause, (unsigned long)sm->exc_bt_info.depth);
      char b[300]; int n = 0;
      for (uint32_t i = 0; i < sm->exc_bt_info.depth && i < 16 && n < 260; i++) n += snprintf(b + n, sizeof b - n, "0x%08lx ", (unsigned long)sm->exc_bt_info.bt[i]);
      reply(b);
    } else reply("err summary");
    free(sm);
  } else if (!strcmp(c, "ble")) {   // depuracion: "ble start" arranca BLE aunque safe=1
    if (!strcmp(args, "kick")) { blelink::kick(); reply("ok"); }
    else if (!strcmp(args, "params")) { char b[300]; blelink::paramDiag(b, sizeof b); reply(b); }
    else if (!strncmp(args, "slowadv ", 8)) { blelink::setSlowAdv(args[8] == '1'); reply("ok"); }
    else if (!strncmp(args, "want ", 5)) { long u = 0, mx = 0; sscanf(args + 5, "%ld %ld", &u, &mx); if (!mx) mx = u; if (u >= 6 && mx >= u && mx <= 40) { blelink::setWantItvl((uint16_t)u, (uint16_t)mx); reply("ok"); } else reply("err range"); }
    else if (!strcmp(args, "start") && !bleStarted) { reply("ok"); bleStarted = true; blelink::begin(loopTask); } else reply("err args");
  } else if (!strcmp(c, "reboot")) {
    kvmhid::releaseAllNow(); reply("ok"); delay(100); crashMagic = 0; esp_restart();
  } else if (!strcmp(c, "bootloader")) {
    kvmhid::releaseAllNow(); reply("ok"); delay(100);
    REG_WRITE(RTC_CNTL_OPTION1_REG, RTC_CNTL_FORCE_DOWNLOAD_BOOT); esp_restart();
  } else if (!strcmp(c, "status")) {
    replyf("host=%d leds=0x%02X hidready=%d susp=%d proto=%s sd=%d mode=%c ble=%d mtu=%u ble_itvl_ms=%.2f", tud_mounted() ? 1 : 0, kvmhid::leds(),
           tud_hid_n_ready(0) ? 1 : 0, tud_suspended() ? 1 : 0, kvmhid::bootProtocol() ? "boot" : "report", sdmsc::ready() ? 1 : 0,
           kvmmode::letter(), blelink::connected() ? 1 : 0, blelink::mtu(), blelink::itvlUnits() * 1.25);
  } else if (!strcmp(c, "info")) {
    const uint8_t* cd = tud_descriptor_configuration_cb(0);
    char bd[80]; blelink::diag(bd, sizeof bd);
    replyf("fw=" RIOTKVM_FW_VERSION " name=RiotKVM proto=" RIOTKVM_STR(RIOTKVM_BLE_PROTO) " mode=%c rst=%d crashes=%lu safe=%d ble_started=%d [%s] build=\"%s %s\" sd_mb=%lu boot_sd_ms=%lu boot_usb_ms=%lu sd_init_ms=%lu ui_t0=%lu ui_splash_ms=%lu ui_frame_us_max=%lu cfg_len=%u n_itf=%u",
           kvmmode::letter(), (int)rstReason, (unsigned long)crashCount, safeMode ? 1 : 0, bleStarted ? 1 : 0, bd, __DATE__, __TIME__,
           (unsigned long)(sdmsc::sectors() / 2048), (unsigned long)bootSdMs, (unsigned long)bootUsbMs, (unsigned long)sdmsc::initMs(),
           (unsigned long)ui::firstFrameMs(), (unsigned long)ui::splashMs(), (unsigned long)ui::frameUsMax(),
           cd ? cd[2] | (cd[3] << 8) : 0, cd ? cd[4] : 0);
  } else if (!strcmp(c, "ui")) {   // coste de la UI (lcd-dev): `ui` o `ui reset`
    if (*args == 'r') { ui::resetStats(); reply("ok"); return; }
    char b[300]; ui::report(b, sizeof b); reply(b);
  } else if (!strcmp(c, "cpu")) {  // CPU % por tarea desde la llamada anterior (la primera: media desde el arranque)
    char b[340]; ui::cpu(b, sizeof b); reply(b);
  } else if (!strcmp(c, "rep")) {   // ultimos informes HID enviados por ID (p. ej. id3 abs: byte0 = botones, luego X,Y LE, rueda, pan)
    char b[200]; kvmhid::repDiag(b, sizeof b); reply(b);
  } else if (!strcmp(c, "lat")) {   // latencia interna BLE->HID en us: buckets <250,<500,<1000,<2000,<4000,<8000,<16000,>=16000
    const kvmhid::Hist* hs[2] = {&kvmhid::histQueue(), &kvmhid::histDeliver()};
    char b[340]; int n = 0;
    for (int k = 0; k < 2; k++) {
      const kvmhid::Hist& h = *hs[k];
      n += snprintf(b + n, sizeof b - n, "%s n=%lu avg=%lu max=%lu h=%lu,%lu,%lu,%lu,%lu,%lu,%lu,%lu ", k ? "deliver" : "queue", (unsigned long)h.n, (unsigned long)(h.n ? h.sum / h.n : 0), (unsigned long)h.max,
                    (unsigned long)h.b[0], (unsigned long)h.b[1], (unsigned long)h.b[2], (unsigned long)h.b[3], (unsigned long)h.b[4], (unsigned long)h.b[5], (unsigned long)h.b[6], (unsigned long)h.b[7]);
    }
    reply(b);
  } else if (!strcmp(c, "stats")) {
    if (*args == 'r') { kvmhid::resetStats(); sdmsc::resetStats(); blelink::resetStats(); lat = Lat{}; reply("ok"); return; }
    const kvmhid::Stats& h = kvmhid::stats(); const sdmsc::Stats& s = sdmsc::stats();
    replyf("cmd_n=%lu cmd_avg_us=%lu cmd_max_us=%lu snd_n=%lu snd_fail=%lu snd_avg_us=%lu snd_max_us=%lu bootdrop=%lu ble_w=%lu ble_rec=%lu ble_drop=%lu ble_bad=%lu ble_conn=%lu ble_disp=%lu",
           (unsigned long)lat.n, (unsigned long)(lat.n ? lat.sum / lat.n : 0), (unsigned long)lat.max,
           (unsigned long)h.sent, (unsigned long)h.failed, (unsigned long)(h.sent ? h.sendUsSum / h.sent : 0),
           (unsigned long)h.sendUsMax, (unsigned long)h.bootDropped,
           (unsigned long)blelink::stats().writes, (unsigned long)blelink::stats().records, (unsigned long)blelink::stats().dropped,
           (unsigned long)(blelink::stats().malformed + blelink::stats().unknown), (unsigned long)blelink::stats().connects, (unsigned long)blelink::stats().displaced);
    replyf("sd_rd=%lu sd_wr=%lu sd_kb=%lu sd_err=%lu sd_retry=%lu sd_reinit=%lu sd_op_max_us=%lu sd_cb_max_us=%lu",
           (unsigned long)s.reads, (unsigned long)s.writes, (unsigned long)(s.bytes / 1024), (unsigned long)s.errors,
           (unsigned long)s.retries, (unsigned long)s.reinits, (unsigned long)s.opUsMax, (unsigned long)s.cbUsMax);
  } else reply("err unknown");
}

// ---- entrada CDC: linea acumulada en buffer fijo (sin String) ----
static char line[256]; static size_t lineLen = 0; static bool lineOver = false;

static void pump() {
  uint8_t chunk[64]; int n;
  while ((n = cdc->available()) > 0) {
    n = cdc->read(chunk, n > (int)sizeof chunk ? sizeof chunk : n);
    for (int i = 0; i < n; i++) {
      char ch = chunk[i];
      if (ch == '\n' || ch == '\r') {
        if (lineOver) reply("err overflow");
        else if (lineLen) {
          int64_t ev = rxEventPending ? rxEventUs : 0;
          line[lineLen] = 0;
          bool hidCmd = ev && (line[0] == 'm' || line[0] == 'k');   // latencia interna rx->entrega tambien por CDC
          if (hidCmd) { kvmhid::setRx(ev); kvmhid::markExec(); }
          cmd(line, lineLen, ev);
          if (hidCmd) kvmhid::setRx(0);
        }
        lineLen = 0; lineOver = false;
      } else if (lineLen < sizeof line - 1) line[lineLen++] = ch;
      else lineOver = true;
    }
    rxEventPending = false;
  }
}

void setup() {
  loopTask = xTaskGetCurrentTaskHandle();
  rstReason = esp_reset_reason();
  if (crashMagic != 0xC0FFEE01) { crashMagic = 0xC0FFEE01; crashCount = 0; }
  if (rstReason == ESP_RST_PANIC || rstReason == ESP_RST_INT_WDT || rstReason == ESP_RST_TASK_WDT || rstReason == ESP_RST_WDT) crashCount++;
  else if (rstReason == ESP_RST_POWERON) crashCount = 0;
  safeMode = crashCount >= 2;
  if (logMagic != 0x4C4F4731 || rstReason == ESP_RST_POWERON || rstReason == ESP_RST_SW) { logMagic = 0x4C4F4731; logLen = 0; }
  esp_log_set_vprintf(logVprintf);
  // OJO: con ARDUINO_USB_CDC_ON_BOOT=1 el core llama a USB.begin() desde app_main() ANTES de setup()
  // (USB.h define ARDUINO_USB_ON_BOOT), con solo la interfaz CDC. Por eso CDC_ON_BOOT=0 en platformio.ini
  // y el CDC se crea aqui; asi el orden lo controlamos nosotros.
  cdc = new USBCDC(0);
  cdc->enableReboot(false);          // cerrar el puerto serie no debe mandar el dongle a modo descarga
  cdc->setRxBufferSize(2048);        // antes de begin(): evita desbordes y el xQueueSend de 10 ms desde usbd
  cdc->setTxTimeoutMs(15);           // acotado: respuestas largas (stats/info) caben; si el host no drena, se corta a los 15 ms
  cdc->begin();
  cdc->onEvent(ARDUINO_USB_CDC_RX_EVENT, cdcEvent);          // dormir en notificacion en vez de sondear con delay(1)
  cdc->onEvent(ARDUINO_USB_CDC_LINE_STATE_EVENT, cdcEvent);
  USB.onEvent(usbEvent);
  kvmmode::load();
  USB.PID(kvmmode::pid());           // un PID por modo: Windows no mezcla drivers
  USB.usbAttributes(TUSB_DESC_CONFIG_ATT_SELF_POWERED | TUSB_DESC_CONFIG_ATT_REMOTE_WAKEUP);

#ifdef BLE_EARLY
  if (!safeMode) { bleStarted = true; blelink::begin(loopTask); delay(1500); }   // experimento: BLE antes que USB/SD
#endif
  if (kvmmode::sd()) sdmsc::begin(); // MSC + tarea SD (primer init sincrono)
  bootSdMs = millis();
  if (kvmmode::hid()) kvmhid::begin();
  USB.begin();                       // lo antes posible: la pantalla va despues
  bootUsbMs = millis();
  // prioridad de loop (ruta caliente HID) por encima de UI (0). La afinidad de usbd NO se puede fijar a posteriori:
  // este FreeRTOS no es SMP (vTaskCoreAffinitySet no existe) y el core crea usbd con xTaskCreate sin nucleo.
  vTaskPrioritySet(loopTask, 10);

  pinMode(0, INPUT_PULLUP);
  ui::begin();                       // splash + estado en su tarea (prioridad 0, nucleo 1); no bloquea
}

// boton BOOT (GPIO0): pulsacion corta (30 ms..1 s, al soltar) cicla A->B->C
static void button() {
  static uint32_t downAt = 0;
  bool dn = digitalRead(0) == LOW;
  uint32_t now = millis();
  if (dn && !downAt) downAt = now ? now : 1;
  else if (!dn && downAt) {
    uint32_t d = now - downAt; downAt = 0;
    if (d >= 30 && d < 1000) { ui::course("BCA"[kvmmode::current()]); kvmmode::requestNext(); }
  }
}

void loop() {
  // dormir hasta evento/dato CDC/BLE; con informes pendientes, reintentar cada 2 ms; si no, 20 ms (boton, vigilancia)
  ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(kvmhid::pending() ? 2 : 20));
  if (kvmhid::releaseRequested()) kvmhid::releaseAllNow();
  blelink::service();
  kvmhid::service();
  pump();
  button();
  kvmmode::service();
  sdmsc::consumeChange();
  if (!bleStarted && !safeMode && millis() >= BLE_START_DELAY_MS) { bleStarted = true; blelink::begin(loopTask); }
  if (millis() > 10000 && crashCount) crashCount = 0;   // arranque estable
}
