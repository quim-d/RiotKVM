#include "sd_msc.h"
#include "USBMSC.h"
#include "tusb.h"
#include "esp_timer.h"
#include "driver/sdmmc_host.h"
#include "sdmmc_cmd.h"

namespace sdmsc {
namespace {

USBMSC* msc = nullptr;
sdmmc_card_t card;
bool hostUp = false;
volatile bool s_ready = false, s_change = false;
volatile uint32_t s_sectors = 0, s_initMs = 0;
Stats st{};

TaskHandle_t task = nullptr;
TaskHandle_t waiter = nullptr;

// Una sola operacion en vuelo (bulk-only). El buffer pertenece a la E/S hasta el done.
struct Req { uint32_t lba; void* buf; uint32_t size; bool wr; } req;
volatile bool reqPending = false;

bool cardInit() {
  sdmmc_host_t host = SDMMC_HOST_DEFAULT();
  host.max_freq_khz = SDMMC_FREQ_HIGHSPEED;
  if (!hostUp) {  // el host (e ISR) se crea una vez, en la tarea SD (nucleo 0)
    sdmmc_slot_config_t slot = SDMMC_SLOT_CONFIG_DEFAULT();
    slot.width = 4; slot.clk = GPIO_NUM_12; slot.cmd = GPIO_NUM_16;
    slot.d0 = GPIO_NUM_14; slot.d1 = GPIO_NUM_17; slot.d2 = GPIO_NUM_21; slot.d3 = GPIO_NUM_18;
    slot.flags |= SDMMC_SLOT_FLAG_INTERNAL_PULLUP;
    if (sdmmc_host_init() != ESP_OK) return false;
    if (sdmmc_host_init_slot(SDMMC_HOST_SLOT_1, &slot) != ESP_OK) return false;
    hostUp = true;
  }
  return sdmmc_card_init(&host, &card) == ESP_OK;
}

void setReady(bool r) {
  if (r) {
    uint32_t n = card.csd.capacity;
    if (!n || card.csd.sector_size != 512) { r = false; }
    else {
      msc->begin(n, 512);
      s_sectors = n;
      msc->mediaPresent(true);
    }
  } else {
    msc->mediaPresent(false);
  }
  if (s_ready != r) { s_ready = r; s_change = true; }
}

esp_err_t doIo() {
  return req.wr ? sdmmc_write_sectors(&card, req.buf, req.lba, req.size / 512)
                : sdmmc_read_sectors(&card, req.buf, req.lba, req.size / 512);
}

void handleReq() {
  int64_t t0 = esp_timer_get_time();
  int32_t res = -1;
  if (s_ready) {
    esp_err_t e = doIo();
    if (e != ESP_OK) {            // un reintento; si falla de nuevo, reinicializar la tarjeta
      st.retries++;
      e = doIo();
      if (e != ESP_OK) {
        st.errors++;
        st.reinits++;
        if (cardInit()) e = doIo();
        if (e != ESP_OK) setReady(false);
      }
    }
    if (e == ESP_OK) {
      res = (int32_t)req.size;
      if (req.wr) st.writes++; else st.reads++;
      st.bytes += req.size;
    }
  }
  uint32_t dt = (uint32_t)(esp_timer_get_time() - t0);
  if (dt > st.opUsMax) st.opUsMax = dt;
  reqPending = false;
  tud_msc_async_io_done(res, false);
}

void sdTask(void*) {
  int64_t t0 = esp_timer_get_time();
  bool ok = cardInit();
  s_initMs = (uint32_t)((esp_timer_get_time() - t0) / 1000);
  setReady(ok);
  s_change = false;  // el estado inicial lo muestra setup()
  if (waiter) xTaskNotifyGive(waiter);
  uint32_t lastPoll = millis();
  for (;;) {
    ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(250));
    if (reqPending) { handleReq(); continue; }
    if (millis() - lastPoll < 1000) continue;
    lastPoll = millis();
    if (s_ready) {                // sondeo de presencia (no hay pin de deteccion de tarjeta)
      if (sdmmc_get_status(&card) != ESP_OK) {
        st.reinits++;
        if (!cardInit()) setReady(false);
      }
    } else if (cardInit()) {      // recuperacion / insercion en caliente
      setReady(true);
    }
  }
}

int32_t submit(bool wr, uint32_t lba, void* buf, uint32_t size, uint32_t offset) {
  int64_t t0 = esp_timer_get_time();
  int32_t ret = -1;
  if (!offset && !(size & 511) && s_ready && !reqPending) {
    req = {lba, buf, size, wr};
    reqPending = true;
    xTaskNotifyGive(task);
    ret = TUD_MSC_RET_ASYNC;
  }
  uint32_t dt = (uint32_t)(esp_timer_get_time() - t0);
  if (dt > st.cbUsMax) st.cbUsMax = dt;
  return ret;
}

int32_t onRead(uint32_t lba, uint32_t offset, void* buf, uint32_t size) { return submit(false, lba, buf, size, offset); }
int32_t onWrite(uint32_t lba, uint32_t offset, uint8_t* buf, uint32_t size) { return submit(true, lba, buf, size, offset); }
bool onStartStop(uint8_t, bool, bool) { return true; }  // sin trazas: corre en usbd

}  // namespace

bool begin() {
  msc = new USBMSC();  // registra la interfaz MSC (antes que HID, como en el firmware anterior)
  msc->vendorID("RiotKVM"); msc->productID("SD Ventoy"); msc->productRevision("1.0");
  msc->onRead(onRead); msc->onWrite(onWrite); msc->onStartStop(onStartStop);
  msc->isWritable(true);
  msc->mediaPresent(false);
  waiter = xTaskGetCurrentTaskHandle();
  xTaskCreatePinnedToCore(sdTask, "sdio", 6144, nullptr, 5, &task, 0);
  ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(4000));
  return s_ready;
}

bool ready() { return s_ready; }
uint32_t sectors() { return s_sectors; }
uint32_t initMs() { return s_initMs; }
bool consumeChange() { bool c = s_change; s_change = false; return c; }
const Stats& stats() { return st; }
void resetStats() { st = Stats{}; }

}  // namespace sdmsc

// SYNCHRONIZE CACHE(10) (0x35): el core responde ILLEGAL_REQUEST. Escribimos sin cache (cada
// WRITE10 termina en la tarjeta antes de completar), asi que basta con responder OK.
extern "C" int32_t __real_tud_msc_scsi_cb(uint8_t lun, uint8_t const scsi_cmd[16], void* buffer, uint16_t bufsize);
extern "C" int32_t __wrap_tud_msc_scsi_cb(uint8_t lun, uint8_t const scsi_cmd[16], void* buffer, uint16_t bufsize) {
  if (scsi_cmd[0] == 0x35) return sdmsc::ready() ? 0 : -1;
  return __real_tud_msc_scsi_cb(lun, scsi_cmd, buffer, bufsize);
}
