# Genera src/ui_assets.h (calavera pirata + tibias cruzadas, 1 bit) y docs/ui/skull_preview.png.
# Dibujo propio con primitivas sin antialiasing. Uso: python tools/gen_skull.py
import os, math
from PIL import Image, ImageDraw, ImageChops
W = H = 64
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def new(): return Image.new("1", (W, H), 0)

# --- calavera ---
sk = new(); d = ImageDraw.Draw(sk)
d.ellipse((9, 1, 54, 41), fill=1)                 # craneo
d.rounded_rectangle((19, 32, 44, 47), 4, fill=1)  # mandibula
d.ellipse((13, 15, 28, 31), fill=0)               # orbitas
d.ellipse((35, 15, 50, 31), fill=0)
d.polygon([(32, 30), (28, 37), (36, 37)], fill=0) # nariz
d.line((19, 33, 24, 33), fill=0); d.line((39, 33, 44, 33), fill=0)  # sien/pomulo
for x in (24, 28, 32, 36, 40): d.line((x, 41, x, 47), fill=0)       # dientes
d.line((20, 41, 43, 41), fill=0)
d.point([(31, 5), (32, 4), (33, 4), (34, 5)], fill=0)               # brillo (hueco) en la frente
d.point([(14, 9), (15, 8), (16, 7)], fill=0)

# --- tibias ---
bn = new(); d = ImageDraw.Draw(bn)
def bone(x0, y0, x1, y1):
    d.line((x0, y0, x1, y1), fill=1, width=5)
    ang = math.atan2(y1 - y0, x1 - x0); nx, ny = -math.sin(ang), math.cos(ang)
    for (ex, ey) in ((x0, y0), (x1, y1)):
        for s in (-1, 1):
            cx, cy = ex + nx * 3.2 * s, ey + ny * 3.2 * s
            d.ellipse((cx - 2.6, cy - 2.6, cx + 2.6, cy + 2.6), fill=1)
bone(5, 44, 58, 61); bone(58, 44, 5, 61)
# hueco negro alrededor de la calavera para que se lea
halo = sk.copy()
for _ in range(2): halo = halo.filter(__import__("PIL.ImageFilter", fromlist=["x"]).MaxFilter(3))
bn = ImageChops.subtract(bn, halo)

def pack(im):
    out = []
    for y in range(H):
        for xb in range(0, W, 8):
            v = 0
            for b in range(8):
                v = (v << 1) | (1 if im.getpixel((xb + b, y)) else 0)
            out.append(v)
    return out

def arr(name, data):
    s = "static const uint8_t %s[] PROGMEM = {\n" % name
    for i in range(0, len(data), 16):
        s += "  " + ",".join("0x%02X" % b for b in data[i:i+16]) + ",\n"
    return s + "};\n"


# --- iconos de modo 40x40 (A: SD, B: teclado+raton, C: todo). Misma geometria que tools/preview_splash.py ---
def iconmask(kind):
    m = Image.new("1", (40, 40), 0); d = ImageDraw.Draw(m)
    def sdcard(ox, oy, sc=1.0):
        w, h = int(28 * sc), int(36 * sc)
        d.polygon([(ox + 8, oy), (ox + w, oy), (ox + w, oy + h), (ox, oy + h), (ox, oy + 8)], fill=1)
        for k in range(4): d.rectangle((ox + int((4 + k * 5) * sc), oy + int(3 * sc), ox + int((6 + k * 5) * sc), oy + int(10 * sc)), fill=0)
        d.rectangle((ox + int(6 * sc), oy + int(18 * sc), ox + w - int(6 * sc), oy + int(28 * sc)), outline=0)
    def kbd(ox, oy, w=34, h=18):
        n = 7 if w > 30 else 5
        d.rounded_rectangle((ox, oy, ox + w, oy + h), 2, fill=1)
        for r in range(3):
            for c in range(n): d.rectangle((ox + 2 + c * (w - 4) // n, oy + 2 + r * 5, ox + 3 + c * (w - 4) // n, oy + 3 + r * 5), fill=0)
        d.rectangle((ox + 8, oy + h - 5, ox + w - 8, oy + h - 3), fill=0)
    def mouse(ox, oy):
        d.rounded_rectangle((ox, oy, ox + 14, oy + 22), 6, fill=1); d.line((ox + 7, oy, ox + 7, oy + 8), fill=0); d.line((ox, oy + 9, ox + 14, oy + 9), fill=0)
    if kind == "A": sdcard(6, 2, 1.0); d.rectangle((6, 38, 33, 39), fill=1)
    elif kind == "B": kbd(1, 16); mouse(24, 0)
    else: sdcard(0, 0, .6); kbd(1, 22, 30, 16); mouse(26, 6)
    return m
def pack40(im):
    out = []
    for y in range(40):
        for xb in range(0, 40, 8):
            v = 0
            for b in range(8): v = (v << 1) | (1 if im.getpixel((xb + b, y)) else 0)
            out.append(v)
    return out

hdr = "// GENERADO por tools/gen_skull.py - no editar a mano. Bitmaps 1 bit, MSB primero, %dx%d, %d bytes por fila.\n#pragma once\n#include <Arduino.h>\nnamespace uiasset {\nstatic constexpr int SKULL_W = %d, SKULL_H = %d;\n" % (W, H, W // 8, W, H)
hdr += "// centros de las orbitas (para el brillo de los ojos) y radio\nstatic constexpr int EYE_L_X = 20, EYE_R_X = 42, EYE_Y = 23, EYE_R = 6;\n"
hdr += arr("skull", pack(sk)) + arr("bones", pack(bn))
hdr += "static constexpr int ICON_W = 40, ICON_H = 40;   // 5 bytes por fila\n" + arr("iconA", pack40(iconmask("A"))) + arr("iconB", pack40(iconmask("B"))) + arr("iconC", pack40(iconmask("C")))
hdr += "}  // namespace uiasset\n"
open(os.path.join(root, "src", "ui_assets.h"), "w", newline="\n").write(hdr)

prev = Image.new("RGB", (W, H), (0, 0, 0))
prev.paste((60, 120, 60), mask=bn); prev.paste((210, 255, 210), mask=sk)
pd = ImageDraw.Draw(prev)
for cx in (20, 42): pd.ellipse((cx-4, 19, cx+4, 27), fill=(255, 0, 200))
prev.resize((W * 8, H * 8), Image.NEAREST).save(os.path.join(root, "docs", "ui", "skull_preview.png"))
print("ok")
