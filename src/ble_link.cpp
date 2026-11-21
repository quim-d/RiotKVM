#include "ble_link.h"
#include "hid_dev.h"
#include "sd_msc.h"
#include "kvm_mode.h"
#include "tusb.h"
#include "esp_timer.h"
#include "nimble/nimble_port.h"
#include "nimble/nimble_port_freertos.h"
#include "host/ble_hs.h"
#include "host/ble_uuid.h"
#include "host/util/util.h"
#include "services/gap/ble_svc_gap.h"
#include "services/gatt/ble_svc_gatt.h"

// El core libera la memoria del controlador BT al arrancar salvo que bleInUse() sea true (si no, nimble_port_init falla y cae)
extern "C" bool bleInUse(void) { return true; }

namespace blelink {
namespace {

constexpr uint8_t PROTO_VER = 1, FW_MAJOR = 1, FW_MINOR = 0;
constexpr uint16_t NONE = 0xFFFF;
constexpr size_t MAXW = 253;  // ATT_MTU preferido 256 - 3

// op de CMD
enum : uint8_t { OP_KEY_DOWN = 1, OP_KEY_UP, OP_RELEASE_ALL, OP_MOUSE_REL, OP_MOUSE_ABS, OP_BTN_REL, OP_BTN_ABS, OP_WHEEL,
                 OP_PING = 0x10, OP_GET_STATE, OP_SET_MODE };
// op de EVT
enum : uint8_t { EV_STATE = 0x81, EV_PONG, EV_ERR };
enum : uint8_t { E_BUSY = 1, E_RANGE, E_UNKNOWN_OP, E_MALFORMED, E_NO_HOST, E_NO_HID, E_BOOT_PROTO, E_ROLLOVER };

// UUID 7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f00NN en orden LSB primero
#define KVM_UUID(n) BLE_UUID128_INIT(n, 0x00, 0x1f, 0x6e, 0x5c, 0x2a, 0x3f, 0x8d, 0x7a, 0x4b, 0x6e, 0x9b, 0x00, 0x10, 0x0d, 0x7d)
const ble_uuid128_t uSvc = KVM_UUID(0x01), uCmd = KVM_UUID(0x02), uEvt = KVM_UUID(0x03), uInfo = KVM_UUID(0x04);

struct Item { uint16_t conn; uint8_t len; int64_t t; uint8_t d[MAXW]; };
QueueHandle_t q = nullptr;
TaskHandle_t loopT = nullptr;
volatile uint16_t cur = NONE;
volatile bool subscribed = false, stateForce = false;
volatile uint16_t g_mtu = 23, g_itvl = 0, g_lat = 0;
volatile int g_updRc = -1;                 // rc del ultimo ble_gap_update_params (0 = peticion aceptada por el controlador)
volatile uint32_t g_connAt = 0, g_updAt = 0, g_updTries = 0, g_updEvents = 0;
struct Hv { uint32_t ms; uint16_t itvl; uint8_t st; }; Hv hist[8]; volatile uint8_t histN = 0;   // historial de cambios de intervalo
volatile bool g_slowAdv = true;            // anuncio lento mientras hay cliente (menos competencia con los eventos de conexion)
volatile uint16_t g_wantItvl = 6, g_wantMax = 6;          // 6 x 1,25 = 7,5 ms
void requestParams(uint16_t h) {   // min=max=7,5 ms, latencia 0, timeout 4 s
  struct ble_gap_upd_params up = {};
  up.itvl_min = g_wantItvl; up.itvl_max = g_wantMax; up.latency = 0; up.supervision_timeout = 400;
  up.min_ce_len = 0; up.max_ce_len = 0;
  g_updRc = ble_gap_update_params(h, &up); g_updAt = millis(); g_updTries++;
}
void readConn(uint16_t h) { struct ble_gap_conn_desc d; if (ble_gap_conn_find(h, &d) == 0) { g_itvl = d.conn_itvl; g_lat = d.conn_latency; } }
uint16_t evtHandle = 0;
uint8_t ownType = 0;
Stats st{};
static RTC_NOINIT_ATTR volatile int rtcStage;  // ultimo paso alcanzado (sobrevive a un reset por WDT)
int dInit = -99, dCount = -99, dAdd = -99, dAdv = -99; volatile int dSync = 0;

// ---- ruta de loop(): ejecutar registros ----
uint8_t errCode(kvmhid::Res r) {
  switch (r) {
    case kvmhid::H_BUSY: return E_BUSY;
    case kvmhid::H_NOHOST: return E_NO_HOST;
    case kvmhid::H_BOOT: return E_BOOT_PROTO;
    case kvmhid::H_FULL: return E_ROLLOVER;
    case kvmhid::H_NOHID: return E_NO_HID;
    default: return 0;
  }
}

bool notifyRaw(const uint8_t* d, size_t n) {
  uint16_t c = cur;
  if (c == NONE || !subscribed) return false;
  struct os_mbuf* om = ble_hs_mbuf_from_flat(d, n);
  if (!om) return false;
  st.notifies++;
  return ble_gatts_notify_custom(c, evtHandle, om) == 0;
}

int errTokens = 20; uint32_t errRefill = 0;
void sendErr(uint8_t op, uint8_t code) {
  uint32_t now = millis();
  if (now - errRefill >= 50) { errRefill = now; if (errTokens < 20) errTokens++; }
  if (errTokens <= 0) return;
  errTokens--; st.errsSent++;
  uint8_t b[4] = {EV_ERR, 2, op, code};
  notifyRaw(b, 4);
}

void stateBytes(uint8_t* o) {  // 3 bytes
  uint8_t f = 0;
  if (tud_mounted()) f |= 1;
  if (tud_suspended()) f |= 2;
  if (sdmsc::ready()) f |= 4;
  if (kvmhid::bootProtocol()) f |= 8;
  if (kvmmode::hid()) f |= 16;
  if (kvmmode::sd()) f |= 32;
  o[0] = f; o[1] = kvmhid::leds(); o[2] = kvmmode::current();
}

uint8_t lastState[3] = {0xFF, 0xFF, 0xFF};
uint32_t lastStateMs = 0;
void sendState() {
  uint8_t b[5] = {EV_STATE, 3};
  stateBytes(b + 2);
  memcpy(lastState, b + 2, 3); lastStateMs = millis();
  notifyRaw(b, 5);
}

// movimiento relativo agrupado dentro de una escritura
struct Acc { int x = 0, y = 0, w = 0, p = 0; bool any = false; };
int clampi(int v, int lo, int hi) { return v < lo ? lo : v > hi ? hi : v; }
void flushRel(Acc& a) {
  while (a.any) {
    int sx = clampi(a.x, -127, 127), sy = clampi(a.y, -127, 127), sw = clampi(a.w, -127, 127), sp = clampi(a.p, -127, 127);
    kvmhid::Res r = kvmhid::mouseRel((int8_t)sx, (int8_t)sy, (int8_t)sw, (int8_t)sp);
    if (r != kvmhid::H_OK) { sendErr(OP_MOUSE_REL, errCode(r)); a = Acc(); return; }
    a.x -= sx; a.y -= sy; a.w -= sw; a.p -= sp;
    a.any = a.x || a.y || a.w || a.p;
  }
  a = Acc();
}

void execute(const uint8_t* d, size_t n) {
  Acc acc;
  size_t i = 0;
  while (i < n) {
    if (i + 2 > n) { st.malformed++; sendErr(0, E_MALFORMED); break; }
    uint8_t op = d[i], len = d[i + 1];
    if (i + 2 + len > n) { st.malformed++; sendErr(op, E_MALFORMED); break; }
    const uint8_t* p = d + i + 2;
    i += 2 + len;
    st.records++;
    if (op != OP_MOUSE_REL && op != OP_WHEEL) flushRel(acc);
    kvmhid::Res r = kvmhid::H_OK; uint8_t err = 0;
    switch (op) {
      case OP_KEY_DOWN: case OP_KEY_UP:
        if (len != 1) { err = E_MALFORMED; break; }
        if (!((p[0] >= 1 && p[0] <= 0xA4) || (p[0] >= 0xE0 && p[0] <= 0xE7))) { err = E_RANGE; break; }
        r = op == OP_KEY_DOWN ? kvmhid::kbdPressRaw(p[0]) : kvmhid::kbdReleaseRaw(p[0]);
        break;
      case OP_RELEASE_ALL:
        if (len != 0) { err = E_MALFORMED; break; }
        kvmhid::releaseAllNow(); break;
      case OP_MOUSE_REL:
        if (len != 2 && len != 4) { err = E_MALFORMED; break; }
        acc.x += (int8_t)p[0]; acc.y += (int8_t)p[1];
        if (len == 4) { acc.w += (int8_t)p[2]; acc.p += (int8_t)p[3]; }
        acc.any = true; break;
      case OP_MOUSE_ABS: {
        if (len != 4 && len != 6) { err = E_MALFORMED; break; }
        int x = p[0] | (p[1] << 8), y = p[2] | (p[3] << 8);
        r = kvmhid::mouseAbs((int16_t)clampi(x, 0, 32767), (int16_t)clampi(y, 0, 32767),
                             len == 6 ? clampi((int8_t)p[4], -127, 127) : 0, len == 6 ? clampi((int8_t)p[5], -127, 127) : 0);
        break; }
      case OP_BTN_REL: case OP_BTN_ABS:
        if (len != 1) { err = E_MALFORMED; break; }
        if (p[0] > 0x1F) { err = E_RANGE; break; }
        r = op == OP_BTN_REL ? kvmhid::mouseRelButtons(p[0]) : kvmhid::mouseAbsButtons(p[0]);
        break;
      case OP_WHEEL:
        if (len != 2) { err = E_MALFORMED; break; }
        acc.w += (int8_t)p[0]; acc.p += (int8_t)p[1]; acc.any = true; break;
      case OP_PING: {
        if (len != 2) { err = E_MALFORMED; break; }
        uint32_t t = (uint32_t)esp_timer_get_time();
        uint8_t b[8] = {EV_PONG, 6, p[0], p[1], (uint8_t)t, (uint8_t)(t >> 8), (uint8_t)(t >> 16), (uint8_t)(t >> 24)};
        notifyRaw(b, 8); break; }
      case OP_GET_STATE:
        if (len != 0) { err = E_MALFORMED; break; }
        sendState(); break;
      case OP_SET_MODE:
        if (len != 1) { err = E_MALFORMED; break; }
        if (!kvmmode::request(p[0])) err = E_RANGE;
        break;
      default:
        st.unknown++; err = E_UNKNOWN_OP; break;
    }
    if (r != kvmhid::H_OK) err = errCode(r);
    if (err) sendErr(op, err);
  }
  flushRel(acc);
}

// ---- GATT ----
int chrAccess(uint16_t conn, uint16_t, struct ble_gatt_access_ctxt* ctxt, void* arg) {
  intptr_t which = (intptr_t)arg;
  if (which == 2 && ctxt->op == BLE_GATT_ACCESS_OP_WRITE_CHR) {   // CMD
    Item it;
    it.t = esp_timer_get_time();
    uint16_t len = OS_MBUF_PKTLEN(ctxt->om);
    if (len > MAXW) len = MAXW;
    uint16_t got = 0;
    if (ble_hs_mbuf_to_flat(ctxt->om, it.d, len, &got) != 0) return BLE_ATT_ERR_UNLIKELY;
    it.conn = conn; it.len = (uint8_t)got;
    st.writes++;
    if (xQueueSend(q, &it, 0) != pdTRUE) st.dropped++;
    else if (loopT) xTaskNotifyGive(loopT);
    return 0;
  }
  if (which == 4 && ctxt->op == BLE_GATT_ACCESS_OP_READ_CHR) {    // INFO
    uint16_t caps = 0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20;
    uint8_t b[8] = {PROTO_VER, FW_MAJOR, FW_MINOR, kvmmode::current(), (uint8_t)caps, (uint8_t)(caps >> 8), 0, 0};
    return os_mbuf_append(ctxt->om, b, sizeof b) == 0 ? 0 : BLE_ATT_ERR_INSUFFICIENT_RES;
  }
  return BLE_ATT_ERR_UNLIKELY;
}

struct ble_gatt_chr_def chrs[4];
struct ble_gatt_svc_def svcs[2];

void advStart();

int gapEvent(struct ble_gap_event* ev, void*) {
  switch (ev->type) {
    case BLE_GAP_EVENT_CONNECT:
      if (ev->connect.status == 0) {
        uint16_t h = ev->connect.conn_handle, old = cur;
        cur = h; subscribed = false; g_mtu = 23; st.connects++;
        if (old != NONE) { st.displaced++; ble_gap_terminate(old, BLE_ERR_REM_USER_CONN_TERM); }  // un cliente nuevo desplaza al anterior
        g_connAt = millis(); g_updTries = 0; histN = 0;
        requestParams(h);   // sin set_data_len: los comandos caben en una PDU de 27 B
        readConn(h);
        kvmhid::requestReleaseAll(); if (loopT) xTaskNotifyGive(loopT);
      }
      advStart();  // seguir anunciando (tambien con cliente) para poder desplazarlo
      break;
    case BLE_GAP_EVENT_DISCONNECT: {
      uint16_t h = ev->disconnect.conn.conn_handle;
      if (h == cur) { cur = NONE; subscribed = false; }
      kvmhid::requestReleaseAll(); if (loopT) xTaskNotifyGive(loopT);
      advStart();
      break; }
    case BLE_GAP_EVENT_SUBSCRIBE:
      if (ev->subscribe.attr_handle == evtHandle && ev->subscribe.conn_handle == cur) {
        subscribed = ev->subscribe.cur_notify; stateForce = true; if (loopT) xTaskNotifyGive(loopT);
        if (g_itvl > g_wantMax) requestParams(cur);
      }
      break;
    case BLE_GAP_EVENT_CONN_UPDATE:
      if (ev->conn_update.conn_handle == cur) { readConn(cur); g_updEvents++; Hv& h = hist[histN++ & 7]; h.ms = millis() - g_connAt; h.itvl = g_itvl; h.st = (uint8_t)ev->conn_update.status; }
      break;
    case BLE_GAP_EVENT_MTU:
      if (ev->mtu.conn_handle == cur) g_mtu = ev->mtu.value;
      break;
    case BLE_GAP_EVENT_ADV_COMPLETE:
      advStart(); break;
    case BLE_GAP_EVENT_REPEAT_PAIRING:   // sin bonding: aceptar de nuevo (inseguro a proposito)
      return BLE_GAP_REPEAT_PAIRING_RETRY;
    default: break;
  }
  return 0;
}

// La libreria precompilada trae NimBLE con anuncio extendido (EXT_ADV): ble_gap_adv_start() devuelve ENOTSUPP,
// asi que se usa la instancia 0 con PDU legacy (ADV_IND + SCAN_RSP), visible para cualquier central BLE 4.x.
void advStart() {
  static bool configured = false;
  static bool curSlow = false;
  bool wantSlow = g_slowAdv && cur != NONE;
  if (!configured || curSlow != wantSlow) {
    if (configured) ble_gap_ext_adv_stop(0);
    configured = false;
    struct ble_gap_ext_adv_params p = {};
    p.connectable = 1; p.scannable = 1; p.legacy_pdu = 1;
    p.own_addr_type = ownType;
    p.primary_phy = BLE_HCI_LE_PHY_1M; p.secondary_phy = BLE_HCI_LE_PHY_1M;
    if (wantSlow) { p.itvl_min = BLE_GAP_ADV_ITVL_MS(400); p.itvl_max = BLE_GAP_ADV_ITVL_MS(500); }
    else { p.itvl_min = BLE_GAP_ADV_ITVL_MS(30); p.itvl_max = BLE_GAP_ADV_ITVL_MS(50); }
    p.channel_map = 0; p.sid = 0; p.tx_power = 127;
    dAdv = ble_gap_ext_adv_configure(0, &p, nullptr, gapEvent, nullptr);
    if (dAdv != 0) return;
    struct ble_hs_adv_fields f = {};
    f.flags = BLE_HS_ADV_F_DISC_GEN | BLE_HS_ADV_F_BREDR_UNSUP;
    f.uuids128 = (ble_uuid128_t*)&uSvc; f.num_uuids128 = 1; f.uuids128_is_complete = 1;
    struct os_mbuf* d = os_msys_get_pkthdr(0, 0);
    if (!d || ble_hs_adv_set_fields_mbuf(&f, d) != 0 || (dAdv = ble_gap_ext_adv_set_data(0, d)) != 0) return;
    struct ble_hs_adv_fields r = {};
    static const char name[] = "QKVM";
    r.name = (const uint8_t*)name; r.name_len = sizeof(name) - 1; r.name_is_complete = 1;
    struct os_mbuf* m = os_msys_get_pkthdr(0, 0);
    if (!m || ble_hs_adv_set_fields_mbuf(&r, m) != 0 || (dAdv = ble_gap_ext_adv_rsp_set_data(0, m)) != 0) return;
    configured = true; curSlow = wantSlow;
  }
  dAdv = ble_gap_ext_adv_start(0, 0, 0);   // EALREADY(2) si ya anuncia: ignorar
}

void onSync() {
  dSync++; rtcStage = 6;
  ble_hs_id_infer_auto(0, &ownType);
  advStart();
}

void hostTask(void*) { nimble_port_run(); nimble_port_freertos_deinit(); }

}  // namespace

static void beginImpl();
static void initTask(void*) { beginImpl(); vTaskDelete(nullptr); }
void begin(TaskHandle_t loopTask) {
  loopT = loopTask;
  xTaskCreatePinnedToCore(initTask, "bleinit", 8192, nullptr, 5, nullptr, 0);   // el controlador BT esta fijado al nucleo 0
}

static void beginImpl() {
  rtcStage = 1;
  q = xQueueCreate(16, sizeof(Item));
  chrs[0] = {}; chrs[0].uuid = &uCmd.u; chrs[0].access_cb = chrAccess; chrs[0].arg = (void*)2;
  chrs[0].flags = BLE_GATT_CHR_F_WRITE_NO_RSP | BLE_GATT_CHR_F_WRITE;
  chrs[1] = {}; chrs[1].uuid = &uEvt.u; chrs[1].access_cb = chrAccess; chrs[1].arg = (void*)3;
  chrs[1].flags = BLE_GATT_CHR_F_NOTIFY; chrs[1].val_handle = &evtHandle;
  chrs[2] = {}; chrs[2].uuid = &uInfo.u; chrs[2].access_cb = chrAccess; chrs[2].arg = (void*)4;
  chrs[2].flags = BLE_GATT_CHR_F_READ;
  chrs[3] = {};
  svcs[0] = {}; svcs[0].type = BLE_GATT_SVC_TYPE_PRIMARY; svcs[0].uuid = &uSvc.u; svcs[0].characteristics = chrs;
  svcs[1] = {};
  dInit = nimble_port_init();
  rtcStage = 2;
  if (dInit != ESP_OK) return;
  ble_hs_cfg.sync_cb = onSync;
  ble_hs_cfg.sm_io_cap = BLE_HS_IO_NO_INPUT_OUTPUT;  // Just Works
  ble_hs_cfg.sm_bonding = 0; ble_hs_cfg.sm_mitm = 0; ble_hs_cfg.sm_sc = 0;
  ble_svc_gap_init(); ble_svc_gatt_init();
  ble_svc_gap_device_name_set("QKVM");
  rtcStage = 3;
  ble_att_set_preferred_mtu(247);
  dCount = ble_gatts_count_cfg(svcs);
  dAdd = ble_gatts_add_svcs(svcs);
  rtcStage = 4;
  nimble_port_freertos_init(hostTask);
  rtcStage = 5;
}

void service() {
  if (!q) return;
  if (cur != NONE && g_itvl > g_wantMax) {   // Windows concede 7,5 ms a ratos y rechaza (HCI 59) o revierte: insistir pronto y luego mas espaciado
    uint32_t now = millis(), gap = g_updTries < 3 ? 200 : g_updTries < 10 ? 500 : 10000;
    if (now - g_updAt >= gap) { requestParams(cur); readConn(cur); }
  }
  Item it;
  while (xQueueReceive(q, &it, 0) == pdTRUE) {
    if (it.conn != cur) continue;  // del cliente desplazado/desconectado
    kvmhid::setRx(it.t); kvmhid::markExec();
    execute(it.d, it.len);
    kvmhid::setRx(0);
  }
  uint8_t s[3]; stateBytes(s);
  bool changed = memcmp(s, lastState, 3) != 0;
  if (stateForce || (changed && millis() - lastStateMs >= 20)) {
    stateForce = false;
    if (cur != NONE) sendState(); else memcpy(lastState, s, 3);
  }
}

int paramDiag(char* b, size_t n) {
  int k = 0; char hb[120] = ""; uint8_t cnt = histN;
  for (uint8_t i = cnt > 8 ? cnt - 8 : 0; i < cnt && k < 100; i++) k += snprintf(hb + k, sizeof hb - k, " %lu:%.1f/%u", (unsigned long)hist[i & 7].ms, hist[i & 7].itvl * 1.25, hist[i & 7].st);
  return snprintf(b, n, "hist(ms:itvl/st)=%s itvl_ms=%.2f lat=%u want_ms=%.2f upd_rc=%d upd_tries=%lu upd_events=%lu slow_adv=%d", hb, itvlUnits() * 1.25, (unsigned)g_lat, g_wantItvl * 1.25, (int)g_updRc,
                  (unsigned long)g_updTries, (unsigned long)g_updEvents, g_slowAdv ? 1 : 0);
}
void setSlowAdv(bool on) { g_slowAdv = on; if (cur != NONE) ble_gap_ext_adv_stop(0); advStart(); }
void setWantItvl(uint16_t u, uint16_t mx) { g_wantItvl = u; g_wantMax = mx; g_updTries = 0; if (cur != NONE) requestParams(cur); }
int diag(char* b, size_t n) { return snprintf(b, n, "stage=%d init=%d count=%d add=%d sync=%d adv=%d", rtcStage, dInit, dCount, dAdd, (int)dSync, dAdv); }
void kick() { uint16_t c = cur; if (c != NONE) ble_gap_terminate(c, BLE_ERR_REM_USER_CONN_TERM); }
bool connected() { return cur != NONE; }
uint16_t mtu() { return g_mtu; }
uint16_t itvlUnits() { return cur == NONE ? 0 : g_itvl; }
const Stats& stats() { return st; }
void resetStats() { st = Stats{}; }

}  // namespace blelink
