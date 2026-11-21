// HID del dongle: teclado (ID1, interfaz boot protocol) + raton relativo (ID2) + raton absoluto (ID3).
// Todas las funciones de envio se llaman desde UNA sola tarea (loopTask). Los eventos USB solo
// activan banderas (requestReleaseAll) y despiertan a esa tarea.
#pragma once
#include <Arduino.h>

namespace kvmhid {

enum Res : uint8_t { H_OK = 0, H_BUSY, H_NOHOST, H_BOOT, H_FULL, H_NOHID };

// histograma de latencia interna (us): <250,<500,<1000,<2000,<4000,<8000,<16000,>=16000
struct Hist { uint32_t b[8]; uint32_t n, max; uint64_t sum; };
struct Stats {
  uint32_t sent, failed, bootDropped;
  uint64_t sendUsSum;
  uint32_t sendUsMax;
};

bool enabled();                     // false en modo A (sin HID)
void begin();                       // construye el dispositivo HID (antes de USB.begin)
const char* resStr(Res r);          // "ok", "err busy", ...

Res kbdPressRaw(uint8_t k);         // k: 1..0xA4 o 0xE0..0xE7
Res kbdReleaseRaw(uint8_t k);
Res kbdType(const char* s, size_t n, uint32_t* skipped);  // ASCII en_US; salta >= 0x80
Res mouseRel(int8_t x, int8_t y, int8_t wheel, int8_t pan = 0);
Res mouseRelButtons(uint8_t b);
Res mouseAbs(int16_t x, int16_t y, int8_t wheel, int8_t pan = 0);
Res mouseAbsButtons(uint8_t b);

void requestReleaseAll();           // seguro desde cualquier tarea/evento (no envia)
void releaseAllNow();               // solo desde la tarea de envio: pone a cero y envia
void service();                     // reintenta lo pendiente (informes fallidos); llamar desde loop
bool pending();                     // hay algo por reenviar y el bus lo permite
bool releaseRequested();            // consume la peticion de requestReleaseAll

void setRx(int64_t us);             // marca de tiempo de recepcion (esp_timer us) del registro que se va a ejecutar; 0 = sin marca
void markExec();                    // anota rx->inicio de ejecucion (cola) con la marca actual
const Hist& histQueue();            // rx -> ejecucion
const Hist& histDeliver();          // rx -> informe entregado al host (fin de la transferencia IN)
int repDiag(char* b, size_t n);     // diagnostico: informes por ID (1 kbd, 2 rel, 3 abs): enviados, fallos, ultimo
uint8_t leds();
const Stats& stats();
void resetStats();
bool bootProtocol();                // el host puso HID_PROTOCOL_BOOT (BIOS)

}  // namespace kvmhid
