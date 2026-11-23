# quim-dkvm

KVM de bolsillo sobre un **LilyGO T-Dongle-S3 (ESP32-S3)**. El dongle se enchufa al servidor y ofrece:

- **Teclado y ratón USB HID** (ratón relativo y absoluto), controlados por **Bluetooth LE** desde un portátil.
- **microSD como pendrive USB** (USB MSC) para Ventoy, ISOs y herramientas.
- **Tres modos** con el botón BOOT, guardados en memoria no volátil (NVS): `A` solo almacenamiento, `B` teclado+ratón, `C` ambos.
- **Pantalla LCD** (ST7735 80x160) con arranque y estado por modo.
- Puerto serie CDC para depuración y control.

Junto con una capturadora HDMI→USB en el portátil, es un "RDP de bolsillo" para configurar BIOS y sistemas.

> El BLE es **abierto a propósito** (Just Works, sin PIN, reconexión automática): cualquiera dentro del alcance puede controlar el teclado y el ratón. Úsalo en entornos de confianza.

## Contenido

| Ruta | Qué es |
|---|---|
| `src/` | Firmware (Arduino-ESP32 4.0 / pioarduino 61.04.00, TinyUSB, NimBLE, LovyanGFX). Versión en `src/version.h` |
| `variants/tdongle_s3/` | Variante de placa con VID/PID propios (1209:0001) |
| `platformio.ini` | Proyecto PlatformIO |
| `tools/flash.py` | Flashea un `firmware.factory.bin`: envía `bootloader` por CDC y escribe con `esptool` |
| `tools/*_skull.py`, `tools/preview_splash.py` | Generación de los bitmaps del arranque y previsualizaciones |
| `docs/PROTOCOL.md` | Protocolo CDC y contrato BLE v1 |
| `docs/ui/` | Previsualizaciones de la pantalla |

## Compilar y flashear

```powershell
python -m platformio run
python tools\flash.py .pio\build\tdongle-s3\firmware.factory.bin
```

El script pone el dongle en modo descarga con el comando serie `bootloader` y flashea con `esptool` (el error final de pySerial es normal: el puerto desaparece al reiniciar). Si el dongle no responde, enchúfalo con el botón BOOT pulsado y usa `python -m esptool --port COMx --after watchdog-reset write-flash 0x0 .pio\build\tdongle-s3\firmware.factory.bin`.

### Versión del firmware

La versión (semver) está **solo** en `src/version.h` (`QKVM_FW_MAJOR/MINOR/PATCH`); el dongle la publica en `info` por CDC (`fw=X.Y.Z`) y en INFO por BLE. Es independiente de la del cliente (`client/Cargo.toml`). Al etiquetar `vX.Y.Z`, el CI compila el firmware y lo adjunta a la release como `qkvm-fw-<versión>.factory.bin` (el mismo `firmware.factory.bin` que usa `tools/flash.py`), junto con `manifest.json`: versión del cliente, versión del firmware, `proto`, nombre del asset y su SHA-256.

## BLE

Nombre `QKVM`. Servicio `7d0d1000-9b6e-4b7a-8d3f-2a5c6e1f0001`; características CMD (`…0002`, escritura sin respuesta), EVT (`…0003`, notificación) e INFO (`…0004`, lectura). Detalle en `docs/PROTOCOL.md`.

## Estado

Proyecto en desarrollo. El cliente de escritorio (Rust, CLI + GUI) todavía no se publica aquí.
