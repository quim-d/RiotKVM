import sys
# Genera packaging/macos/QKVM.iconset desde dkvm-gui/assets/skull_64.png (calavera pixel-art sobre placa negra con borde verde).
# Uso: python3 tools/gen_macos_icon.py client/dkvm-gui/assets/skull_64.png QKVM.iconset && iconutil -c icns QKVM.iconset -o packaging/macos/QKVM.icns
from PIL import Image, ImageDraw
src = Image.open(sys.argv[1]).convert("RGBA"); out = sys.argv[2]
def icon(n):
    c = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    # placa macOS: 824 px con esquinas de radio ~185, fondo negro y borde verde de la paleta
    d = ImageDraw.Draw(c); o = 100
    d.rounded_rectangle((o, o, 1024 - o, 1024 - o), radius=185, fill=(0, 0, 0, 255), outline=(0, 255, 70, 255), width=12)
    sk = src.resize((640, 640), Image.NEAREST)
    c.alpha_composite(sk, (192, 180))
    return c.resize((n, n), Image.LANCZOS if n < 1024 else Image.NEAREST)
import os
os.makedirs(out, exist_ok=True)
for b in (16, 32, 128, 256, 512):
    icon(b).save(f"{out}/icon_{b}x{b}.png"); icon(b * 2).save(f"{out}/icon_{b}x{b}@2x.png")
