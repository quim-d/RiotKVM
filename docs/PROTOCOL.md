# Protocolo del dongle KVM

Estado: **etapa 1** (solo CDC de texto). El protocolo BLE binario versionado se define en la etapa 2.

## CDC (puerto serie USB, 115200 8N1 nominal; la velocidad es irrelevante)

- Una orden por linea, terminada en `\n` o `\r` (se ignoran lineas vacias, vale `\r\n`). Maximo **255 bytes** por linea: si se supera, se descarta toda la linea y se responde `err overflow`.
- Respuesta: una linea terminada en `\r\n`. Se pueden enviar varias ordenes sin esperar el `ok` (cola RX del dongle: 2048 bytes; se procesan en orden).
- Los enteros son decimales (en `kd`/`ku` tambien `0x..`). Basura o argumentos de mas/menos -> `err args`.
- La salida del dongle es no bloqueante: si el host no lee, las respuestas se descartan (nunca bloquea el HID).

| Orden | Efecto | Respuesta |
|---|---|---|
| `type <texto>` | Escribe texto ASCII (layout en_US). Todo lo que hay tras el primer espacio, incluidos espacios finales. Bytes >= 0x80 o sin tecla se saltan | `ok` / `err nonascii skipped=N` / `err busy` ... |
| `kd <uso>` / `ku <uso>` | Pulsa / suelta un uso HID (1..0xA4 o modificadores 0xE0..0xE7). Fuera de rango -> `err range`; mas de 6 teclas -> `err rollover` | `ok` / `err ...` |
| `kall` | Suelta teclado y botones de ambos ratones | `ok` |
| `mm dx dy [rueda]` | Ratón relativo; se **recorta** a -127..127 | `ok` / `err ...` |
| `ma x y [rueda]` | Ratón absoluto, x,y recortados a 0..32767 (un informe absoluto (0,0) sin botones se envia como x=1: Windows descarta el todo-cero) | `ok` / `err ...` |
| `mbr mask` / `mba mask` | Botones del ratón relativo / absoluto (bits 0..4; > 0x1F -> `err range`) | `ok` / `err ...` |
| `status` | `host=<0/1 enumerado> leds=0xNN hidready=<0/1> susp=<0/1> proto=<boot/report> sd=<0/1> mode=<A/B/C> ble=<0/1> mtu=N ble_itvl_ms=X` | linea |
| `info` | `fw=etapa2 name=QKVM proto=1 mode= rst= crashes= safe= ble_started= [diag BLE] build="..." sd_mb= boot_sd_ms= boot_usb_ms= sd_init_ms= cfg_len= n_itf=` (`proto=` es la version del protocolo BLE; `rst` = esp_reset_reason; `safe=1` = BLE desactivado tras 2 caidas) | linea |
| `stats` / `stats reset` | Contadores (dos lineas): HID (`cmd_*`: desde el evento RX hasta que termina el envio; `snd_*`: duracion de la llamada tud_hid_n_report (envio sin espera, desde la etapa de latencia)) y SD (`sd_*`) | lineas / `ok` |
| `rep` | Diagnostico HID: por ID de informe (1 teclado, 2 raton rel, 3 raton abs) `idN n=<enviados> fail=<fallos> last=<ultimos bytes en hex, sin el ID>`. ID3: byte0 = botones (bits 0..4), luego X (u16 LE), Y (u16 LE), rueda, pan. `stats reset` lo pone a cero | linea |
| `lat` | Latencia interna del dongle en us, UNA linea: `queue n= avg= max= h=b0,..,b7 deliver n= avg= max= h=...`. `queue` = recepcion (evento RX CDC o escritura BLE) -> inicio de ejecucion; `deliver` = recepcion -> fin de la transferencia IN del informe HID (el host ya lo leyo; incluye hasta 1 ms de sondeo USB). Buckets (us): <250,<500,<1000,<2000,<4000,<8000,<16000,>=16000. `stats reset` tambien la pone a cero. Solo cuentan `kd/ku/mm/ma/mbr/mba` por CDC y escrituras BLE (no `type`) | linea |
| `ble params` | Diagnostico de la conexion BLE: `hist(ms:itvl/st)=` cambios de intervalo desde el CONNECT (ms:intervalo/estado HCI, 59 = parametros inaceptables rechazados por el central), `itvl_ms lat want_ms upd_rc upd_tries upd_events slow_adv` | linea |
| `ble want MIN [MAX]` | Intervalo de conexion pedido (unidades de 1,25 ms; 6 = 7,5 ms) | `ok` / `err range` |
| `ble slowadv 0\|1` | Anuncio lento (400-500 ms) mientras hay cliente (por defecto 1) | `ok` |
| `mode [a\|b\|c]` | Sin argumento: `mode=X`. Con argumento: `ok` y el dongle guarda el modo en NVS, suelta todo y se reinicia (otro PID, otro COM) | `ok` |
| `reboot` | Suelta todo y reinicia | `ok` |
| `heap`, `log`, `coredump` | Depuracion: memoria libre, trazas ESP_LOG en RTC, resumen del ultimo panic (pc + backtrace, decodificar con addr2line) | lineas |
| `ble kick` / `ble start` | Depuracion: desconecta al cliente BLE / arranca BLE si estaba en safe | `ok` |
| `bootloader` | Suelta todo, responde `ok` y reinicia en modo descarga ROM (VID_303A PID_0009) | `ok` |

