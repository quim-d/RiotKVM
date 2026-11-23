"""Dongle simulado: interpreta el protocolo de texto igual que src/main.cpp y guarda el estado HID."""
from __future__ import annotations

import time

from .base import Transport, check_reply

_KNOWN = {"type", "kd", "ku", "kall", "mm", "ma", "mbr", "mba", "status", "bootloader"}


def _i8(v: int) -> int:
    v &= 0xFF
    return v - 256 if v > 127 else v


class MockTransport(Transport):
    name = "mock"

    def __init__(self, latency: float = 0.0, echo=None):
        self.latency = latency
        self.echo = echo                    # callable(str) para volcar el tráfico (dry-run / -v)
        self.sent: list[str] = []
        self.opened = False
        self.fail_next: str | None = None   # inyectar un 'err ...' en la siguiente orden
        self.reset_state()

    def reset_state(self):
        self.keys: set[int] = set()
        self.typed: list[str] = []
        self.rel_buttons = 0
        self.abs_buttons = 0
        self.abs_pos = (0, 0)
        self.rel_total = [0, 0]
        self.wheel_total = 0
        # ('kd',u) ('ku',u) ('kall',) ('mm',dx,dy,w) ('ma',x,y,w) ('mbr',m) ('mba',m)
        self.events: list[tuple] = []

    def open(self):
        self.opened = True

    def close(self):
        self.opened = False

    def send(self, line: str, wait: bool = True):
        if not self.opened:
            raise RuntimeError("MockTransport no abierto")
        self.sent.append(line)
        if self.echo:
            self.echo(line)
        if self.latency:
            time.sleep(self.latency)
        reply = check_reply(line, self._handle(line))
        return reply if wait else None

    def _handle(self, line: str) -> str:
        if self.fail_next:
            r, self.fail_next = self.fail_next, None
            return r
        line = line.strip()
        c, _, a = line.partition(" ")
        if c not in _KNOWN:
            return "err comando desconocido"
        n = []
        for p in a.split()[:3]:
            try:
                n.append(int(p, 0))
            except ValueError:
                break
        n += [0] * (3 - len(n))
        if c == "type":
            self.typed.append(a)
        elif c == "kd":
            u = int(a, 0) & 0xFF
            self.keys.add(u)
            self.events.append(("kd", u))
        elif c == "ku":
            u = int(a, 0) & 0xFF
            self.keys.discard(u)
            self.events.append(("ku", u))
        elif c == "kall":
            self.keys.clear()
            self.events.append(("kall",))
        elif c == "mm":
            dx, dy, w = (_i8(v) for v in n)
            self.rel_total[0] += dx
            self.rel_total[1] += dy
            self.wheel_total += w
            self.events.append(("mm", dx, dy, w))
        elif c == "ma":
            x, y, w = n[0] & 0xFFFF, n[1] & 0xFFFF, _i8(n[2])
            self.abs_pos = (x, y)
            self.wheel_total += w
            self.events.append(("ma", x, y, w))
        elif c == "mbr":
            self.rel_buttons = n[0] & 0xFF
            self.events.append(("mbr", self.rel_buttons))
        elif c == "mba":
            self.abs_buttons = n[0] & 0xFF
            self.events.append(("mba", self.abs_buttons))
        elif c == "status":
            return "host=1 leds=0x00 hidready=1"
        return "ok"
