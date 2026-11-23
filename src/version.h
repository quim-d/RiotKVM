// Version del firmware (semver) y del protocolo BLE: UNICA fuente de verdad.
// La usan el `info` CDC (`fw=X.Y.Z ... proto=N`), INFO BLE ([0] proto, [1] major, [2] minor, [6] patch) y el CI de
// releases (.github/workflows/release.yml lee las lineas `#define QKVM_... <numero>` con sed: no cambies su formato).
#pragma once

// Empieza en 1.1.0 y no en 1.0.0: los firmwares anteriores al versionado ya anunciaban 1.0 por INFO BLE (patch 0),
// asi que 1.0.0 seria indistinguible de ellos para el comprobador de versiones.
#define QKVM_FW_MAJOR 1
#define QKVM_FW_MINOR 1
#define QKVM_FW_PATCH 0

// Version del contrato BLE de docs/PROTOCOL.md. Solo sube con un cambio incompatible (el cliente rechaza otra).
#define QKVM_BLE_PROTO 1

#define QKVM_STR_(x) #x
#define QKVM_STR(x) QKVM_STR_(x)
#define QKVM_FW_VERSION QKVM_STR(QKVM_FW_MAJOR) "." QKVM_STR(QKVM_FW_MINOR) "." QKVM_STR(QKVM_FW_PATCH)