Errores: `err args`, `err range`, `err overflow`, `err unknown`, `err busy` (endpoint ocupado o host suspendido que no despierta; el informe se perdio), `err nohost` (el host no ha enumerado el dongle), `err bootproto` (el host esta en boot protocol/BIOS: no se envian informes de raton), `err rollover`, `err nonascii skipped=N`.

Soltado automatico: el dongle suelta teclas y botones (y reenvia el estado vacio) al **enumerar, desenumerar, suspender, reanudar** el bus y cuando el cliente **baja DTR** (cierra el puerto). Si un informe de teclado/boton falla, se reintenta hasta que el bus lo admita.

Remote wakeup: el descriptor declara remote wakeup; si el host esta suspendido, una orden HID lo despierta antes de enviar.

## Orden de arranque (importante para latencia)
Serial -> init SD -> MSC -> HID -> `USB.begin()` -> pantalla/LED.

---

# BLE v1 (etapa 2) - CONTRATO

Version de protocolo: **1**. Los valores de esta seccion son estables; cualquier cambio se avisa en `docs/handoff/firmware-dev.md` y sube el byte de version de INFO.

## Conexion
- Rol: el dongle es periferico GATT. Anuncia **de forma permanente (tambien con un cliente conectado, para que otro pueda desplazarlo)**: en los datos de anuncio el UUID de servicio (128 bit) y en el scan response el nombre `QKVM` (antes `QKVM`; el UUID no cambia, el cliente busca por UUID) (tambien es el nombre GAP). Conectable, sin filtro.
- **Just Works, sin passkey y sin emparejamiento ni cifrado obligatorios** (decision del dueño: inseguro a proposito). Cualquier cliente puede escribir/leer sin bonding.
- **Un solo cliente**: si se conecta otro, el dongle desconecta al anterior (suelta teclas y botones) y acepta al nuevo. Al desconectar, el dongle hace `releaseAll` y vuelve a anunciar.
- MTU: el dongle acepta hasta **247** (el cliente inicia el intercambio MTU). Maximo por escritura = MTU-3 (244 con MTU 247; 20 si no se negocia).
- Parametros de conexion: el dongle solicita intervalo 7,5-15 ms, latencia 0, timeout de supervision 4 s. El central puede ignorarlos (Windows suele conceder ~7,5-15 ms; macOS minimo 11,25 ms).
- El cliente debe **suscribirse a EVT** (CCCD) para recibir estado; el dongle envia STATE al suscribirse.

## UUIDs
| Elemento | UUID | Propiedades |
|---|---|---|
| Servicio | `7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0001` | |
| CMD | `7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0002` | **Write Without Response** (cliente -> dongle) |
| EVT | `7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0003` | **Notify** (dongle -> cliente) |
| INFO | `7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0004` | **Read** |

## Formato de trama (CMD y EVT)
Una escritura (o notificacion) es una **secuencia de registros** concatenados, sin cabecera global:

```
[op:u8][len:u8][payload:len bytes]   [op][len][payload] ...
```
- Enteros multibyte en **little endian**. `len` es la longitud del payload (0..255).
- Se pueden agrupar tantos registros como quepan en MTU-3. Se ejecutan **en orden**.
- Registro con `len` que excede el fin de la escritura: se descarta ese registro y el resto de la escritura (y se notifica ERR `MALFORMED`).
- `op` desconocido: se **salta** usando `len` (compatibilidad hacia delante) y se notifica ERR `UNKNOWN_OP` (limitado a ~20/s).
- Sin respuesta por registro: el exito no se confirma. Los fallos se notifican con ERR.

