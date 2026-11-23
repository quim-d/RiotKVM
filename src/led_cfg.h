// Ajuste de la luz (LED APA102 del dongle), guardado en NVS: auto (color segun modo / BLE / fallo de SD, el de siempre),
// color fijo con brillo, o apagado. Lo cambian la orden CDC `led` y el CMD BLE SET_LED; lo lee la tarea UI (ui_lcd).
#pragma once
#include <Arduino.h>

namespace ledcfg {
enum : uint8_t { AUTO = 0, FIXED = 1, OFF = 2 };
struct Cfg { uint8_t mode, r, g, b, bright; };   // bright 0..100 (%)
void load();                    // lee NVS (por defecto AUTO). Llamar al principio de setup()
Cfg get();                      // copia coherente (seguro desde cualquier tarea)
uint32_t gen();                 // sube con cada cambio: la UI repinta el LED al verlo
bool set(const Cfg& c);         // valida (mode <= 2, bright <= 100); false = fuera de rango. Aplica ya; NVS diferido
void service();                 // desde loop: guarda en NVS tras 1 s sin cambios (arrastrar el brillo no gasta flash)
void describe(char* b, size_t n);   // "auto" | "off" | "RRGGBB@NN"
}
