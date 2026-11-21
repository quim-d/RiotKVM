// microSD (SDMMC 4 bit) expuesta como USB MSC con E/S asincrona (TUD_MSC_RET_ASYNC).
// Los callbacks de TinyUSB (tarea usbd) solo encolan; una tarea en el nucleo 0 hace sdmmc_* y
// completa con tud_msc_async_io_done(). Un unico hilo toca la tarjeta (init, E/S, sondeo, reinit).
#pragma once
#include <Arduino.h>

namespace sdmsc {

struct Stats {
  uint32_t reads, writes, errors, retries, reinits;
  uint64_t bytes;
  uint32_t opUsMax;       // duracion maxima de una E/S de SD (en la tarea SD, no en usbd)
  uint32_t cbUsMax;       // tiempo maximo dentro del callback MSC (en usbd): debe ser de microsegundos
};

// Configura el MSC y lanza la tarea; espera el primer intento de init (max ~3 s). Antes de USB.begin().
bool begin();
bool ready();               // tarjeta lista y mediaPresent(true)
uint32_t sectors();
uint32_t initMs();          // duracion del primer init (ms)
bool consumeChange();       // true una vez cuando cambia ready() (para refrescar pantalla)
const Stats& stats();
void resetStats();

}  // namespace sdmsc
