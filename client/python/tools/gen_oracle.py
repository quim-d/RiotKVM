"""Genera client/testdata/oracle.json con las salidas de la implementación Python de referencia.

La implementación Rust (dkvm-core/tests/oracle.rs) debe producir lo mismo para las mismas entradas:
mismos usos HID, mismas tramas enviadas al transporte y mismos códigos de salida de error.

    cd client/python && python tools/gen_oracle.py
"""
import json
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))

import shlex  # noqa: E402

from dkvm import protocol as P  # noqa: E402
from dkvm.errors import DkvmError  # noqa: E402
from dkvm.input import Kvm, abs_from_pixels  # noqa: E402
from dkvm.keys import parse_combo  # noqa: E402
from dkvm.layouts import get_layout  # noqa: E402
from dkvm.script import Context, parse_script, run_script  # noqa: E402
from dkvm.transport import MockTransport  # noqa: E402


def mk(layout):
    t = MockTransport()
    t.open()
    return Kvm(t, layout, sleep=lambda s: None), t


def err(e):
    return {"error": e.exit_code}


def guard(fn):
    try:
        return fn()
    except DkvmError as e:
        return err(e)


out = {}

# ---- layouts: carácter -> pulsaciones
chars = [chr(c) for c in range(0, 0x180)] + list("€—…漢😀´¨`^·¬ºª¡¿")
out["strokes"] = {}
for name in ("es", "us"):
    lay = get_layout(name)
    d = {}
    for ch in chars:
        d[ch] = guard(lambda: {"s": [list(s) for s in lay.strokes(ch)]})
    out["strokes"][name] = d

# ---- combos
combos = ["ctrl+alt+del", "F5", "win+r", "ctrl+@", "ctrl++", "+", "plus", "shift+plus", "alt+tab", "ctrl+shift+esc",
          "", "ctrl+", "ctrl+nada", "0xZZ", "0x04", "0x1FF", "ctrl+0x4c", "A", "ñ", "á", "ctrl+ñ", "Ctrl+Alt+Supr",
          " ctrl + c ", "ctrl+f13", "f24", "altgr+q", "cmd+space", "a+++", "++", "ctrl+á", "shift+ñ", "AltGr+e"]
out["combos"] = []
for name in ("es", "us"):
    lay = get_layout(name)
    for c in combos:
        out["combos"].append({"layout": name, "combo": c,
                              "r": guard(lambda: {"s": [list(s) for s in parse_combo(c, lay)]})})

# ---- type_text (cadenas fijas + aleatorias con semilla)
texts = ["Hi", "a\r\nb", "Hola, ñandú @#", "ok ñ", "áéíóú ÀÈÌÒÙ âêîôû äëïöü", "`^´¨", "C:\\Users\\x", "{[]}|~€",
         "tab\there", "", "漢", "1234567890 !\"·$%&/()=", "?¿¡'", "<>"]
rnd = random.Random(1234)
alpha = [chr(c) for c in range(32, 127)] + list("ñÑáéíóúüÜçÇ€¡¿º ª´¨\n\t")
for _ in range(300):
    texts.append("".join(rnd.choice(alpha) for _ in range(rnd.randint(1, 20))))
out["type"] = []
for name in ("es", "us"):
    for tx in texts:
        k, t = mk(name)
        r = guard(lambda: (k.type_text(tx), {"sent": list(t.sent)})[1])
        out["type"].append({"layout": name, "text": tx, "r": r})

# ---- type --fw
out["type_fw"] = []
for tx in ["hola", "hola\n", "a\nb", "a\r\nb\r\n", "\n", "", "a\n\n"]:
    k, t = mk("es")
    out["type_fw"].append({"text": tx, "r": guard(lambda: (k.type_text_firmware(tx), {"sent": list(t.sent)})[1])})

# ---- press / key_down_name / key_up_name
out["press"] = []
for c in ["ctrl+alt+del", "shift+a", "ctrl+shift+a", "a+a", "ctrl+@", "nada", "alt+tab", "ctrl+ñ"]:
    for fn in ("press", "key_down_name", "key_up_name"):
        k, t = mk("es")
        r = guard(lambda: (getattr(k, fn)(c), {"sent": list(t.sent)})[1])
        out["press"].append({"fn": fn, "combo": c, "r": r})

# ---- ratón
out["mouse"] = []
for dx, dy, w in [(300, -200, 0), (0, 0, 0), (127, -127, 0), (128, 0, 0), (-1000, 5, 3), (5, 5, 300), (-255, 254, -129)]:
    k, t = mk("es")
    out["mouse"].append({"op": "move_rel", "args": [dx, dy, w],
                         "r": guard(lambda: (k.move_rel(dx, dy, w), {"sent": list(t.sent)})[1])})
for amt, rel in [(-3, False), (3, True), (300, False), (0, False), (-128, True)]:
    k, t = mk("es")
    k.move_abs(100, 200)
    out["mouse"].append({"op": "scroll", "args": [amt, rel],
                         "r": guard(lambda: (k.scroll(amt, rel), {"sent": list(t.sent)})[1])})
