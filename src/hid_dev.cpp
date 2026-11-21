#include "hid_dev.h"
#include "USBHID.h"
#include "USBHIDKeyboard.h"  // solo para extern KeyboardLayout_en_US y KEY_*
#include "tusb.h"
#include "esp_timer.h"

namespace kvmhid {
namespace {

constexpr uint8_t RID_KBD = HID_REPORT_ID_KEYBOARD;  // 1
constexpr uint8_t RID_REL = HID_REPORT_ID_MOUSE;     // 2
constexpr uint8_t RID_ABS = 3;
// Constantes de keyboardLayout/KeyboardLayout.h (no es API publica)
constexpr uint8_t L_SHIFT = 0x80, L_ALT_GR = 0x40, L_ISO_KEY = 0x64, L_ISO_REPL = 0x32;

const uint8_t kbdDesc[] = {TUD_HID_REPORT_DESC_KEYBOARD(HID_REPORT_ID(RID_KBD))};
const uint8_t relDesc[] = {TUD_HID_REPORT_DESC_MOUSE(HID_REPORT_ID(RID_REL))};
const uint8_t absDesc[] = {TUD_HID_REPORT_DESC_ABSMOUSE(HID_REPORT_ID(RID_ABS))};

volatile uint8_t g_leds = 0;
volatile bool g_relReq = false;

class HidDev : public USBHIDDevice {
public:
  USBHID hid{HID_ITF_PROTOCOL_KEYBOARD};  // el primero que se crea fija el protocolo de la interfaz
  HidDev() { hid.addDevice(this, sizeof(kbdDesc) + sizeof(relDesc) + sizeof(absDesc)); }
  uint16_t _onGetDescriptor(uint8_t* dst) override {
    memcpy(dst, kbdDesc, sizeof(kbdDesc));
    memcpy(dst + sizeof(kbdDesc), relDesc, sizeof(relDesc));
    memcpy(dst + sizeof(kbdDesc) + sizeof(relDesc), absDesc, sizeof(absDesc));
    return sizeof(kbdDesc) + sizeof(relDesc) + sizeof(absDesc);
  }
  void _onOutput(uint8_t id, const uint8_t* b, uint16_t len) override {  // contexto usbd: solo guardar
    if (id == RID_KBD && len >= 1) g_leds = b[0];
  }
};
HidDev* g = nullptr;

// ---- estado deseado + colas de informes pendientes (envio SIN esperar: el endpoint IN va a 1 ms) ----
// Todo se toca desde una sola tarea de envio, salvo el callback de fin de transferencia (usbd), que solo anota la latencia y notifica.
uint8_t kMods = 0, kKeys[6] = {0};
uint8_t relBtn = 0, absBtn = 0;
int16_t absX = 0, absY = 0;
Stats st{};
Hist hq{}, hd{};
volatile int64_t g_rx = 0;
volatile int64_t inflightT = 0;
TaskHandle_t notifyT = nullptr;

void histAdd(Hist& h, int64_t d) {
  if (d < 0) d = 0;
  uint32_t u = d > 0x7FFFFFFF ? 0x7FFFFFFF : (uint32_t)d;
  int i = u < 250 ? 0 : u < 500 ? 1 : u < 1000 ? 2 : u < 2000 ? 3 : u < 4000 ? 4 : u < 8000 ? 5 : u < 16000 ? 6 : 7;
  h.b[i]++; h.n++; h.sum += u; if (u > h.max) h.max = u;
}

struct KRep { uint8_t d[8]; int64_t t; };                          // snapshots de teclado: sin fusion (un toque rapido no se pierde)
struct RRep { uint8_t btn; int32_t x, y, w, p; int64_t t; };       // raton relativo: se fusiona con la cola si los botones son los mismos
struct ARep { uint8_t btn; int16_t x, y; int32_t w, p; int64_t t; };  // absoluto: gana la ultima posicion
template <class T, int N> struct Ring {
  T a[N]; uint8_t h = 0, n = 0;
  bool empty() const { return n == 0; }
  bool full() const { return n == N; }
  T& head() { return a[h]; }
  T& tail() { return a[(h + n - 1) % N]; }
  T& push() { T& r = a[(h + n) % N]; n++; return r; }
  void pop() { h = (h + 1) % N; n--; }
  void clear() { h = n = 0; }
};
Ring<KRep, 32> kq;
Ring<RRep, 16> rq;
Ring<ARep, 16> aq;

bool isBoot() { return g && tud_hid_n_get_protocol(0) == HID_PROTOCOL_BOOT; }
bool canSend() { return g && tud_mounted() && !tud_suspended(); }
inline int8_t cl8(int32_t v) { return v < -127 ? -127 : v > 127 ? 127 : (int8_t)v; }

uint32_t repCnt[4] = {0}, repFail[4] = {0}; uint8_t repLast[4][8] = {{0}}, repLen[4] = {0};   // diagnostico por ID de informe
bool submit(uint8_t id, const void* d, uint16_t n, int64_t t) {
  int64_t t0 = esp_timer_get_time();
  inflightT = t;
  bool ok = tud_hid_n_report(0, isBoot() ? 0 : id, d, n);
  uint32_t dt = (uint32_t)(esp_timer_get_time() - t0);
  st.sendUsSum += dt; if (dt > st.sendUsMax) st.sendUsMax = dt;
  if (id < 4) {
    if (ok) { repCnt[id]++; repLen[id] = n > 8 ? 8 : n; memcpy(repLast[id], d, repLen[id]); } else repFail[id]++;
  }
  if (ok) st.sent++; else { st.failed++; inflightT = 0; }
  return ok;
}

// Envia como mucho UN informe (el endpoint solo admite uno en vuelo). Prioridad: teclado > relativo > absoluto.
void drain() {
  if (!canSend() || !tud_hid_n_ready(0)) return;
  if (!kq.empty()) { if (submit(RID_KBD, kq.head().d, 8, kq.head().t)) kq.pop(); return; }
  if (!rq.empty()) {
    RRep& e = rq.head();
    hid_mouse_report_t r{}; r.buttons = e.btn; r.x = cl8(e.x); r.y = cl8(e.y); r.wheel = cl8(e.w); r.pan = cl8(e.p);
    if (!submit(RID_REL, &r, sizeof r, e.t)) return;
    e.x -= r.x; e.y -= r.y; e.w -= r.wheel; e.p -= r.pan;
    if (!e.x && !e.y && !e.w && !e.p) rq.pop();
    return;
  }
  if (!aq.empty()) {
    ARep& e = aq.head();
    hid_abs_mouse_report_t r{}; r.buttons = e.btn; r.x = e.x; r.y = e.y; r.wheel = cl8(e.w); r.pan = cl8(e.p);
    if (!r.x && !r.y && !r.buttons && !r.wheel) r.x = 1;  // Windows descarta un informe absoluto todo a cero
    if (!submit(RID_ABS, &r, sizeof r, e.t)) return;
    e.w -= r.wheel; e.p -= r.pan;
    if (!e.w && !e.p) aq.pop();
  }
}

// comprobaciones comunes de la ruta de envio (sin esperas: a un host dormido se le pide despertar y el informe se descarta)
Res gate() {
  if (!g) return H_NOHID;
  if (!tud_mounted()) return H_NOHOST;
  if (tud_suspended()) { tud_remote_wakeup(); return H_BUSY; }
  return H_OK;
}

int64_t stamp() { int64_t t = g_rx; return t ? t : esp_timer_get_time(); }

Res kbdPush() {
  Res r = gate(); if (r != H_OK) return r;
  if (kq.full()) { st.failed++; return H_BUSY; }
  KRep& e = kq.push(); memset(e.d, 0, 8); e.d[0] = kMods; memcpy(e.d + 2, kKeys, 6); e.t = stamp();
  drain();
  return H_OK;
}

Res relPush(int32_t x, int32_t y, int32_t w, int32_t p, bool btnChange) {
  if (isBoot()) { st.bootDropped++; return H_BOOT; }  // en boot protocol el ID se quita y el host lo leeria como teclado
  Res r = gate(); if (r != H_OK) return r;
  if (!btnChange && !rq.empty() && rq.tail().btn == relBtn) {  // fusion: sumar deltas pendientes
    RRep& t = rq.tail(); t.x += x; t.y += y; t.w += w; t.p += p;
  } else if (rq.full()) {
    if (btnChange) { st.failed++; return H_BUSY; }
    RRep& t = rq.tail(); t.x += x; t.y += y; t.w += w; t.p += p;
  } else {
    RRep& t = rq.push(); t = {relBtn, x, y, w, p, stamp()};
  }
  drain();
  return H_OK;
}

Res absPush(int16_t x, int16_t y, int32_t w, int32_t p, bool btnChange) {
  if (isBoot()) { st.bootDropped++; return H_BOOT; }
  Res r = gate(); if (r != H_OK) return r;
  absX = x; absY = y;
  if (!btnChange && !aq.empty() && aq.tail().btn == absBtn) {  // ultima posicion gana
    ARep& t = aq.tail(); t.x = x; t.y = y; t.w += w; t.p += p;
  } else if (aq.full()) {
    if (btnChange) { st.failed++; return H_BUSY; }
    ARep& t = aq.tail(); t.x = x; t.y = y; t.w += w; t.p += p;
  } else {
    ARep& t = aq.push(); t = {absBtn, x, y, w, p, stamp()};
  }
  drain();
  return H_OK;
}

// espera (solo para `type`, nunca en la ruta caliente) a que haya hueco en la cola de teclado
bool waitKbdRoom() {
  for (int i = 0; i < 300 && kq.n > 28; i++) { drain(); vTaskDelay(1); }
  return kq.n <= 28;
}

}  // namespace

// fin de transferencia del endpoint HID (contexto usbd). tud_hid_report_complete_cb no se puede envolver (el
// core la define debil en el mismo objeto), asi que se envuelve hidd_xfer_cb. Mide rx->entrega y despierta al emisor.
extern "C" bool __real_hidd_xfer_cb(uint8_t rhport, uint8_t ep_addr, xfer_result_t result, uint32_t xferred_bytes);
extern "C" bool __wrap_hidd_xfer_cb(uint8_t rhport, uint8_t ep_addr, xfer_result_t result, uint32_t xferred_bytes) {
  bool in = ep_addr & 0x80;
  int64_t t = in ? inflightT : 0;
  if (t) { inflightT = 0; histAdd(hd, esp_timer_get_time() - t); }
  bool r = __real_hidd_xfer_cb(rhport, ep_addr, result, xferred_bytes);
  if (in && notifyT) xTaskNotifyGive(notifyT);
  return r;
}

bool enabled() { return g != nullptr; }

void begin() {
  notifyT = xTaskGetCurrentTaskHandle();
  g = new HidDev();
  g->hid.begin();
}

const char* resStr(Res r) {
  switch (r) {
    case H_OK: return "ok";
    case H_BUSY: return "err busy";
    case H_NOHOST: return "err nohost";
    case H_BOOT: return "err bootproto";
    case H_FULL: return "err rollover";
    case H_NOHID: return "err nohid";
  }
  return "err";
}

Res kbdPressRaw(uint8_t k) {
  if (k >= 0xE0 && k < 0xE8) kMods |= 1 << (k - 0xE0);
  else {
    bool have = false; int slot = -1;
    for (int i = 0; i < 6; i++) { if (kKeys[i] == k) have = true; if (kKeys[i] == 0 && slot < 0) slot = i; }
    if (!have) { if (slot < 0) return H_FULL; kKeys[slot] = k; }
  }
  return kbdPush();
}

Res kbdReleaseRaw(uint8_t k) {
  if (k >= 0xE0 && k < 0xE8) kMods &= ~(1 << (k - 0xE0));
  else for (int i = 0; i < 6; i++) if (kKeys[i] == k) kKeys[i] = 0;
  return kbdPush();
}

Res kbdType(const char* s, size_t n, uint32_t* skipped) {
  *skipped = 0;
  for (size_t i = 0; i < n; i++) {
    uint8_t c = (uint8_t)s[i];
    if (c >= 0x80) { (*skipped)++; continue; }
    uint8_t k = KeyboardLayout_en_US[c];
    if (!k) { (*skipped)++; continue; }
    uint8_t saveMods = kMods; uint8_t saveKeys[6]; memcpy(saveKeys, kKeys, 6);
    if (k & L_SHIFT) { kMods |= 0x02; k &= ~L_SHIFT; }
    if (k & L_ALT_GR) { kMods |= 0x40; k &= ~L_ALT_GR; }
    if (k == L_ISO_REPL) k = L_ISO_KEY;
    int slot = -1;
    for (int j = 0; j < 6; j++) if (kKeys[j] == 0) { slot = j; break; }
    if (slot < 0) { kMods = saveMods; return H_FULL; }
    if (!waitKbdRoom()) { kMods = saveMods; return H_BUSY; }
    kKeys[slot] = k;
    Res r = kbdPush();
    kMods = saveMods; memcpy(kKeys, saveKeys, 6);  // soltar la tecla
    Res r2 = kbdPush();
    if (r != H_OK || r2 != H_OK) return r != H_OK ? r : r2;
  }
  return H_OK;
}

Res mouseRel(int8_t x, int8_t y, int8_t wheel, int8_t pan) { return relPush(x, y, wheel, pan, false); }

Res mouseRelButtons(uint8_t b) {
  uint8_t old = relBtn; relBtn = b;
  Res r = relPush(0, 0, 0, 0, true);
  if (r != H_OK && r != H_BOOT) relBtn = old;   // el host no lo vio: mantener coherencia
  return r;
}

Res mouseAbs(int16_t x, int16_t y, int8_t wheel, int8_t pan) { return absPush(x, y, wheel, pan, false); }

Res mouseAbsButtons(uint8_t b) {
  uint8_t old = absBtn; absBtn = b;
  Res r = absPush(absX, absY, 0, 0, true);
  if (r != H_OK && r != H_BOOT) absBtn = old;
  return r;
}

void requestReleaseAll() { g_relReq = true; }
bool releaseRequested() { bool r = g_relReq; g_relReq = false; return r; }

void releaseAllNow() {
  bool hadRel = relBtn, hadAbs = absBtn;
  memset(kKeys, 0, 6); kMods = 0; relBtn = absBtn = 0;
  kq.clear(); rq.clear(); aq.clear();   // lo pendiente ya no es fiable: se sustituye por "todo suelto"
  if (!g || !tud_mounted()) return;
  KRep& e = kq.push(); memset(e.d, 0, 8); e.t = esp_timer_get_time();
  if (hadRel) { RRep& r = rq.push(); r = {0, 0, 0, 0, 0, esp_timer_get_time()}; }
  if (hadAbs) { ARep& a = aq.push(); a = {0, absX, absY, 0, 0, esp_timer_get_time()}; }
  drain();
}

void service() { drain(); }

int repDiag(char* b, size_t n) {   // por ID: informes enviados, fallos y ultimos bytes (sin el ID)
  int k = 0;
  for (int i = 1; i <= 3 && k < (int)n - 40; i++) {
    k += snprintf(b + k, n - k, "id%d n=%lu fail=%lu last=", i, (unsigned long)repCnt[i], (unsigned long)repFail[i]);
    for (int j = 0; j < repLen[i]; j++) k += snprintf(b + k, n - k, "%02X", repLast[i][j]);
    k += snprintf(b + k, n - k, " ");
  }
  return k;
}

bool pending() { return (!kq.empty() || !rq.empty() || !aq.empty()) && canSend(); }
uint8_t leds() { return g_leds; }
const Stats& stats() { return st; }
void resetStats() { st = Stats{}; hq = Hist{}; hd = Hist{}; memset(repCnt, 0, sizeof repCnt); memset(repFail, 0, sizeof repFail); }
void setRx(int64_t us) { g_rx = us; }
void markExec() { if (g_rx) histAdd(hq, esp_timer_get_time() - g_rx); }
const Hist& histQueue() { return hq; }
const Hist& histDeliver() { return hd; }
bool bootProtocol() { return isBoot(); }

}  // namespace kvmhid
