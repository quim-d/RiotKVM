"""Genera el tema GRUB2 de Ventoy con la estetica QKVM (docs/DESIGN.md) en packaging/ventoy-theme/.

Salida (lo que dkvm-flash copia a /ventoy/theme/ de la particion exFAT):
  qkvm/   theme_<WxH>_<n>.txt + bg_<WxH>_<n>.png (una por consigna del pie), pixmaps 9-slice de 1 px e icons/
  fonts/  JetBrains Mono en .pf2 (solo con --fonts: necesita grub-mkfont) + OFL.txt

Ventoy elige entre los theme_*.txt los que llevan la resolucion activa en la ruta ("resolution_fit": 1) y, entre
ellos, uno al azar en cada arranque: asi rota la consigna del pie sin nada animado en pantalla.

Cada variante se dibuja a su resolucion exacta (GRUB 2.04 estira el desktop-image sin respetar el aspecto):
calavera y cabecera con escalado entero y sin suavizado, fondo negro, bordes de 1 px, sin degradados.

Uso (en un Debian con python3-pil, grub-common y fonts-jetbrains-mono; p. ej. en Docker/OrbStack):
  python3 tools/gen_ventoy_theme.py [--fonts]
"""
import os, re, subprocess, sys
from PIL import Image, ImageDraw, ImageFont

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(root, "packaging", "ventoy-theme")
TTF_DIR = os.environ.get("JBM_DIR", "/usr/share/fonts/truetype/jetbrains-mono")

# paleta (docs/DESIGN.md)
NEGRO, VERDE, VERDE_OSC, AMBAR = "#000000", "#00FF46", "#005A1E", "#FFB000"
CIAN, MAGENTA, ROJO, HUESO, GRIS = "#00DCFF", "#FF00C8", "#FF2828", "#D2FFD7", "#6E786E"

def rgb(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))

# --- calavera y tibias: los mismos bitmaps del LCD (src/ui_assets.h, generados por tools/gen_skull.py) ---
hdr = open(os.path.join(root, "src", "ui_assets.h"), encoding="utf-8").read()
def arr(n):
    m = re.search(r"%s\[\] PROGMEM = \{(.*?)\};" % n, hdr, re.S)
    return [int(x, 16) for x in re.findall(r"0x([0-9A-Fa-f]{2})", m.group(1))]

