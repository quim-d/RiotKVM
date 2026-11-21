// Modo USB del dongle: A = solo SD (MSC), B = solo teclado+raton (HID), C = ambos. Guardado en NVS.
// Cambiar de modo = guardar + soltar todo + reiniciar (el USB se re-enumera con otro PID y otro descriptor).
#pragma once
#include <Arduino.h>

namespace kvmmode {
enum : uint8_t { A = 0, B = 1, C = 2 };
void load();                 // lee NVS (por defecto C). Llamar al principio de setup()
uint8_t current();
bool hid();                  // el modo actual incluye HID
bool sd();                   // el modo actual incluye MSC/SD
uint16_t pid();              // PID USB del modo actual
char letter();
bool request(uint8_t m);     // pide cambiar (m 0..2); seguro desde cualquier tarea. false si m invalido
void requestNext();          // A->B->C->A
void service();              // desde loop: aplica la peticion (guarda, suelta, reinicia)
}
