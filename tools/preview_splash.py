# Previsualiza offline 3 variantes del splash (160x80, 1,8 s: la animacion se escribe en 1,2 s y va 1,5x mas lenta,
# como UI_SPLASH_MS / UI_SPLASH_ANIM_MS de src/ui_lcd.h) con los assets reales de src/ui_assets.h y la firma "made by
# quim-d" con la fuente TomThumb de LovyanGFX (si esta en .pio/libdeps; si no, se omite).
# Salida: docs/ui/splash_<v>.gif (x4) y docs/ui/splash_<v>_strip.png (tira de frames). Uso: python tools/preview_splash.py
import os, re, random
from PIL import Image, ImageDraw, ImageFont
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
hdr = open(os.path.join(root, "src", "ui_assets.h"), encoding="utf-8").read()
def arr(n):
    m = re.search(r"%s\[\] PROGMEM = \{(.*?)\};" % n, hdr, re.S)
    return [int(x, 16) for x in re.findall(r"0x([0-9A-Fa-f]{2})", m.group(1))]
SK, BN = arr("skull"), arr("bones")
def bit(a, x, y): return (a[y * 8 + x // 8] >> (7 - x % 8)) & 1
W, H = 160, 80
GREEN, DG, AMB, MAG, CY, BONE, GREY, RED = (0,255,70),(0,90,30),(255,176,0),(255,0,200),(0,220,255),(210,255,215),(110,120,110),(255,40,40)
def font(cands, size, fallback):
    for path, idx in cands:
        try: return ImageFont.truetype(path, size, index=idx)
        except Exception: pass
    return fallback
F = font([("C:/Windows/Fonts/consola.ttf", 0), ("/System/Library/Fonts/Menlo.ttc", 0)], 8, ImageFont.load_default())   # Windows o macOS
F2 = font([("C:/Windows/Fonts/consolab.ttf", 0), ("/System/Library/Fonts/Menlo.ttc", 1)], 14, F)
IM = None
SPLASH_MS, ANIM_MS = 2600, 1200
# TomThumb (GFXfont de LovyanGFX): glifos reales para la firma
TT = None
_tt = os.path.join(root, ".pio", "libdeps", "tdongle-s3", "LovyanGFX", "src", "lgfx", "Fonts", "GFXFF", "TomThumb.h")
if os.path.exists(_tt):
    _h = re.sub(r"/\*.*?\*/", "", open(_tt, encoding="utf-8", errors="ignore").read(), flags=re.S)   # sin comentarios (llevan 0x..)
    _bm = [int(x, 16) for x in re.findall(r"0x([0-9A-Fa-f]{2})", re.search(r"TomThumbBitmaps\[\] PROGMEM = \{(.*?)\};", _h, re.S).group(1))]
    _gl = [tuple(int(v) for v in g) for g in re.findall(r"\{\s*(-?\d+),\s*(-?\d+),\s*(-?\d+),\s*(-?\d+),\s*(-?\d+),\s*(-?\d+)\s*\}", re.search(r"TomThumbGlyphs\[\] PROGMEM = \{(.*?)\};", _h, re.S).group(1))]
    TT = (_bm, _gl)
def tiny(d, s, x, y, col):
    """Como drawString con TomThumb y datum top_left (ascendente 5: la linea base queda en y+5)."""
    if not TT: return
    bm, gl = TT
    for ch in s:
        off, w, h, adv, xo, yo = gl[ord(ch) - 0x20]   # esta version: un byte por fila (width 8), MSB a la izquierda
        for yy in range(h):
            for xx in range(w):
                if bm[off + yy] >> (7 - xx) & 1: d.point((x + xo + xx, y + 5 + yo + yy), fill=col)
        x += adv
def made_by(d, y, t):
    if t >= 600: tiny(d, "made by quim-d", W - 2 - 4 * 14, y, GREY)
def big(txt, x, y, col, k=3):
    if not txt: return
    m = Image.new("1", (6 * len(txt), 8), 0); ImageDraw.Draw(m).text((0, -1), txt, fill=1, font=F)
    m = m.resize((m.width * k, 8 * k), Image.NEAREST); IM.paste(Image.new("RGB", m.size, col), (x, y), m)
def typed(d, s, x, y, t, t0, per, col, font=F):
    n = max(0, min(len(s), int((t - t0) / per))) if t >= t0 else 0
    if font is F2: big(s[:n], x, y, col)
    else: d.text((x, y), s[:n], fill=col, font=font)
    return n
def sprite(d, a, ox, oy, col, rows=64, shift=lambda r: 0, clip=None):
    for r in range(rows):
        if clip and not clip(r): continue
        s = shift(r)
        for x in range(64):
            if bit(a, x, r): 
                X = ox + x + s
                if 0 <= X < W: d.point((X, oy + r), fill=col)
def skull(d, ox, oy, rows=64, shift=lambda r: 0, clip=None):
    sprite(d, BN, ox, oy, DG, rows, shift, clip); sprite(d, SK, ox, oy, BONE, rows, shift, clip)
def eyes(d, ox, oy, col, closed=False):
    for ex in (20, 42):
        if closed: d.rectangle((ox+ex-6, oy+23, ox+ex+6, oy+24), fill=col)
        else: d.ellipse((ox+ex-5, oy+23-5, ox+ex+5, oy+23+5), fill=col)
def scan(img):
    px = img.load()
    for y in range(1, H, 2):
        for x in range(W): px[x, y] = tuple(int(c * .72) for c in px[x, y])
    return img

# V1 GLITCH: calavera por bandas con desplazamiento, ojos verde->magenta, titulo con RGB split
def v1(t):
    random.seed(int(t / 40)); im = Image.new("RGB", (W, H)); d = ImageDraw.Draw(im); global IM; IM = im
    rows = 64 if t >= 380 else int(t * 64 / 380); gl = int((520 - t) / 70) + 1 if t < 520 else 0
    offs = [random.randint(-gl, gl) if gl and random.random() < .35 else 0 for _ in range(64)]
    skull(d, 4, 8, rows, lambda r: offs[r])
    if t < 380: d.line((0, 8 + rows, W, 8 + rows), fill=GREEN)
    if t >= 380:
        closed = 760 <= t < 820 or 980 <= t < 1040
        eyes(d, 4, 8, GREEN if t < 700 else MAG, closed)
    if 300 <= t < 800 and random.random() < .3:
        typed(d, "RiotKVM", 75, 12, t, 300, 60, MAG, F2); typed(d, "RiotKVM", 77, 12, t, 300, 60, CY, F2)
    typed(d, "RiotKVM", 76, 12, t, 300, 60, GREEN, F2)
    typed(d, "[ OK ] usb", 76, 36, t, 640, 20, GREEN); typed(d, "[ OK ] hid", 76, 46, t, 780, 20, GREEN)
    typed(d, "[ OK ] ble", 76, 56, t, 920, 20, GREEN); typed(d, "mode C", 76, 68, t, 1050, 30, AMB)
    made_by(d, 1, t)
    return im

# V2 POST/BIOS: lineas de arranque tipo terminal a la izquierda; calavera aparece por tramado ordenado (4 pasos)
BAYER = [[0,8,2,10],[12,4,14,6],[3,11,1,9],[15,7,13,5]]
def v2(t):
    im = Image.new("RGB", (W, H)); d = ImageDraw.Draw(im)
    lines = [("> boot", 0, GREEN), ("[ OK ] usb", 150, GREEN), ("[ OK ] hid", 300, GREEN), ("[ OK ] sd", 450, GREEN), ("[ OK ] ble", 600, GREEN)]
    y = 4
    for s, t0, c in lines:
        typed(d, s, 3, y, t, t0, 14, c); y += 11
    if t >= 750: d.rectangle((3, y + 2, 8, y + 9), fill=GREEN) if (t // 120) % 2 == 0 and t < 1000 else None
    lvl = 0 if t < 450 else min(16, int((t - 450) / 25))     # 0..16 umbral de tramado
    sk = Image.new("RGB", (W, H)); sd = ImageDraw.Draw(sk); skull(sd, 90, 8)
    px, sp = im.load(), sk.load()
    for yy in range(8, 72):
        for xx in range(90, 154):
            if sp[xx, yy] != (0, 0, 0) and BAYER[yy % 4][xx % 4] < lvl: px[xx, yy] = sp[xx, yy]
    if t >= 850: eyes(d, 90, 8, CY if t < 1050 else MAG, 1000 <= t < 1060)
    if t >= 900: typed(d, "RiotKVM", 3, 62, t, 900, 40, AMB)
    made_by(d, 73, t)
    return im

# V3 APERTURA: calavera centrada se abre desde el centro, tibias entran por los lados, flash y desliza a la izq. + titulo
def v3(t):
    random.seed(int(t / 40)); im = Image.new("RGB", (W, H)); d = ImageDraw.Draw(im); global IM; IM = im
    slide = 0 if t < 650 else min(1.0, (t - 650) / 250); ox = int(48 - 44 * slide)
    half = 32 if t >= 320 else int(t * 32 / 320)
    skull(d, ox, 8, 64, clip=None)
    # mascara de apertura: borra todo salvo una franja central
    if half < 32: d.rectangle((0, 0, ox + 32 - half - 1, H), fill=(0, 0, 0)); d.rectangle((ox + 32 + half, 0, W, H), fill=(0, 0, 0))
    if t >= 320:
        col = (255, 255, 255) if t < 400 else GREEN if t < 600 else CY if t < 800 else MAG
        eyes(d, ox, 8, col, 880 <= t < 940)
    if 650 <= t: typed(d, "RiotKVM", 76, 18, t, 780, 45, GREEN, F2); typed(d, "ready_", 76, 44, t, 1040, 40, AMB)
    if 700 <= t < 900 and random.random() < .4: d.line((76, 14 + random.randint(0, 24), 150, 14 + random.randint(0, 24)), fill=DG)
    made_by(d, 73, t)
    return im

out = os.path.join(root, "docs", "ui"); os.makedirs(out, exist_ok=True)
for name, fn in (("v1_glitch", v1), ("v2_bios", v2), ("v3_apertura", v3)):
    ts = [r * ANIM_MS // SPLASH_MS for r in range(0, SPLASH_MS + 1, 40)]   # fotogramas cada 40 ms reales
    fr = [scan(fn(t)).resize((W * 4, H * 4), Image.NEAREST) for t in ts]
    fr[0].save(os.path.join(out, "splash_%s.gif" % name), save_all=True, append_images=fr[1:] + [fr[-1]] * 20, duration=40, loop=0)
    pick = [0, 12, 18, 24, 30, 36, 42, len(ts) - 1]; sel = [scan(fn(ts[i])) for i in pick]
    strip = Image.new("RGB", (W * 4 + 3 * 3, (H + 3) * 2 + 3), (40, 40, 40))
    for k, f in enumerate(sel): strip.paste(f, ((k % 4) * (W + 3), (k // 4) * (H + 3)))
    strip.resize((strip.width * 2, strip.height * 2), Image.NEAREST).save(os.path.join(out, "splash_%s_strip.png" % name))

# ---- pantallas de modo (estaticas): cabecera con acento, icono 1 bit 40x40, filas segun modo ----
def icon(kind, col, col2):
    m = Image.new("1", (40, 40), 0); d = ImageDraw.Draw(m)
    def sdcard(ox, oy, sc=1.0):
        w, h = int(28 * sc), int(36 * sc)
        d.polygon([(ox + 8, oy), (ox + w, oy), (ox + w, oy + h), (ox, oy + h), (ox, oy + 8)], fill=1)
        for k in range(4): d.rectangle((ox + int((4 + k * 5) * sc), oy + int(3 * sc), ox + int((6 + k * 5) * sc), oy + int(10 * sc)), fill=0)
        d.rectangle((ox + int(6 * sc), oy + int(18 * sc), ox + w - int(6 * sc), oy + int(28 * sc)), outline=0)
    def kbd(ox, oy, w=34, h=18):
        d.rounded_rectangle((ox, oy, ox + w, oy + h), 2, fill=1)
        for r in range(3):
            for c in range(7 if w > 30 else 5): d.rectangle((ox + 2 + c * (w - 4) // (7 if w > 30 else 5), oy + 2 + r * 5, ox + 3 + c * (w - 4) // (7 if w > 30 else 5), oy + 3 + r * 5), fill=0)
        d.rectangle((ox + 8, oy + h - 5, ox + w - 8, oy + h - 3), fill=0)
    def mouse(ox, oy):
        d.rounded_rectangle((ox, oy, ox + 14, oy + 22), 6, fill=1); d.line((ox + 7, oy, ox + 7, oy + 8), fill=0); d.line((ox, oy + 9, ox + 14, oy + 9), fill=0)
    if kind == "A": sdcard(6, 2, 1.0); d.rectangle((6, 38, 33, 39), fill=1)
    elif kind == "B": kbd(1, 16); mouse(26, 0) if False else None; mouse(24, 0)
    else: sdcard(0, 0, .6); kbd(1, 22, 30, 16); mouse(26, 6)
    out = Image.new("RGB", (40, 40)); out.paste(Image.new("RGB", (40, 40), col), (0, 0), m); return out

MODES = {"A": ("CARGO", AMB, "SD+USB", [("sd", "[ OK ]", GREEN, "58.3G", CY), ("usb", "[ OK ]", GREEN, "host", CY)]),
         "B": ("BOARDING", CY, "KBD+MOUSE", [("ble", "[ OK ]", GREEN, "linked", MAG), ("usb", "[ OK ]", GREEN, "host", CY), ("hid", "[idle]", GREEN, "", GREEN)]),
         "C": ("FULL", GREEN, "TODO", [("ble", "[ OK ]", GREEN, "linked", MAG), ("sd", "[ OK ]", GREEN, "58.3G", CY), ("usb", "[ OK ]", GREEN, "host", CY), ("hid", "[used]", MAG, "", GREEN)])}
for k, (nm, acc, sub, rows) in MODES.items():
    im = Image.new("RGB", (W, H)); d = ImageDraw.Draw(im); IM = im
    d.rectangle((0, 0, W, 10), fill=acc); d.text((2, 0), "%s %s" % (k, nm), fill=(0, 0, 0), font=F); d.text((W - 2 - 6 * 7, 0), "RiotKVM", fill=(0, 0, 0), font=F)
    d.rectangle((0, 11, 2, H), fill=acc) if False else None
    im.paste(icon(k, acc, MAG), (4, 20))
    d.text((4, 64), sub, fill=acc, font=F)
    y = 15 if len(rows) > 2 else 24; step = 14 if len(rows) > 2 else 18
    for tag, tg, tc, m, mc in rows:
        d.text((52, y), tag, fill=DG, font=F); d.text((76, y), tg, fill=tc, font=F); d.text((118, y), m, fill=mc, font=F); y += step
    d.rectangle((0, 11, W - 1, H - 1), outline=acc) if False else d.line((0, H - 1, W, H - 1), fill=acc)
    scan(im).resize((W * 4, H * 4), Image.NEAREST).save(os.path.join(out, "mode_%s_%s.png" % (k, nm.lower()))) if False else scan(im).resize((W * 4, H * 4), Image.NEAREST).save(os.path.join(root, "docs", "ui", "mode_%s_%s.png" % (k, nm.lower())))
print("ok")