def skull_img():
    im = Image.new("RGB", (64, 64), rgb(NEGRO))
    px = im.load()
    for name, col in (("bones", VERDE_OSC), ("skull", HUESO)):
        a = arr(name)
        for y in range(64):
            for x in range(64):
                if a[y * 8 + x // 8] & (0x80 >> (x % 8)):
                    px[x, y] = rgb(col)
    d = ImageDraw.Draw(im)
    for cx in (20, 42):  # orbitas en verde fosforo, como el LCD y skull_*.png
        d.ellipse([cx - 5, 18, cx + 5, 28], fill=rgb(VERDE))
    return im

# --- iconos de clase de menu (8x8, 1 bit, escalado entero) ---
ICONS = {
    # todo lo que hay en /Tools (menu_class "parent": "/Tools"): cruz de rescate
    "qkvm-rescate": (AMBAR, ["...##...", "...##...", "...##...", "########", "########", "...##...", "...##...", "...##..."]),
}

def icon(rows, col, k):
    im = Image.new("RGBA", (8, 8), (0, 0, 0, 0))
    px = im.load()
    for y, r in enumerate(rows):
        for x, c in enumerate(r):
            if c == "#":
                px[x, y] = rgb(col) + (255,)
    return im.resize((8 * k, 8 * k), Image.NEAREST)

def solid(path, col, w=1, h=1):
    Image.new("RGB", (w, h), rgb(col)).save(path, optimize=True)

def nine(dirn, prefix, border, center):
    """Pixmap 9-slice de GRUB con borde de 1 px."""
    for s in ("nw", "n", "ne", "e", "se", "s", "sw", "w"):
        solid(os.path.join(dirn, "%s_%s.png" % (prefix, s)), border)
    solid(os.path.join(dirn, "%s_c.png" % prefix), center)

def text1(d, xy, s, font, col):
    """Texto sin suavizado (mascara de 1 bit), como pide DESIGN.md."""
    d.fontmode = "1"
    d.text(xy, s, font=font, fill=rgb(col))

def ttf(bold, size):
    return ImageFont.truetype(os.path.join(TTF_DIR, "JetBrainsMono-%s.ttf" % ("Bold" if bold else "Regular")), size)

# Nombres de las fuentes .pf2 (grub-mkfont: "<familia> <estilo> <tamaño>")
def fname(bold, size):
    return "JetBrains Mono %s %d" % ("Bold" if bold else "Regular", size)

# Maquetas por resolucion (px). u = celda de la rejilla de 8 px.
LAYOUTS = {
    (1920, 1080): dict(k=3, margin=96, top=40, sub=28, rule=264, menu_top=288, menu_h=600, item=24,
                       item_h=40, icon=3, small=16, tbar=916, tlabel=930, modes=974, tip=1016, ver="80%@95%"),
    (1024, 768): dict(k=2, margin=32, top=24, sub=16, rule=168, menu_top=184, menu_h=448, item=16,
                      item_h=28, icon=2, small=16, tbar=644, tlabel=654, modes=686, tip=724, ver="760@4"),
}
FONT_SIZES = sorted({(False, L["item"]) for L in LAYOUTS.values()} | {(True, L["item"]) for L in LAYOUTS.values()} | {(False, 16)})

# Lema de la cabecera (fijo) y citas del pie (rotan entre arranques). Citas reales, con autoria.
LEMA = "si no nos dejan soñar, no les dejaremos dormir"
PIE = [
    "la emancipación de los trabajadores será obra de los trabajadores mismos — AIT, 1864",
    "la anarquía es la más alta expresión del orden — Élisée Reclus",
    "Tierra y Libertad — Ricardo Flores Magón",
    "llevamos un mundo nuevo en nuestros corazones — Durruti",
]

def background(W, H, L, pie):
    im = Image.new("RGB", (W, H), rgb(NEGRO))
    d = ImageDraw.Draw(im)
    k, m, t = L["k"], L["margin"], L["top"]
    sk = skull_img().resize((64 * k, 64 * k), Image.NEAREST)
    im.paste(sk, (m, t))
    x = m + 64 * k + 4 * 8
    # "QKVM" con la tinta alineada arriba con la calavera y de la mitad de su alto
    hk = 64 * k // 2
    size = 8
    while True:
        b = ttf(True, size + 1).getbbox("QKVM")
        if b[3] - b[1] > hk:
            break
        size += 1
    f = ttf(True, size)
    b = f.getbbox("QKVM")
    text1(d, (x - b[0], t + 4 - b[1]), "QKVM", f, VERDE)
    fs = ttf(False, L["sub"])
    sb = fs.getbbox("Mg")
    line = int((sb[3] - sb[1]) * 1.6)
    y2 = t + 64 * k - 2 * line
    text1(d, (x, y2 - sb[1]), LEMA, fs, GRIS)
    text1(d, (x, y2 + line - sb[1]), "> elige una imagen_", fs, VERDE)
    # regla de 1 px bajo la cabecera
    d.line((m, L["rule"], W - m, L["rule"]), fill=rgb(VERDE), width=1)
    # consigna del pie (rota entre arranques): a la derecha, en la fila de la cuenta atras
    fp = ttf(False, L["small"])
    pb = fp.getbbox("Mg")
    w = d.textlength(pie, font=fp)
    assert w < W - 2 * m, pie
    text1(d, (W - m - w, L["tlabel"] + (L["item_h"] - (pb[3] - pb[1])) // 2 - pb[1]), pie, fp, GRIS)
    return im

def theme_txt(W, H, L):
    m = L["margin"]
    it, sm = fname(False, L["item"]), fname(False, L["small"])
    itb = fname(True, L["item"])
    menu_w = W - 2 * m
    ic = 8 * L["icon"]
    col2 = m + menu_w // 4
    # 1a linea, en el primer byte: Ventoy la lee al abrir el tema para colocar su etiqueta de version (si no, sale en
    # azul #0000ff a la izquierda y pisa la linea de atajos). Gris de la paleta: a 1920 abajo a la derecha; a 1024
    # la linea de atajos llega casi al borde, asi que arriba a la derecha.
    return f"""ventoy_left_top_color: "@{L["ver"]}@{GRIS}@"
# Tema QKVM para Ventoy (GRUB2) {W}x{H}. GENERADO por tools/gen_ventoy_theme.py: no editar a mano.
# Estetica de docs/DESIGN.md: fondo negro, verde fosforo, bordes de 1 px, sin redondeos ni degradados.
desktop-image: "background.png"
desktop-color: "{NEGRO}"
title-text: ""
message-font: "{it}"
message-color: "{HUESO}"
message-bg-color: "{NEGRO}"
terminal-font: "{sm}"
terminal-box: "terminal_box_*.png"

+ boot_menu {{
  left = {m}
  top = {L["menu_top"]}
  width = {menu_w}
  height = {L["menu_h"]}
  menu_pixmap_style = "menu_*.png"
  item_font = "{it}"
  selected_item_font = "{itb}"
  item_color = "{VERDE}"
  selected_item_color = "{NEGRO}"
  selected_item_pixmap_style = "select_*.png"
  item_height = {L["item_h"]}
  item_padding = {L["k"] * 2}
  item_spacing = 2
  icon_width = {ic}
  icon_height = {ic}
  item_icon_space = {L["k"] * 4}
  scrollbar = true
  scrollbar_width = {L["k"] * 3}
  scrollbar_frame = "sbframe_*.png"
  scrollbar_thumb = "thumb_*.png"
}}

# cuenta atras (solo si ventoy.json pone VTOY_MENU_TIMEOUT): barra ambar fina + texto
+ progress_bar {{
  id = "__timeout__"
  left = {m}
  top = {L["tbar"]}
  width = {menu_w}
  height = {L["k"] * 2}
  show_text = false
  fg_color = "{AMBAR}"
  bg_color = "{NEGRO}"
  border_color = "{AMBAR}"
}}
+ label {{
  id = "__timeout__"
  text = "@TIMEOUT_NOTIFICATION_LONG@"
  left = {m}
  top = {L["tlabel"]}
  width = {menu_w}
  height = {L["item_h"]}
  align = "left"
  font = "{sm}"
  color = "{AMBAR}"
}}

# avisos de modo de Ventoy (memdisk, iso raw, grub2, wimboot, drivers UEFI): ambar
+ label {{ left = {m} top = {L["modes"]} width = {menu_w // 4} height = {L["item_h"]} text = "@VTOY_MEM_DISK@" font = "{sm}" color = "{AMBAR}" align = "left" }}
+ label {{ left = {col2} top = {L["modes"]} width = {menu_w // 4} height = {L["item_h"]} text = "@VTOY_ISO_RAW@" font = "{sm}" color = "{AMBAR}" align = "left" }}
+ label {{ left = {m + menu_w // 2} top = {L["modes"]} width = {menu_w // 4} height = {L["item_h"]} text = "@VTOY_GRUB2_MODE@" font = "{sm}" color = "{AMBAR}" align = "left" }}
+ label {{ left = {m + 3 * menu_w // 4} top = {L["modes"]} width = {menu_w // 4} height = {L["item_h"]} text = "@VTOY_WIMBOOT_MODE@" font = "{sm}" color = "{AMBAR}" align = "left" }}
+ label {{ left = {W - m - menu_w // 3} top = {L["top"]} width = {menu_w // 3} height = {L["item_h"]} text = "@VTOY_ISO_UEFI_DRV@" font = "{sm}" color = "{AMBAR}" align = "right" }}

# teclas (F1 ayuda, F2 explorar, F5 herramientas...): texto secundario en gris
+ label {{ left = {m} top = {L["tip"]} width = {menu_w} height = {L["item_h"]} text = "@VTOY_HOTKEY_TIP@" font = "{sm}" color = "{GRIS}" align = "left" }}
"""

def gen_theme():
    d = os.path.join(OUT, "qkvm")
    os.makedirs(os.path.join(d, "icons"), exist_ok=True)
    nine(d, "menu", VERDE, NEGRO)
    nine(d, "terminal_box", VERDE, NEGRO)
    nine(d, "sbframe", GRIS, NEGRO)
    solid(os.path.join(d, "select_c.png"), VERDE)
    solid(os.path.join(d, "thumb_c.png"), VERDE)
    # iconos a 48 px (8x8 x6): GRUB los reduce a icon_width (24 = /2, 16 = /3) sin fracciones
    for n, (col, rows) in ICONS.items():
        icon(rows, col, 6).save(os.path.join(d, "icons", n + ".png"), optimize=True)
    for (W, H), L in LAYOUTS.items():
        for k, pie in enumerate(PIE, 1):
            tag = "%dx%d_%d" % (W, H, k)
            background(W, H, L, pie).save(os.path.join(d, "bg_%s.png" % tag), optimize=True)
            open(os.path.join(d, "theme_%s.txt" % tag), "w", newline="\n").write(theme_txt(W, H, L).replace('desktop-image: "background.png"', 'desktop-image: "bg_%s.png"' % tag))

def gen_fonts():
    d = os.path.join(OUT, "fonts")
    os.makedirs(d, exist_ok=True)
    # ASCII, Latin-1, puntuacion, flechas, cajas y bloques: lo que usan Ventoy y los nombres de ISO habituales
    ranges = ["0x20-0x7E", "0xA0-0x17F", "0x2010-0x2027", "0x2190-0x2193", "0x2500-0x259F"]
    for bold, size in FONT_SIZES:
        src = os.path.join(TTF_DIR, "JetBrainsMono-%s.ttf" % ("Bold" if bold else "Regular"))
        out = os.path.join(d, "jbmono-%s-%d.pf2" % ("bold" if bold else "regular", size))
        cmd = ["grub-mkfont", "-s", str(size), "-o", out] + sum((["-r", r] for r in ranges), []) + [src]
        subprocess.check_call(cmd)
        # comprobar el nombre interno que usa theme.txt
        data = open(out, "rb").read()
        i = data.index(b"NAME") + 8
        name = data[i:data.index(b"\0", i)].decode()
        assert name == fname(bold, size), (name, fname(bold, size))

if __name__ == "__main__":
    import shutil
    for old in ("qkvm", "qkvm_1920x1080", "qkvm_1024x768"):
        shutil.rmtree(os.path.join(OUT, old), ignore_errors=True)
    gen_theme()
    if "--fonts" in sys.argv:
        gen_fonts()
    print("ok:", OUT)
