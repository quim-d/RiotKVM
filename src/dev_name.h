// Nombre del dongle (por si tienes varios), guardado en NVS. Por defecto "RiotKVM". Es el nombre BLE (GAP + scan
// response; el UUID del servicio no cambia) y el que se ve en la cabecera de la pantalla. Lo cambian la orden CDC `name`
// y el CMD BLE SET_NAME. Los descriptores USB (fabricante/producto "RiotKVM") NO cambian.
#pragma once
#include <Arduino.h>

namespace devname {
constexpr size_t MAX_LEN = 20;                 // el scan response legacy (31 B) lleva solo el nombre: 2 + 20 caben de sobra
constexpr const char* DEFAULT_NAME = "RiotKVM";
void load();                                   // lee NVS (por defecto RiotKVM). Llamar al principio de setup()
void get(char* out, size_t n);                 // copia coherente (seguro desde cualquier tarea); n >= MAX_LEN + 1
uint32_t gen();                                // sube con cada cambio: la UI repinta la cabecera y BLE re-anuncia
bool valid(const uint8_t* s, size_t len);      // 1..MAX_LEN de [A-Za-z0-9._-]
// Valida y aplica: guarda en NVS (al momento: es un cambio raro) y re-anuncia el BLE. len 0 = volver a RiotKVM.
// false = no valido (ERR RANGE / err range). Llamar desde loop() (CDC o ejecucion de CMD BLE).
bool set(const uint8_t* s, size_t len);
}
