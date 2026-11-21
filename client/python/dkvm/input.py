"""Control de alto nivel: teclado y ratón sobre un Transport.

Ruta de entrada sin colas propias: cada acción se traduce a órdenes y se envía
con pipelining (wait=False) y un flush al final de la acción.
"""
from __future__ import annotations

import time

from . import protocol as P
from .errors import UsageError
from .keys import parse_combo
from .layouts import Layout, get_layout
from .transport.base import Transport

BUTTONS = {"left": P.BTN_LEFT, "l": P.BTN_LEFT, "right": P.BTN_RIGHT, "r": P.BTN_RIGHT,
           "middle": P.BTN_MIDDLE, "m": P.BTN_MIDDLE}


def button_mask(name: str) -> int:
    try:
        return BUTTONS[name.lower()]
    except KeyError:
        raise UsageError(f"botón desconocido {name!r}; usa left, right o middle") from None


def abs_from_pixels(px: float, py: float, width: int, height: int) -> tuple[int, int]:
    """Posición en el vídeo mostrado (px, py) -> coordenadas absolutas 0..32767."""
    if width < 2 or height < 2:
        raise UsageError("resolución inválida")
    x = round(min(max(px, 0), width - 1) / (width - 1) * P.ABS_MAX)
    y = round(min(max(py, 0), height - 1) / (height - 1) * P.ABS_MAX)
    return x, y


def parse_screen(text: str) -> tuple[int, int]:
    try:
        w, h = text.lower().split("x")
        return int(w), int(h)
    except ValueError:
        raise UsageError(f"--screen debe ser ANCHOxALTO (p. ej. 1920x1080), no {text!r}") from None


class Kvm:
    def __init__(self, transport: Transport, layout: Layout | str = "es", sleep=time.sleep):
        self.t = transport
        self.layout = get_layout(layout) if isinstance(layout, str) else layout
        self._sleep = sleep
        self.buttons = 0                       # máscara de botones mantenidos (modo actual)
        self.abs_pos = (P.ABS_MAX // 2, P.ABS_MAX // 2)

    # ---------- bajo nivel ----------
    def _send(self, line: str):
        self.t.send(line, wait=False)

    def raw(self, line: str) -> str:
        return self.t.send(line, wait=True)

    def status(self) -> dict:
        return P.parse_status(self.raw(P.status()))

    # ---------- teclado ----------
    def key_down(self, usage: int):
        self._send(P.key_down(usage))
        self.t.flush()

    def key_up(self, usage: int):
        self._send(P.key_up(usage))
        self.t.flush()

    def release_all(self):
        self._send(P.release_all_keys())
        self._send(P.buttons_rel(0))
        self._send(P.buttons_abs(0))
        self.buttons = 0
        self.t.flush()

    def _tap(self, usage: int, mods: int):
        mod_usages = [0xE0 + i for i in range(8) if mods & (1 << i)]
        for m in mod_usages:
            self._send(P.key_down(m))
        self._send(P.key_down(usage))
        self._send(P.key_up(usage))
        for m in reversed(mod_usages):
            self._send(P.key_up(m))

    def type_text(self, text: str, delay: float = 0.0):
        """Escribe 'text' con el layout del destino (vía kd/ku, no con 'type' del firmware)."""
        strokes = self.layout.text_strokes(text)   # valida todo antes de enviar nada
        try:
            for usage, mods in strokes:
                self._tap(usage, mods)
                if delay:
                    self.t.flush()
                    self._sleep(delay)
        finally:
            self.t.flush()

    def type_text_firmware(self, text: str):
        """'type' nativo del dongle (layout US fijo en el firmware, una línea)."""
        lines = text.replace("\r\n", "\n").split("\n")
        if len(lines) > 1 and lines[-1] == "":   # el '\n' final termina la última línea (no añade un Enter extra)
            lines.pop()
        for line in lines:
            if line:
                self._send(P.type_text(line))
            self._send(P.key_down(0x28))
            self._send(P.key_up(0x28))
        self.t.flush()

    def press(self, combo: str, hold: float = 0.0):
        """Pulsa y suelta una combinación ('ctrl+alt+del'). Libera siempre al acabar."""
        keys = parse_combo(combo, self.layout)
        downs: list[int] = []
        try:
            for usage, mods in keys:
                for m in [0xE0 + i for i in range(8) if mods & (1 << i)]:
                    if m not in downs:
                        self._send(P.key_down(m))
                        downs.append(m)
                if usage not in downs:
                    self._send(P.key_down(usage))
                    downs.append(usage)
            self.t.flush()
            if hold:
                self._sleep(hold)
        finally:
            for u in reversed(downs):
                self._send(P.key_up(u))
            self.t.flush()

    def key_down_name(self, combo: str):
        for usage, mods in parse_combo(combo, self.layout):
            for m in [0xE0 + i for i in range(8) if mods & (1 << i)]:
                self._send(P.key_down(m))
            self._send(P.key_down(usage))
        self.t.flush()

    def key_up_name(self, combo: str):
        for usage, mods in reversed(parse_combo(combo, self.layout)):
            self._send(P.key_up(usage))
            for m in reversed([0xE0 + i for i in range(8) if mods & (1 << i)]):
                self._send(P.key_up(m))
        self.t.flush()

    # ---------- ratón ----------
    def move_rel(self, dx: int, dy: int, wheel: int = 0):
        """Movimiento relativo; trocea en pasos de ±127 (el informe HID es int8)."""
        dx, dy, wheel = int(dx), int(dy), int(wheel)
        while dx or dy or wheel:
            sx, sy, sw = (max(-127, min(127, v)) for v in (dx, dy, wheel))
            self._send(P.mouse_rel(sx, sy, sw))
            dx, dy, wheel = dx - sx, dy - sy, wheel - sw
        self.t.flush()

    def move_abs(self, x: int, y: int):
        x, y = int(x), int(y)
        self._send(P.mouse_abs(x, y))
        self.abs_pos = (x, y)
        self.t.flush()

    def move_px(self, px: float, py: float, width: int, height: int):
        self.move_abs(*abs_from_pixels(px, py, width, height))

    def _set_buttons(self, mask: int, relative: bool):
        self.buttons = mask
        self._send(P.buttons_rel(mask) if relative else P.buttons_abs(mask))

    def button_down(self, name: str, relative: bool = False):
        self._set_buttons(self.buttons | button_mask(name), relative)
        self.t.flush()

    def button_up(self, name: str, relative: bool = False):
        self._set_buttons(self.buttons & ~button_mask(name), relative)
        self.t.flush()

    def click(self, name: str = "left", count: int = 1, relative: bool = False, interval: float = 0.0):
        mask = button_mask(name)
        try:
            for i in range(count):
                self._set_buttons(self.buttons | mask, relative)
                self._set_buttons(self.buttons & ~mask, relative)
                if interval and i < count - 1:
                    self.t.flush()
                    self._sleep(interval)
        finally:
            self.t.flush()

    def scroll(self, amount: int, relative: bool = False):
        """Rueda: positivo = arriba. Trocea en pasos de ±127."""
        amount = int(amount)
        while amount:
            s = max(-127, min(127, amount))
            self._send(P.mouse_rel(0, 0, s) if relative else P.mouse_abs(*self.abs_pos, s))
            amount -= s
        self.t.flush()
