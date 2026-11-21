"""Exporta calavera e iconos de modo desde src/ui_assets.h a docs/ui/assets/ (PNG RGBA transparente + .ico)."""
import re, os
from PIL import Image, ImageDraw
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
src = open(os.path.join(root, "src", "ui_assets.h")).read()
out = os.path.join(root, "docs", "ui", "assets"); os.makedirs(out, exist_ok=True)
def arr(n):
    m = re.search(r"%s\[\] PROGMEM = \{(.*?)\};" % n, src, re.S)
    return bytes(int(x, 16) for x in re.findall(r"0x([0-9A-Fa-f]{2})", m.group(1)))
def bm(data, w, h, col, img):
    bpr = (w + 7) // 8
    px = img.load()
    for y in range(h):
        for x in range(w):
            if data[y * bpr + x // 8] & (0x80 >> (x % 8)): px[x, y] = col
BONE, DG, GR = (210,255,215,255), (0,90,30,255), (0,255,70,255)
AM, CY = (255,176,0,255), (0,220,255,255)
sk = Image.new("RGBA", (64, 64), (0,0,0,0))
bm(arr("bones"), 64, 64, DG, sk); bm(arr("skull"), 64, 64, BONE, sk)
d = ImageDraw.Draw(sk)
for cx in (20, 42):   # orbitas en verde fosforo (como el LCD)
    d.ellipse([cx-5, 23-5, cx+5, 23+5], fill=GR)
def save(img, name, sizes):
    for s in sizes:
        img.resize((s, s), Image.NEAREST).save(os.path.join(out, "%s_%d.png" % (name, s)))
# 16/32/48 no son multiplos enteros de 64: reescalar con BOX para que no salga roto; 64 y 256 (x4) nitidos
for s in (16, 32, 48, 64, 256):
    r = Image.NEAREST if s in (64, 256) else Image.BOX
    sk.resize((s, s), r).save(os.path.join(out, "skull_%d.png" % s))
sk.resize((256, 256), Image.NEAREST).save(os.path.join(out, "skull.ico"), sizes=[(16,16),(32,32),(48,48),(256,256)])
for n, k, c in (("A_cargo", "iconA", AM), ("B_boarding", "iconB", CY), ("C_full", "iconC", GR)):
    im = Image.new("RGBA", (40, 40), (0,0,0,0)); bm(arr(k), 40, 40, c, im)
    for s in (40, 80, 160):
        im.resize((s, s), Image.NEAREST).save(os.path.join(out, "mode_%s_%d.png" % (n, s)))