## CMD: cliente -> dongle
| op | Nombre | len | Payload | Efecto |
|---|---|---|---|---|
| 0x01 | KEY_DOWN | 1 | `usage:u8` | Pulsa un uso HID de teclado (1..0xA4 o modificadores 0xE0..0xE7) |
| 0x02 | KEY_UP | 1 | `usage:u8` | Suelta ese uso |
| 0x03 | RELEASE_ALL | 0 | - | Suelta teclas y botones de ambos ratones |
| 0x04 | MOUSE_REL | 2 o 4 | `dx:i8, dy:i8 [, wheel:i8, pan:i8]` | Movimiento relativo (-127..127; -128 se trata como -127) |
| 0x05 | MOUSE_ABS | 4 o 6 | `x:u16, y:u16 [, wheel:i8, pan:i8]` | Posicion absoluta 0..32767 (valores mayores se recortan) |
| 0x06 | BUTTONS_REL | 1 | `mask:u8` | Estado de botones del raton relativo (bit0 izq, bit1 der, bit2 medio, bit3 atras, bit4 adelante; mask > 0x1F -> ERR RANGE) |
| 0x07 | BUTTONS_ABS | 1 | `mask:u8` | Igual, para el raton absoluto |
| 0x08 | WHEEL | 2 | `wheel:i8, pan:i8` | Rueda/pan por el raton relativo (sin mover) |
| 0x10 | PING | 2 | `seq:u16` | Responde EVT PONG con el mismo `seq` |
| 0x11 | GET_STATE | 0 | - | Responde EVT STATE |
| 0x12 | SET_MODE | 1 | `mode:u8` (0=A, 1=B, 2=C) | Cambia el modo USB (ver abajo): guarda en NVS, suelta todo y **reinicia el dongle** (USB se re-enumera con otro PID y la conexion BLE se corta; el cliente debe reconectar) |

El estado de teclas/botones es **persistente en el dongle** (KEY_DOWN deja la tecla pulsada hasta KEY_UP/RELEASE_ALL/desconexion). Maximo 6 teclas no modificadoras a la vez.

Modos: **A** = solo SD (MSC), **B** = solo teclado+ratones HID, **C** = ambos. En modo A los comandos HID fallan con ERR `NO_HID`; en B, no hay SD. Cada modo tiene un PID USB distinto (A=0x0010, B=0x0011, C=0x0001). SET_MODE se guarda en NVS. Tambien se cambia con el boton BOOT (pulsacion corta).

## EVT: dongle -> cliente (notificaciones)
| op | Nombre | len | Payload |
|---|---|---|---|
| 0x81 | STATE | 3 | `flags:u8, leds:u8, mode:u8` |
| 0x82 | PONG | 6 | `seq:u16, t_us:u32` (microsegundos del dongle desde el arranque, tomados al procesar el ping) |
| 0x83 | ERR | 2 | `op:u8, code:u8` |

`flags` de STATE: bit0 = host USB enumerado, bit1 = host USB suspendido, bit2 = SD presente (lista), bit3 = host en boot protocol (BIOS), bit4 = HID disponible en el modo actual, bit5 = SD disponible en el modo actual. `leds`: bits del informe LED del teclado (bit0 NumLock, bit1 CapsLock, bit2 ScrollLock). `mode`: 0=A, 1=B, 2=C.
STATE se envia al suscribirse, ante GET_STATE y **cada vez que cambia** algo de lo anterior (agrupado, como mucho cada ~20 ms).
Codigos ERR: 1 `BUSY` (informe perdido: endpoint ocupado/host suspendido que no despierta), 2 `RANGE`, 3 `UNKNOWN_OP`, 4 `MALFORMED`, 5 `NO_HOST` (USB no enumerado), 6 `NO_HID` (modo sin HID), 7 `BOOT_PROTO` (raton en boot protocol), 8 `ROLLOVER` (mas de 6 teclas).

## INFO (lectura, 8 bytes)
```
[0] proto_version = 1
[1] fw_major   [2] fw_minor
[3] mode       (0=A,1=B,2=C)
[4..5] caps u16: bit0 teclado, bit1 raton rel, bit2 raton abs, bit3 MSC/SD, bit4 SET_MODE, bit5 PING
[6..7] reservado (0)
```
Se puede leer en cualquier momento (no hace falta suscribirse).

## Orden de peticiones recomendado para el cliente
1. Escanear por UUID de servicio (o nombre `QKVM` (antes `QuimKVM`; el UUID no cambia, el cliente busca por UUID)); conectar; negociar MTU (247).
2. Descubrir servicio; leer INFO (comprobar `proto_version == 1`); suscribirse a EVT.
3. Enviar CMD con Write Without Response; agrupar movimientos de raton en una escritura por intervalo de conexion.
4. Reconexion automatica: si se pierde el enlace, volver a escanear/conectar (el dongle ya anuncia).

## Orden CDC y BLE
`bootloader`, `stats`, `info`... son solo CDC. BLE no expone ninguna orden peligrosa. CDC sigue siendo el canal de depuracion (protocolo de texto de arriba), tambien funciona en paralelo.

## Notas de implementacion BLE (medido en este dongle, Windows 11 + bleak)
- La libreria precompilada trae NimBLE con anuncio extendido: se usa la instancia 0 con PDU legacy (ADV_IND + SCAN_RSP). UUID de servicio en el PDU principal, nombre en el scan response.
- El PID depende del modo (C=0x0001, B=0x0011, A=0x0010) y **Windows asigna un COM distinto por PID**: el cliente debe localizar el CDC por VID/PID, no por numero de COM.
- Un cliente BLE puede pedir MTU 247; con Windows se obtuvo MTU 247 e intervalo de conexion 15,00 ms.
