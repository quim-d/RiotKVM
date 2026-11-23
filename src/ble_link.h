// BLE v1 (ver docs/PROTOCOL.md): servicio GATT propio sobre NimBLE (API C de ESP-IDF).
// El callback de escritura solo copia a una cola; loop() (la misma ruta HID validada) la ejecuta.
#pragma once
#include <Arduino.h>

namespace blelink {
struct Stats { uint32_t writes, records, dropped, malformed, unknown, errsSent, notifies, connects, displaced; };

void begin(TaskHandle_t loopTask);   // arranca NimBLE y el anuncio
void service();                      // desde loop(): ejecuta comandos en cola y envia EVT
int diag(char* buf, size_t n);       // codigos de retorno del arranque (depuracion)
void kick();                         // depuracion: desconecta al cliente actual
bool connected();
uint16_t mtu();
uint16_t itvlUnits();               // intervalo de conexion actual en unidades de 1,25 ms (0 sin cliente)
int paramDiag(char* buf, size_t n);  // intervalo real, rc de la peticion, reintentos (CDC `ble params`)
void setSlowAdv(bool on);
void nameChanged();                  // nombre del dongle cambiado (dev_name): GAP + scan response nuevos, sin reiniciar            // anuncio lento con cliente conectado (A/B)
void setWantItvl(uint16_t minUnits, uint16_t maxUnits);    // intervalo pedido (x1,25 ms), p. ej. 6 = 7,5 ms
const Stats& stats();
void resetStats();
}
