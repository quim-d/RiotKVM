"""Nombres de teclas -> usos HID (página Keyboard/Keypad 0x07) y combinaciones 'ctrl+alt+del'."""
from __future__ import annotations

from .errors import UsageError

MOD_LCTRL, MOD_LSHIFT, MOD_LALT, MOD_LGUI = 0x01, 0x02, 0x04, 0x08
MOD_RCTRL, MOD_RSHIFT, MOD_RALT, MOD_RGUI = 0x10, 0x20, 0x40, 0x80

NAMES: dict[str, int] = {
    "enter": 0x28, "return": 0x28, "esc": 0x29, "escape": 0x29, "backspace": 0x2A, "bksp": 0x2A,
    "tab": 0x2B, "space": 0x2C, "capslock": 0x39, "caps": 0x39,
    "printscreen": 0x46, "prtsc": 0x46, "scrolllock": 0x47, "pause": 0x48, "insert": 0x49, "ins": 0x49,
    "home": 0x4A, "pageup": 0x4B, "pgup": 0x4B, "delete": 0x4C, "del": 0x4C, "end": 0x4D,
    "pagedown": 0x4E, "pgdn": 0x4E, "right": 0x4F, "left": 0x50, "down": 0x51, "up": 0x52,
    "numlock": 0x53, "menu": 0x65, "app": 0x65,
    "ctrl": 0xE0, "lctrl": 0xE0, "control": 0xE0, "shift": 0xE1, "lshift": 0xE1,
    "alt": 0xE2, "lalt": 0xE2, "win": 0xE3, "gui": 0xE3, "super": 0xE3, "meta": 0xE3, "cmd": 0xE3, "lwin": 0xE3,
    "rctrl": 0xE4, "rshift": 0xE5, "ralt": 0xE6, "altgr": 0xE6, "rwin": 0xE7, "rgui": 0xE7,
}
for _i, _c in enumerate("abcdefghijklmnopqrstuvwxyz"):
    NAMES[_c] = 0x04 + _i
for _i, _c in enumerate("1234567890"):
    NAMES[_c] = 0x1E + _i
for _i in range(1, 13):
    NAMES[f"f{_i}"] = 0x3A + _i - 1
for _i in range(13, 25):
    NAMES[f"f{_i}"] = 0x68 + _i - 13

_REV = {}
for _n, _u in NAMES.items():
    _REV.setdefault(_u, _n)


def usage_name(usage: int) -> str:
    return _REV.get(usage, f"0x{usage:02X}")


def is_modifier(usage: int) -> bool:
    return 0xE0 <= usage <= 0xE7


def mods_to_usages(mask: int) -> list[int]:
    return [0xE0 + i for i in range(8) if mask & (1 << i)]


def parse_combo(text: str, layout) -> list[tuple[int, int]]:
    """'ctrl+alt+del' -> [(usage, extra_mods_mask), ...] en orden de pulsación.

    Cada elemento es un nombre de tecla o un único carácter (resuelto con el
    layout, que puede añadir Shift/AltGr). 'plus' = '+'. Un '+' final ('ctrl++') también vale.
    """
    text = text.strip()
    if not text:
        raise UsageError("combinación vacía")
    if text.endswith("++"):
        parts = text[:-2].split("+") + ["plus"] if text[:-2] else ["plus"]
    else:
        parts = text.split("+")
    out: list[tuple[int, int]] = []
    for p in parts:
        p = p.strip()
        if not p:
            raise UsageError(f"combinación mal formada: {text!r}")
        low = p.lower()
        if low in NAMES:
            out.append((NAMES[low], 0))
        elif low.startswith("0x"):
            try:
                out.append((int(low, 16), 0))
            except ValueError:
                raise UsageError(f"uso HID inválido: {p!r}") from None
        else:
            ch = "+" if low == "plus" else p
            if len(ch) != 1:
                raise UsageError(f"tecla desconocida: {p!r}")
            strokes = layout.strokes(ch)
            if len(strokes) != 1:
                raise UsageError(f"{p!r} no es una tecla simple en el layout {layout.name}")
            out.append(strokes[0])
    return out
