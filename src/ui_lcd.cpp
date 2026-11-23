#include "ui_lcd.h"
#include "ui_assets.h"
#define LGFX_USE_V1
#include <LovyanGFX.hpp>
#include "tusb.h"
#include "esp_random.h"
#include "esp_timer.h"
#include "kvm_mode.h"
#include "hid_dev.h"
#include "sd_msc.h"
#include "ble_link.h"
#include "led_cfg.h"
#include "dev_name.h"

namespace ui {
namespace {

constexpr int PIN_LED_DI = 40, PIN_LED_CI = 39, PIN_TFT_BL = 38, PIN_BTN = 0;
constexpr int SW = 160, SH = 80;

class LGFX : public lgfx::LGFX_Device {
  lgfx::Panel_ST7735S _panel; lgfx::Bus_SPI _bus;
public:
  LGFX() {
    auto b = _bus.config();
    b.spi_host = SPI2_HOST; b.spi_mode = 0; b.freq_write = 27000000;
    b.pin_sclk = 5; b.pin_mosi = 3; b.pin_miso = -1; b.pin_dc = 2;
    b.dma_channel = SPI_DMA_CH_AUTO;
    _bus.config(b); _panel.setBus(&_bus);
    auto p = _panel.config();
    p.pin_cs = 4; p.pin_rst = 1; p.pin_busy = -1;
    p.panel_width = 80; p.panel_height = 160; p.offset_x = 26; p.offset_y = 1;
    p.invert = true; p.rgb_order = false;
    _panel.config(p); setPanel(&_panel);
  }
};

LGFX lcd;
lgfx::LGFX_Sprite spr(&lcd);        // sprite de BANDA 160x20 (6,4 KB, RAM interna): todo se compone y se envia por bandas con DMA
constexpr int BH = 20;
TaskHandle_t task = nullptr;
volatile uint32_t wakeAt = 0, tFirst = 0, tSplash = 0, usMax = 0, tInitMs = 0, statsT0 = 0;
volatile char courseTo = 0;
Stats st{};                          // contadores de coste: solo los escribe la tarea UI

// ---- paleta (RGB565) ----
uint16_t C_GREEN, C_DGREEN, C_AMBER, C_MAG, C_CYAN, C_RED, C_BONE, C_GREY, C_WHITE = 0xFFFF;

const char* modeName(uint8_t m) { return m == 0 ? "CARGO" : m == 1 ? "BOARDING" : "FULL"; }
uint16_t accent(uint8_t m) { return m == 0 ? C_AMBER : m == 1 ? C_CYAN : C_GREEN; }
const uint8_t* iconOf(uint8_t m) { return m == 0 ? uiasset::iconA : m == 1 ? uiasset::iconB : uiasset::iconC; }
const char* subOf(uint8_t m) { return m == 0 ? "SD+USB" : m == 1 ? "KBD+MOUSE" : "ALL"; }

// ---- LED APA102: color fijo por estado (modo / BLE / fallo SD). Solo se escribe al cambiar; nada de respiracion ----
void apa(uint8_t r, uint8_t gr, uint8_t b) {
  static uint32_t last = 0xFFFFFFFF;
  uint32_t k = (uint32_t)r << 16 | (uint32_t)gr << 8 | b;
  if (k == last) return;
  last = k; st.ledWrites++;
  auto sh = [](uint8_t v) { for (int i = 7; i >= 0; --i) { digitalWrite(PIN_LED_DI, (v >> i) & 1); digitalWrite(PIN_LED_CI, 1); digitalWrite(PIN_LED_CI, 0); } };
  for (int i = 0; i < 4; i++) sh(0x00);
  sh(0xE0 | 4); sh(b); sh(gr); sh(r);
  for (int i = 0; i < 4; i++) sh(0xFF);
}
// Ajuste del usuario (ledcfg): AUTO = color por estado (lo de siempre, al 90/255 ~ 35 %); FIXED = su color y brillo
// (0..100 % del canal, mismo brillo global APA102; 35 % ~ brillo de AUTO); OFF = apagado.
// Decision: en FIXED el fallo de SD NO pinta rojo. Es una eleccion explicita del usuario y el fallo ya se ve en la
// pantalla (fila SD) y en `status`/STATE; quien quiera el aviso por color usa AUTO.
void ledUpdate(bool ble, bool sdFail) {
  ledcfg::Cfg c = ledcfg::get();
  if (c.mode == ledcfg::OFF) { apa(0, 0, 0); return; }
  if (c.mode == ledcfg::FIXED) { apa(c.r * c.bright / 100, c.g * c.bright / 100, c.b * c.bright / 100); return; }
  uint8_t r, gr, b;
  if (sdFail) { r = 255; gr = 0; b = 0; }
  else if (ble) { r = 255; gr = 0; b = 200; }
  else switch (kvmmode::current()) { case 0: r = 255; gr = 110; b = 0; break; case 1: r = 0; gr = 190; b = 220; break; default: r = 0; gr = 255; b = 60; }
  apa(r * 90 / 255, gr * 90 / 255, b * 90 / 255);
}

// ---- backlight activo en BAJO, PWM ----
void backlight(uint8_t pct) { ledcWrite(PIN_TFT_BL, 255 - (uint32_t)pct * 255 / 100); }

// ---- dibujo por bandas: coordenadas de PANTALLA; la banda actual empieza en `by` y tiene `bh` filas ----
int by = 0, bh = BH;
inline bool vis(int y, int h) { return y < by + bh && y + h > by; }
void FR(int x, int y, int w, int h, uint16_t c) { if (vis(y, h)) spr.fillRect(x, y - by, w, h, c); }
void TX(int x, int y, const char* s, uint16_t c, int sz = 1) { if (!vis(y, 8 * sz)) return; spr.setTextSize(sz); spr.setTextColor(c); spr.setCursor(x, y - by); spr.print(s); }
void BM(int x, int y, const uint8_t* b, int w, int h, uint16_t c) { if (vis(y, h)) spr.drawBitmap(x, y - by, b, w, h, c); }
uint32_t hsh(uint32_t a, uint32_t b) { uint32_t h = a * 2654435761u ^ (b + 0x9E3779B9u) * 40503u; h ^= h >> 13; h *= 0x5bd1e995u; return h ^ (h >> 15); }

// calavera 64x64 en (ox,oy): bones oscuro debajo, craneo encima; glitch = desplazamiento aleatorio de lineas
void skullRows(int ox, int oy, int rows, int glitch, uint32_t fr) {
  for (int r = 0; r < rows; r++) {
    if (!vis(oy + r, 1)) continue;
    int off = (glitch && hsh(fr, r) % 100 < 35) ? (int)(hsh(r, fr) % (2 * glitch + 1)) - glitch : 0;
    spr.drawBitmap(ox + off, oy + r - by, uiasset::bones + r * 8, 64, 1, C_DGREEN);
    spr.drawBitmap(ox + off, oy + r - by, uiasset::skull + r * 8, 64, 1, C_BONE);
  }
}
const uint8_t BAYER[4][4] = { {0,8,2,10}, {12,4,14,6}, {3,11,1,9}, {15,7,13,5} };
void skullDither(int ox, int oy, int lvl) {   // aparece por tramado ordenado (lvl 0..16)
  for (int r = 0; r < 64; r++) {
    if (!vis(oy + r, 1)) continue;
    for (int pass = 0; pass < 2; pass++) {
      const uint8_t* a = pass ? uiasset::skull : uiasset::bones; uint16_t c = pass ? C_BONE : C_DGREEN;
      for (int x = 0; x < 64; x++)
        if (((pgm_read_byte(&a[r * 8 + x / 8]) >> (7 - x % 8)) & 1) && BAYER[(oy + r) & 3][(ox + x) & 3] < lvl) spr.drawPixel(ox + x, oy + r - by, c);
    }
  }
}
void eyes(int ox, int oy, uint16_t col, bool closed) {
  for (int xx : { uiasset::EYE_L_X, uiasset::EYE_R_X }) {
    if (closed) FR(ox + xx - 6, oy + uiasset::EYE_Y, 13, 2, col);
    else if (vis(oy + uiasset::EYE_Y - 6, 13)) spr.fillCircle(ox + xx, oy + uiasset::EYE_Y - by, uiasset::EYE_R - 1, col);
  }
}
int typed(const char* s, int x, int y, uint32_t t, uint32_t t0, uint32_t per, uint16_t col, int sz = 1) {
  int len = strlen(s), n = t < t0 ? 0 : (int)((t - t0) / per); if (n > len) n = len;
  if (n) { char b[24]; memcpy(b, s, n); b[n] = 0; TX(x, y, b, col, sz); }
  return n;
}
// firma "made by quim-d" en GRIS, fuente pequena (TomThumb: 4 px por caracter, 7 filas con descendentes), arrimada a la
// derecha en la fila y; aparece a los 600 ms de animacion
void madeBy(int y, uint32_t t) {
  if (t < 600 || !vis(y, 7)) return;
  static const char s[] = "made by quim-d";
  spr.setFont(&fonts::TomThumb); spr.setTextColor(C_GREY); spr.setTextSize(1); spr.setTextDatum(top_left);
  spr.drawString(s, SW - 2 - 4 * (int)(sizeof s - 1), y - by);
  spr.setFont(&fonts::Font0);
}
// lineas de arranque segun el modo (solo lo que existe en este modo)
int bootLines(const char** o) {
  int n = 0; o[n++] = "[ OK ] usb";
  if (kvmmode::hid()) o[n++] = "[ OK ] hid";
  if (kvmmode::sd()) o[n++] = "[ OK ] sd";
  if (kvmmode::hid()) o[n++] = "[ OK ] ble";
  return n;
}

// ---- splash (UI_SPLASH_VARIANT: 1 glitch, 2 BIOS, 3 apertura). ~1,2 s, una sola vez ----
void splashV1(uint32_t t) {
  uint32_t fr = t / 40;
  int rows = t >= 380 ? 64 : (int)(t * 64 / 380), gl = t < 520 ? (int)((520 - t) / 70) + 1 : 0;
  skullRows(4, 8, rows, gl, fr);
  if (t < 380) FR(0, 8 + rows, SW, 1, C_GREEN);
  if (t >= 380) eyes(4, 8, t < 700 ? C_GREEN : C_MAG, (t >= 760 && t < 820) || (t >= 980 && t < 1040));
  if (t >= 300 && t < 800 && hsh(fr, 7) % 100 < 30) { typed("RiotKVM", 75, 12, t, 300, 45, C_MAG, 2); typed("RiotKVM", 77, 12, t, 300, 45, C_CYAN, 2); }
  typed("RiotKVM", 76, 12, t, 300, 45, C_GREEN, 2);   // 7 letras a tamano 2: 84 px, justo hasta el borde
  const char* ln[4]; int n = bootLines(ln); if (n > 3) n = 3;
  for (int i = 0; i < n; i++) typed(ln[i], 76, 36 + 10 * i, t, 640 + 140 * i, 20, C_GREEN);
  char m[12]; snprintf(m, sizeof m, "mode %c", kvmmode::letter()); typed(m, 76, 68, t, 1050, 30, C_AMBER);
  madeBy(1, t);                                         // arriba a la derecha: abajo esta "mode C"
}
void splashV2(uint32_t t) {
  const char* ln[4]; int n = bootLines(ln);
  typed("> boot", 3, 4, t, 0, 14, C_GREEN);
  for (int i = 0; i < n; i++) typed(ln[i], 3, 15 + 11 * i, t, 150 * (i + 1), 14, C_GREEN);
  if (t >= 750 && t < 1000 && ((t / 120) & 1) == 0) FR(3, 15 + 11 * n + 2, 6, 8, C_GREEN);
  int lvl = t < 450 ? 0 : (int)((t - 450) / 25); if (lvl > 16) lvl = 16;
  if (lvl) skullDither(90, 8, lvl);
  if (t >= 850) eyes(90, 8, t < 1050 ? C_CYAN : C_MAG, t >= 1000 && t < 1060);
  typed("RiotKVM", 3, 62, t, 900, 25, C_AMBER, 2);
  madeBy(73, t);                                        // abajo a la derecha: bajo la calavera (filas 8..71), a la derecha del titulo (x <= 87)
}
void splashV3(uint32_t t) {
  uint32_t fr = t / 40;
  float slide = t < 650 ? 0 : t >= 900 ? 1 : (t - 650) / 250.0f; int ox = (int)(48 - 44 * slide);
  int half = t >= 320 ? 32 : (int)(t * 32 / 320);
  skullRows(ox, 8, 64, 0, 0);
  if (half < 32) { FR(0, 0, ox + 32 - half, SH, TFT_BLACK); FR(ox + 32 + half, 0, SW, SH, TFT_BLACK); }
  if (t >= 320) eyes(ox, 8, t < 400 ? C_WHITE : t < 600 ? C_GREEN : t < 800 ? C_CYAN : C_MAG, t >= 880 && t < 940);
  typed("RiotKVM", 76, 18, t, 860, 20, C_GREEN, 2); typed("ready_", 76, 44, t, 1000, 30, C_AMBER);
  if (t >= 700 && t < 900 && hsh(fr, 3) % 100 < 40) FR(76, 14 + (int)(hsh(fr, 5) % 24), 74, 1, C_DGREEN);
  madeBy(73, t);
}

// ---- pantalla de modo: cabecera (acento) + icono 1 bit + filas segun modo. Estatica; solo se recompone la banda que cambia ----
enum { R_BLE, R_SD, R_USB, R_HID };
struct Layout { uint8_t n, type[4], top[4], h; };
const Layout LAYOUT[3] = { { 2, { R_SD, R_USB }, { 22, 40 }, 16 },
                           { 3, { R_BLE, R_USB, R_HID }, { 14, 28, 42 }, 14 },
                           { 4, { R_BLE, R_SD, R_USB, R_HID }, { 13, 26, 39, 52 }, 13 } };
struct Snap { bool ble, hidSeen, sdFail, sdReady; uint32_t sdSectors; uint8_t usb; } sn{};   // estado a dibujar
uint8_t shownMode = 0;

void rowText(int y, uint8_t type) {
  const char *tg = "", *msg = ""; uint16_t tc = C_GREEN, mc = C_CYAN; char buf[12];
  const char* tag = type == R_BLE ? "ble" : type == R_SD ? "sd" : type == R_USB ? "usb" : "hid";
  switch (type) {
    case R_BLE: if (sn.ble) { tg = "[ OK ]"; msg = "linked"; mc = C_MAG; } else { tg = "[ .. ]"; tc = C_AMBER; msg = "scan"; mc = C_AMBER; } break;
    case R_SD:
      if (sn.sdReady) { tg = "[ OK ]"; snprintf(buf, sizeof buf, "%.1fG", sn.sdSectors / 2.0 / 1024 / 1024); msg = buf; }
      else { tg = "[FAIL]"; tc = C_RED; msg = "no card"; mc = C_RED; } break;
    case R_USB:
      if (sn.usb == 2) { tg = "[ zz ]"; tc = C_AMBER; msg = "sleep"; mc = C_AMBER; }
      else if (sn.usb == 1) { tg = "[ OK ]"; msg = "host"; } else { tg = "[ .. ]"; tc = C_AMBER; msg = "wait"; mc = C_AMBER; } break;
    default: tg = sn.hidSeen ? "[used]" : "[idle]"; tc = sn.hidSeen ? C_MAG : C_GREEN;
  }
  TX(52, y, tag, C_DGREEN); TX(76, y, tg, tc); TX(118, y, msg, mc);
}
void composeStatus() {
  uint8_t m = shownMode; uint16_t ac = accent(m); const Layout& L = LAYOUT[m];
  spr.fillScreen(TFT_BLACK);
  FR(0, 0, SW, 11, ac);
  char h[16]; snprintf(h, sizeof h, "%c %s", "ABC"[m], modeName(m)); TX(2, 2, h, TFT_BLACK);
  // nombre del dongle (dev_name) a la derecha; si no cabe tras el modo (1 caracter de hueco), se recorta con '~' al final
  char nm[devname::MAX_LEN + 1]; devname::get(nm, sizeof nm);
  int fit = (SW - 2 - 2 - 6 * ((int)strlen(h) + 1)) / 6, nl = strlen(nm);
  if (nl > fit) { nl = fit; nm[nl - 1] = '~'; nm[nl] = 0; }
  TX(SW - 2 - 6 * nl, 2, nm, TFT_BLACK);
  BM(4, 20, iconOf(m), uiasset::ICON_W, uiasset::ICON_H, ac);
  TX(4, 64, subOf(m), ac);
  for (int i = 0; i < L.n; i++) rowText(L.top[i] + 2, L.type[i]);
  FR(0, SH - 1, SW, 1, ac);
}
void composeCourse() {
  uint8_t to = courseTo - 'A'; uint16_t ac = accent(to);
  spr.fillScreen(TFT_BLACK);
  FR(0, 0, SW, 1, C_AMBER); FR(0, SH - 1, SW, 1, C_AMBER); FR(0, 0, 1, SH, C_AMBER); FR(SW - 1, 0, 1, SH, C_AMBER);
  TX(10, 8, "! CAMBIANDO RUMBO...", C_AMBER);
  char b[3] = { courseTo, 0, 0 }; TX(10, 26, b, ac, 2); TX(34, 30, "->", C_GREY); TX(52, 30, modeName(to), ac);
  TX(10, 54, "re-enumerando USB", C_GREY);
}
enum { SC_SPLASH, SC_STATUS, SC_COURSE };
uint32_t splashT = 0;
void compose(int sc) {
  if (sc == SC_STATUS) composeStatus();
  else if (sc == SC_COURSE) composeCourse();
  else {
    spr.fillScreen(TFT_BLACK);
#if UI_SPLASH_VARIANT == 1
    splashV1(splashT);
#elif UI_SPLASH_VARIANT == 3
    splashV3(splashT);
#else
    splashV2(splashT);
#endif
  }
}

// envia por DMA la banda ya compuesta en el sprite; mide el tiempo SPI
void pushBand(int y, int h) {
  int64_t a = esp_timer_get_time();
  lcd.startWrite();
  lcd.pushImageDMA(0, y, SW, h, (const lgfx::swap565_t*)spr.getBuffer());
  lcd.endWrite();                                      // espera el fin del DMA
  uint32_t d = (uint32_t)(esp_timer_get_time() - a);
  st.pushUsSum += d; if (d > st.pushUsMax) st.pushUsMax = d;
  st.bytes += (uint32_t)SW * h * 2; st.bands++;
}
void drawRegion(int sc, int y, int h) {                // compone y envia [y, y+h) (h <= BH)
  by = y; bh = h; compose(sc); pushBand(y, h);
}
void drawFull(int sc) { for (int y = 0; y < SH; y += BH) drawRegion(sc, y, SH - y < BH ? SH - y : BH); }
void noteFrame(int64_t a) {
  uint32_t d = (uint32_t)(esp_timer_get_time() - a); if (d > usMax) usMax = d;
  st.frames++; st.frameUsSum += d; if (d > st.frameUsMax) st.frameUsMax = d;
}

uint32_t rowKey(uint8_t type) {
  switch (type) {
    case R_BLE: return sn.ble;
    case R_SD: return sn.sdReady ? 1 + (sn.sdSectors >> 11) : 0;
    case R_USB: return sn.usb;
    default: return sn.hidSeen;
  }
}

void uiTask(void*) {
  uint32_t tb = millis();
  pinMode(PIN_LED_DI, OUTPUT); pinMode(PIN_LED_CI, OUTPUT);
  pinMode(PIN_BTN, INPUT_PULLUP);
  ledcAttach(PIN_TFT_BL, 5000, 8); backlight(0);                    // apagado hasta tener el primer fotograma
  lcd.init(); lcd.setRotation(1);
  spr.setColorDepth(16);
  if (!spr.createSprite(SW, BH)) { vTaskDelete(nullptr); return; }  // sin RAM: sin UI (backlight apagado)
  C_GREEN = lcd.color565(0, 255, 70); C_DGREEN = lcd.color565(0, 90, 30); C_AMBER = lcd.color565(255, 176, 0);
  C_MAG = lcd.color565(255, 0, 200); C_CYAN = lcd.color565(0, 220, 255); C_RED = lcd.color565(255, 40, 40);
  C_BONE = lcd.color565(210, 255, 215); C_GREY = lcd.color565(110, 120, 110);
  spr.setFont(&fonts::Font0);
  wakeAt = millis(); tInitMs = wakeAt - tb;

  uint32_t t0 = millis(); bool first = true;
  while (millis() - t0 < UI_SPLASH_MS) {
    splashT = (millis() - t0) * UI_SPLASH_ANIM_MS / UI_SPLASH_MS;   // tiempo de animacion (mas lento que el real)
    int64_t a = esp_timer_get_time();
    drawFull(SC_SPLASH);
    noteFrame(a);
    if (first) { first = false; tFirst = millis(); backlight(100); }
    vTaskDelay(pdMS_TO_TICKS(15));
  }
  tSplash = millis() - t0;
  statsT0 = millis();
  st = Stats{};                                                      // las estadisticas de reposo empiezan tras el splash

  uint32_t lastSent = kvmhid::stats().sent, lastRec = blelink::stats().records, busyUntil = 0, lastSig = 0xFFFFFFFF;
  uint32_t keys[4] = {0, 0, 0, 0}, ledGen = ledcfg::gen(), nameGen = devname::gen(); bool needFull = true;
  uint8_t bl = 100; bool shownCourse = false;
  for (;;) {
    ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(100));                   // dormir: course() la despierta; si no, sondeo barato cada 100 ms
    int64_t wa = esp_timer_get_time(); st.wakeups++;
    uint32_t now = millis();
    // actividad HID/BLE (solo contadores sin bloqueo, nunca contenido). Mientras dura (y UI_QUIET_MS despues):
    // CERO dibujo, CERO SPI, CERO LED; solo se marca "hay cambios pendientes" y se redibuja al quedar inactivo.
    uint32_t s = kvmhid::stats().sent, r = blelink::stats().records;
    if (s != lastSent || r != lastRec) { lastSent = s; lastRec = r; busyUntil = now + UI_QUIET_MS; wakeAt = now; sn.hidSeen = true; }
    bool busy = (int32_t)(busyUntil - now) > 0;
    if (digitalRead(PIN_BTN) == LOW) wakeAt = now;
    uint32_t idle = now - wakeAt;
    uint8_t want = (UI_OFF_MS && idle >= UI_OFF_MS) ? 0 : (UI_DIM_MS && idle >= UI_DIM_MS) ? 15 : 100;
    if (want != bl) { bl = want; backlight(bl); }                    // un ledcWrite: coste despreciable
    // cambio de ajuste de la luz: se pinta YA aunque haya actividad (lo pide el usuario y son ~100 flancos de GPIO);
    // con el estado de la ultima comprobacion (sn), que es el que se ve en pantalla
    uint32_t lg = ledcfg::gen();
    if (lg != ledGen) { ledGen = lg; if (lastSig != 0xFFFFFFFF) ledUpdate(sn.ble, sn.sdFail); }
    uint32_t ng = devname::gen();
    if (ng != nameGen) { nameGen = ng; needFull = true; }      // nombre nuevo: cabecera
    if (busy) { st.busyUs += (uint32_t)(esp_timer_get_time() - wa); continue; }

    sn.ble = blelink::connected(); sn.sdFail = kvmmode::sd() && !sdmsc::ready();
    sn.sdReady = kvmmode::sd() && sdmsc::ready(); sn.sdSectors = sn.sdReady ? sdmsc::sectors() : 0;
    sn.usb = tud_suspended() ? 2 : tud_mounted() ? 1 : 0;
    uint32_t sig = (sn.ble ? 1 : 0) | (sn.sdFail ? 2 : 0) | (sn.usb << 3) | ((uint32_t)kvmmode::current() << 5);
    if (sig != lastSig) { if (lastSig != 0xFFFFFFFF) wakeAt = now; lastSig = sig; ledUpdate(sn.ble, sn.sdFail); }
    if (bl) {                                                        // pantalla apagada: cero redibujado
      int64_t a = esp_timer_get_time(); int n = 0;
      if (courseTo) {
        if (!shownCourse) { shownCourse = true; drawFull(SC_COURSE); n++; }
      } else {
        const Layout& L = LAYOUT[kvmmode::current()];
        if (needFull || shownMode != kvmmode::current()) {
          shownMode = kvmmode::current(); needFull = false; drawFull(SC_STATUS); n++;
          for (int i = 0; i < L.n; i++) keys[i] = rowKey(L.type[i]);
        } else for (int i = 0; i < L.n; i++) {
          uint32_t k = rowKey(L.type[i]);
          if (k != keys[i]) { keys[i] = k; drawRegion(SC_STATUS, L.top[i], L.h); n++; }   // solo la banda de la fila que cambio
        }
      }
      if (n) noteFrame(a);
    }
    st.busyUs += (uint32_t)(esp_timer_get_time() - wa);
  }
}
}  // namespace

