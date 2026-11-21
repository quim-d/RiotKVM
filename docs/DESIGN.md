# QKVM - identidad visual (fuente única)

Origen: `src/ui_lcd.cpp` y `src/ui_assets.h` (LCD 80x160). La GUI del cliente adopta lo mismo. Si cambia algo, se cambia aquí y en `ui_lcd.cpp`.

## 1. Paleta
| Nombre | Uso | RGB565 | Hex |
|---|---|---|---|
| NEGRO | fondo (siempre) | 0000 | #000000 |
| VERDE (fósforo) | color principal, texto, marcos | 07E8 | #00FF46 |
| VERDE_OSC | tibias, decoración, scanlines (no texto) | 02C3 | #005A1E |
| ÁMBAR | modo A, avisos, `[ .. ]`, `[ zz ]` | FD80 | #FFB000 |
| CIAN | modo B, valores/datos | 06FF | #00DCFF |
| MAGENTA | 2º acento: BLE enlazado, actividad HID | F819 | #FF00C8 |
| ROJO | error (`[FAIL]`, SD ausente) | F945 | #FF2828 |
| HUESO | cráneo, texto destacado | D7FA | #D2FFD7 |
| GRIS | texto secundario, bordes inactivos | 6BCD | #6E786E |
| BLANCO | solo flash del splash | FFFF | #FFFFFF |

## 2. Color por modo
A **CARGO** (solo almacenamiento) = ámbar. B **BOARDING** (teclado+ratón) = cian. C **FULL PIRATE** (ambos; en el LCD se muestra "FULL") = verde. Magenta nunca es color de modo: es "enlazado/actividad".

## 3. Nombres y etiquetas
- Marca: `QKVM` (mayúsculas, en el LCD en tamaño 2-3). Lemas opcionales: "no cure, no pay", "jolly roger OS".
- Estados, siempre 6 caracteres: `[ OK ]` verde, `[ .. ]` ámbar (esperando), `[FAIL]` rojo, `[ zz ]` ámbar (suspendido), `[idle]` verde / `[used]` magenta (HID).
- Filas de estado: `usb` (host / wait / sleep), `hid`, `sd` (tamaño, p. ej. `59.6G`, o `no card`), `ble` (`linked` magenta / `scan` ámbar).
- Cambio de modo: marco ámbar + `! CAMBIANDO RUMBO...`. Prompt: `> `, cursor bloque `_`.

## 4. Iconos y calavera (1 bit, en `docs/ui/assets/`, fondo transparente)
Generados con `python tools/export_assets.py` desde `ui_assets.h` (no editar a mano).
- Calavera (hueso D2FFD7 + tibias 005A1E + órbitas verdes): `skull_16/32/48/64/256.png` y `skull.ico` (16/32/48/256) para ventana/instalador. Origen 64x64; 64 y 256 son escalado entero (nítido), 16/32/48 son reducción suavizada.
- Iconos de modo 40x40 (y x2, x4 = 80/160): `mode_A_cargo_*.png` (ámbar), `mode_B_boarding_*.png` (cian), `mode_C_full_*.png` (verde).
- Escalar siempre con vecino más próximo (sin suavizar) salvo los tamaños pequeños ya exportados.

## 5. Tipografía
Monoespaciada, sin suavizado artístico. LCD: bitmap Font0 6x8 de LovyanGFX (tamaño 1/2/3). GUI: recomendado **JetBrains Mono** embebida (SIL OFL 1.1, libre, redistribuible con el instalador; ~270 KB por peso, con 1 peso Regular y 1 Bold basta). Alternativa sin embebido: Cascadia Mono/Consolas (Windows), DejaVu Sans Mono (Linux, licencia libre Bitstream Vera). Tamaño de cuerpo 10-12 pt.

## 6. Reglas
- Fondo siempre negro; texto verde/hueso. Contraste mínimo 4.5:1 sobre negro para texto: GRIS (~4.6:1) es el límite inferior; VERDE_OSC (~2.6:1) solo decoración, nunca texto.
- Bordes: 1 px, verde (activo) o gris (inactivo); aviso = ámbar; sin esquinas redondeadas ni degradados ni sombras.
- Espaciado: rejilla de 8 px en la GUI (1 carácter = 1 celda del LCD); margen 4-8 px; una fila de estado = una línea de texto.
- Barras de estado: prefijo de 6 caracteres (`[ OK ] usb`), etiqueta en minúsculas.
- Animación: solo los eventos puntuales: splash (~1,2 s), parpadeo de ojos, cursor, aviso de modo, destello de actividad HID. **Nada continuo**: sin scroll, sin respiración, sin lluvia Matrix permanente; la pantalla en reposo está quieta (el LCD se atenúa por inactividad).
- Scanlines: solo decorativas y estáticas, VERDE_OSC al <=20 %.

## 7. Equivalencias LCD <-> GUI
| LCD | GUI |
|---|---|
| Cabecera/icono con color de modo (ámbar/cian/verde) + nombre | Franja superior de 1 línea con el color del modo, icono de modo 40/80 px, nombre en negrita |
| Filas `[ OK ] usb / hid / sd / ble` | Barra de estado inferior o panel lateral, mismas 4 etiquetas, mismos colores |
| Actividad HID `[used]` magenta | Indicador magenta que se apaga solo (sin contenido de teclas) |
| Mini-calavera (splash/ojos) | `skull.ico` como icono de ventana/instalador y `skull_32/64` en cabecera; ojos magenta = BLE enlazado (opcional) |
| `! CAMBIANDO RUMBO...` marco ámbar | Diálogo/banner ámbar durante el cambio de modo |
| Atenuación por inactividad | Sin equivalente; no atenuar la GUI |
