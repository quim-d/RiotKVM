// UI del LCD (ST7735 80x160) con estetica hacker pirata: splash con calavera, pantalla de estado tipo terminal,
// LED APA102 de acento y ahorro de backlight. Corre en SU PROPIA tarea (nucleo 1, prioridad 0 = por debajo de loop()).
// La UI solo LEE estado de otros modulos (kvmmode, kvmhid::stats, sdmsc, blelink, tud_*): no los modifica.
#pragma once
#include <Arduino.h>

#ifndef UI_DIM_MS
#define UI_DIM_MS 30000UL      // sin actividad: atenuar backlight (0 = nunca)
#endif
#ifndef UI_OFF_MS
#define UI_OFF_MS 120000UL     // sin actividad: apagar backlight (0 = nunca)
#endif
#ifndef UI_QUIET_MS
#define UI_QUIET_MS 600UL      // tras la ultima entrada HID/BLE: sin dibujo/SPI/LED hasta pasar este silencio
#endif
#ifndef UI_SPLASH_VARIANT
#define UI_SPLASH_VARIANT 2    // 1 = glitch, 2 = BIOS (defecto), 3 = apertura
#endif
#ifndef UI_SPLASH_MS
#define UI_SPLASH_MS 1800      // duracion del splash de arranque (antes 1200: la animacion va 1,5x mas lenta; solo la pantalla:
                               // USB ya ha arrancado y BLE arranca a los 3 s desde loop(), sin esperar a la UI)
#endif
#ifndef UI_SPLASH_ANIM_MS
#define UI_SPLASH_ANIM_MS 1200 // escala de tiempos con la que estan escritas las variantes (splashV1..V3)
#endif

namespace ui {
struct Stats { uint32_t frames, frameUsSum, frameUsMax, bands, bytes, pushUsSum, pushUsMax, ledWrites, wakeups, busyUs; };
void resetStats();
const Stats& stats();
void report(char* b, size_t n);   // linea de coste de la UI (comando CDC `ui`)
void cpu(char* b, size_t n);      // CPU por tarea desde la llamada anterior (comando CDC `cpu`)
uint32_t initMs();                // lcd.init + sprite (ms)
void begin();                  // crea la tarea; vuelve de inmediato (llamar tras USB.begin())
void course(char toLetter);    // "CAMBIANDO RUMBO..." hacia el modo 'A'..'C' (llamar justo antes de pedir el cambio)
void wake();                   // enciende el backlight y reinicia el temporizador de inactividad (seguro desde cualquier tarea)
uint32_t firstFrameMs();       // millis() cuando se vio el primer fotograma del splash
uint32_t splashMs();           // duracion real del splash (0 mientras dura)
uint32_t frameUsMax();         // peor tiempo de componer+enviar un fotograma (us)
}