void begin() {
#ifdef UI_DISABLED
  pinMode(PIN_TFT_BL, OUTPUT); digitalWrite(PIN_TFT_BL, HIGH);   // build A/B sin UI: solo backlight apagado (activo en bajo); ni LCD, ni LED, ni tarea
  return;
#endif
  if (task) return;
  // nucleo 1 (loop/Arduino), prioridad 0: loop() y las tareas HID/BLE/USB siempre la adelantan
  xTaskCreatePinnedToCore(uiTask, "ui", 6144, nullptr, 0, &task, 1);
}
void course(char l) { if (l >= 'A' && l <= 'C') { courseTo = l; if (task) xTaskNotifyGive(task); } }
void wake() { wakeAt = millis(); }
uint32_t firstFrameMs() { return tFirst; }
uint32_t splashMs() { return tSplash; }
uint32_t frameUsMax() { return usMax; }
uint32_t initMs() { return tInitMs; }
void resetStats() { st = Stats{}; statsT0 = millis(); }
const Stats& stats() { return st; }

void report(char* b, size_t n) {
  uint32_t el = millis() - statsT0; if (!el) el = 1;
#ifdef UI_DISABLED
  const int on = 0;
#else
  const int on = 1;
#endif
  snprintf(b, n, "ui=%d el_ms=%lu frames=%lu fps=%.2f frame_avg_us=%lu frame_max_us=%lu bands=%lu spi_kb=%lu spi_us_sum=%lu spi_us_max=%lu spi_busy_pct=%.3f led_wr=%lu wakeups=%lu ui_busy_pct=%.3f init_ms=%lu",
           on, (unsigned long)el, (unsigned long)st.frames, st.frames * 1000.0 / el, (unsigned long)(st.frames ? st.frameUsSum / st.frames : 0),
           (unsigned long)st.frameUsMax, (unsigned long)st.bands, (unsigned long)(st.bytes / 1024), (unsigned long)st.pushUsSum,
           (unsigned long)st.pushUsMax, st.pushUsSum / 10.0 / el, (unsigned long)st.ledWrites, (unsigned long)st.wakeups,
           st.busyUs / 10.0 / el, (unsigned long)tInitMs);
}

