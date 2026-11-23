"""Distribuciones de teclado del equipo DESTINO: carácter -> pulsaciones HID.

Una pulsación es (uso_hid, máscara_modificadores). El dongle es "tonto": aquí se
decide qué tecla física produce cada carácter según el layout que tiene el servidor.
"""
from __future__ import annotations

import unicodedata

from .errors import UsageError
from .keys import MOD_LSHIFT, MOD_RALT

Stroke = tuple[int, int]
S, A = MOD_LSHIFT, MOD_RALT   # Shift, AltGr

ENTER, TAB, SPACE = 0x28, 0x2B, 0x2C


def _letters() -> dict[str, Stroke]:
    d: dict[str, Stroke] = {}
    for i, c in enumerate("abcdefghijklmnopqrstuvwxyz"):
        d[c] = (0x04 + i, 0)
        d[c.upper()] = (0x04 + i, S)
    return d


def _common() -> dict[str, Stroke]:
    d = _letters()
    d.update({" ": (SPACE, 0), "\n": (ENTER, 0), "\t": (TAB, 0)})
    for i, c in enumerate("1234567890"):
        d[c] = (0x1E + i, 0)
    return d


def _us_table() -> dict[str, Stroke]:
    d = _common()
    for i, c in enumerate("!@#$%^&*()"):
        d[c] = (0x1E + i, S)
    pairs = [(0x2D, "-", "_"), (0x2E, "=", "+"), (0x2F, "[", "{"), (0x30, "]", "}"), (0x31, "\\", "|"),
             (0x33, ";", ":"), (0x34, "'", '"'), (0x35, "`", "~"), (0x36, ",", "<"), (0x37, ".", ">"),
             (0x38, "/", "?")]
    for u, a, b in pairs:
        d[a] = (u, 0)
        d[b] = (u, S)
    return d


def _es_table() -> dict[str, Stroke]:
    d = _common()
    # fila de números: shift y AltGr
    shift = "!\"·$%&/()="
    for i, c in enumerate(shift):
        d[c] = (0x1E + i, S)
    d["|"], d["@"], d["#"], d["~"], d["€"], d["¬"] = (0x1E, A), (0x1F, A), (0x20, A), (0x21, A), (0x22, A), (0x23, A)
    d["º"], d["ª"], d["\\"] = (0x35, 0), (0x35, S), (0x35, A)
    d["'"], d["?"] = (0x2D, 0), (0x2D, S)
    d["¡"], d["¿"] = (0x2E, 0), (0x2E, S)
    d["["] = (0x2F, A)
    d["+"], d["*"], d["]"] = (0x30, 0), (0x30, S), (0x30, A)
    d["ñ"], d["Ñ"] = (0x33, 0), (0x33, S)
    d["{"] = (0x34, A)
    d["ç"], d["Ç"], d["}"] = (0x31, 0), (0x31, S), (0x31, A)
    d["<"], d[">"] = (0x64, 0), (0x64, S)
    d[","], d[";"] = (0x36, 0), (0x36, S)
    d["."], d[":"] = (0x37, 0), (0x37, S)
    d["-"], d["_"] = (0x38, 0), (0x38, S)
    return d


# Teclas muertas del layout español: (uso, mods) que hay que pulsar antes de la vocal/espacio
_ES_DEAD = {
    "́": (0x34, 0),   # acento agudo  ´
    "̈": (0x34, S),   # diéresis      ¨
    "̀": (0x2F, 0),   # acento grave  `
    "̂": (0x2F, S),   # circunflejo   ^
}
_ES_DEAD_CHARS = {"´": "́", "¨": "̈", "`": "̀", "^": "̂"}


class Layout:
    def __init__(self, name: str, table: dict[str, Stroke], dead_marks: dict[str, Stroke] | None = None,
                 dead_chars: dict[str, str] | None = None):
        self.name = name
        self.table = table
        self.dead_marks = dead_marks or {}
        self.dead_chars = dead_chars or {}

    def strokes(self, ch: str) -> list[Stroke]:
        if ch in self.dead_chars:        # la tecla muerta sola: pulsarla y luego espacio
            return [self.dead_marks[self.dead_chars[ch]], (SPACE, 0)]
        if ch in self.table:
            return [self.table[ch]]
        if self.dead_marks:              # vocal acentuada: tecla muerta + base
            dec = unicodedata.normalize("NFD", ch)
            if len(dec) == 2 and dec[1] in self.dead_marks and dec[0] in self.table:
                return [self.dead_marks[dec[1]], self.table[dec[0]]]
        raise UsageError(f"el carácter {ch!r} no se puede escribir con el layout {self.name}")

    def text_strokes(self, text: str) -> list[Stroke]:
        text = text.replace("\r\n", "\n").replace("\r", "\n")
        out: list[Stroke] = []
        for ch in text:
            out.extend(self.strokes(ch))
        return out


LAYOUTS: dict[str, Layout] = {
    "us": Layout("us", _us_table()),
    "es": Layout("es", _es_table(), _ES_DEAD, _ES_DEAD_CHARS),
}


def get_layout(name: str) -> Layout:
    try:
        return LAYOUTS[name.lower()]
    except KeyError:
        raise UsageError(f"layout desconocido {name!r}; disponibles: {', '.join(LAYOUTS)}") from None
