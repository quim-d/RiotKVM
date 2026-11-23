"""Protocolo de texto actual del dongle (CDC). Una línea por orden, respuesta 'ok' | 'err ...'.

Órdenes (src/main.cpp): type, kd, ku, kall, mm, ma, mbr, mba, status, bootloader.
Todo lo que sea layout/texto->HID se resuelve en el cliente; aquí solo se codifica.
"""
from __future__ import annotations

from .errors import UsageError

ABS_MAX = 32767
BTN_LEFT, BTN_RIGHT, BTN_MIDDLE = 1, 2, 4


def _i(v: int, lo: int, hi: int, what: str) -> int:
    v = int(v)
    if not lo <= v <= hi:
        raise UsageError(f"{what}={v} fuera de rango [{lo}, {hi}]")
    return v


def type_text(text: str) -> str:
    if "\n" in text or "\r" in text:
        raise UsageError("'type' del firmware no admite saltos de línea")
    return "type " + text


def key_down(usage: int) -> str:
    return f"kd 0x{_i(usage, 1, 0xE7, 'usage'):02X}"


def key_up(usage: int) -> str:
    return f"ku 0x{_i(usage, 1, 0xE7, 'usage'):02X}"


def release_all_keys() -> str:
    return "kall"


def mouse_rel(dx: int, dy: int, wheel: int = 0) -> str:
    return f"mm {_i(dx, -128, 127, 'dx')} {_i(dy, -128, 127, 'dy')} {_i(wheel, -128, 127, 'wheel')}"


def mouse_abs(x: int, y: int, wheel: int = 0) -> str:
    return f"ma {_i(x, 0, ABS_MAX, 'x')} {_i(y, 0, ABS_MAX, 'y')} {_i(wheel, -128, 127, 'wheel')}"


def buttons_rel(mask: int) -> str:
    return f"mbr {_i(mask, 0, 7, 'botones')}"


def buttons_abs(mask: int) -> str:
    return f"mba {_i(mask, 0, 7, 'botones')}"


def status() -> str:
    return "status"


def parse_status(reply: str) -> dict:
    """'host=1 leds=0x02 hidready=1' -> {'host': True, 'leds': 2, 'hidready': True}"""
    out: dict = {}
    for part in reply.split():
        if "=" not in part:
            continue
        k, v = part.split("=", 1)
        try:
            n = int(v, 0)
        except ValueError:
            out[k] = v
            continue
        out[k] = n if k == "leds" else bool(n) if k in ("host", "hidready", "susp", "sd") else n
    if not out:
        raise ValueError(f"respuesta de status ilegible: {reply!r}")
    return out