// CPU por tarea desde la llamada anterior (contadores FreeRTOS con esp_timer): "nombre@nucleo=%" de un solo nucleo
void cpu(char* b, size_t n) {
  static TaskStatus_t ts[28]; static struct { uint32_t num, run; } prev[28]; static uint32_t prevN = 0; static int64_t prevT = 0;
  uint32_t tot; UBaseType_t k = uxTaskGetSystemState(ts, 28, &tot);
  int64_t nowT = esp_timer_get_time(); double wall = (double)(nowT - prevT); if (wall <= 0) wall = 1;
  int o = snprintf(b, n, "win_ms=%lu", (unsigned long)(wall / 1000));
  for (UBaseType_t i = 0; i < k; i++) {
    uint32_t pr = 0; for (uint32_t j = 0; j < prevN; j++) if (prev[j].num == ts[i].xTaskNumber) pr = prev[j].run;
    double pct = (ts[i].ulRunTimeCounter - pr) * 100.0 / wall;
    if (pct >= 0.05 && o < (int)n - 24) o += snprintf(b + o, n - o, " %s@%c=%.1f", ts[i].pcTaskName, ts[i].xCoreID > 1 ? 'x' : (char)('0' + ts[i].xCoreID), pct);
  }
  for (UBaseType_t i = 0; i < k; i++) { prev[i].num = ts[i].xTaskNumber; prev[i].run = ts[i].ulRunTimeCounter; }
  prevN = k; prevT = nowT;
}
}  // namespace ui