for btn, cnt, rel in [("left", 1, False), ("right", 2, False), ("middle", 1, True), ("x", 1, False), ("l", 0, False)]:
    k, t = mk("es")
    out["mouse"].append({"op": "click", "args": [btn, cnt, rel],
                         "r": guard(lambda: (k.click(btn, cnt, rel), {"sent": list(t.sent)})[1])})
k, t = mk("es")
k.button_down("left")
k.button_down("middle")
k.button_up("left")
out["buttons_seq"] = {"sent": list(t.sent), "buttons": k.buttons}
out["abs_px"] = []
for px, py, w, h in [(0, 0, 1920, 1080), (1919, 1079, 1920, 1080), (5000, -4, 1920, 1080), (960, 540, 1920, 1080),
                     (1, 1, 3, 3), (0.5, 0.5, 2, 2), (1, 0, 3, 2), (10, 10, 1, 5), (100.5, 7.25, 1366, 768)]:
    out["abs_px"].append({"a": [px, py, w, h], "r": guard(lambda: {"xy": list(abs_from_pixels(px, py, w, h))})})

# ---- protocolo
out["protocol"] = []
for fn, args in [("key_down", [4]), ("key_down", [0xE7]), ("key_down", [0xE8]), ("key_up", [0]), ("mouse_rel", [-128, 127, 0]),
                 ("mouse_rel", [128, 0, 0]), ("mouse_abs", [32767, 0, -128]), ("mouse_abs", [32768, 0, 0]),
                 ("buttons_rel", [7]), ("buttons_abs", [8]), ("type_text", ["abc def"]), ("type_text", ["a\nb"])]:
    out["protocol"].append({"fn": fn, "args": args, "r": guard(lambda: {"s": getattr(P, fn)(*args)})})
out["status"] = []
for s in ["host=1 leds=0x02 hidready=0", "host=0 leds=0 hidready=1 extra=7", "ver=abc host=1", "x=0x10", "basura", "", "host=2", "host=1 leds=0x00 hidready=1 susp=0 proto=report sd=1"]:
    try:
        out["status"].append({"in": s, "out": P.parse_status(s)})
    except ValueError:
        out["status"].append({"in": s, "out": None})

# ---- shlex
out["shlex"] = []
for s in ['type "Hola, mundo"', "a b  c", "a#b c", "a # c", '"a b" c', "'a b' c", r'"a\"b"', r'"a\\b"', r'"a\nb"', r"a\ b",
          '""', "''", 'a""b', '"sin cerrar', "'sin cerrar", "a\\", 'x "y z"#c', "  ", "#solo", 'type "a # no"', "é ñ"]:
    try:
        out["shlex"].append({"in": s, "out": shlex.split(s, comments=True)})
    except ValueError as e:
        out["shlex"].append({"in": s, "out": None, "err": str(e)})

# ---- scripts
scripts = [
    '# demo\nlayout us\nscreen 1920x1080\nmouse-mode abs\nmove 960 540\nclick left 2\ntype "Hi there"\nkey ctrl+a\nsleep 0.1\nstatus\n',
    'keydown shift\nfoo bar\n',
    'type "sin cerrar',
    'layout es\ntype "Hola ñandú"\nkey ctrl+alt+del enter\n',
    'mouse-mode rel\nmove 300 -200\nclick right\ndown left\nup left\nscroll -3\n',
    'mouse-mode abs\nmove 100 200\nscroll 5\ndown left\nrelease\n',
    'screen 800x600\nmove 799 599\nmove -5 9999\n',
    'raw kd 0x04\nraw nope\nkey a\n',
    'raw mm 1 2 3\nstatus\n',
    'move 1\n', 'move a b\n', 'click left x\n', 'click middle 3\n', 'sleep x\n', 'mouse-mode up\n', 'screen 10\n',
    'layout xx\n', 'key\n', 'keydown a b\n', 'keyup ctrl+c\n', '   \n# nada\n', 'TYPE "x"\n', 'type a b c\n',
    'down\nup\nclick\n', 'move 1.9 2.9\nmouse-mode rel\nmove 1.9 -2.9\n',
]
out["scripts"] = []
for sc in scripts:
    for layout in ("es",):
        k, t = mk(layout)
        ctx = Context(k, sleep=lambda s: None)
        try:
            run_script(ctx, sc)
            r = {"sent": list(t.sent), "results": [[c, v] for c, v in ctx.results]}
        except DkvmError as e:
            r = {"error": e.exit_code, "sent": list(t.sent), "msg": str(e)}
        except Exception as e:  # ValueError de float() etc. que el Python deja escapar
            r = {"error": None, "sent": list(t.sent), "msg": f"{type(e).__name__}: {e}"}
        out["scripts"].append({"text": sc, "r": r})

dest = os.path.normpath(os.path.join(HERE, "..", "..", "testdata", "oracle.json"))
os.makedirs(os.path.dirname(dest), exist_ok=True)
with open(dest, "w", encoding="utf-8") as f:
    json.dump(out, f, ensure_ascii=False, indent=0, sort_keys=True)
print("escrito", dest, {k: len(v) for k, v in out.items() if hasattr(v, "__len__")})
