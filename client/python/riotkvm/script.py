"""Macros reproducibles: un fichero de texto, una acción por línea.

    # comentario
    layout es                # es | us
    mouse-mode abs           # abs | rel (afecta a move/click/down/up/scroll)
    screen 1920x1080         # en modo abs, move X Y se interpreta en píxeles; sin esto, 0..32767
    type "Hola, mundo"       # texto con el layout; las comillas son shlex
    key ctrl+alt+del         # pulsar y soltar
    keydown shift            # mantener / keyup shift
    move 960 540             # abs: posición; rel: desplazamiento
    click left 2             # botón [veces]
    down left / up left      # mantener / soltar botón
    scroll -3                # rueda (positivo = arriba)
    sleep 0.5                # segundos
    release                  # suelta todas las teclas y botones
    status
    raw kd 0x04              # orden cruda del protocolo del dongle
"""
from __future__ import annotations

import shlex
from dataclasses import dataclass, field

from .errors import UsageError
from .input import Kvm, abs_from_pixels, parse_screen
from .layouts import get_layout


@dataclass
class Context:
    kvm: Kvm
    relative: bool = False
    screen: tuple[int, int] | None = None
    sleep: callable = field(default=None)
    results: list = field(default_factory=list)   # salida de 'status'/'raw'


def do_move(kvm: Kvm, x: float, y: float, relative: bool, screen: tuple[int, int] | None):
    if relative:
        kvm.move_rel(int(x), int(y))
    elif screen:
        kvm.move_px(x, y, *screen)
    else:
        kvm.move_abs(int(x), int(y))


def _need(tok: list[str], lo: int, hi: int, usage: str):
    if not lo <= len(tok) - 1 <= hi:
        raise UsageError(f"uso: {usage}")


def execute(ctx: Context, tok: list[str]):
    cmd, kvm = tok[0].lower(), ctx.kvm
    if cmd == "type":
        _need(tok, 1, 99, "type TEXTO")
        kvm.type_text(" ".join(tok[1:]))
    elif cmd == "key":
        _need(tok, 1, 99, "key COMBO...")
        for c in tok[1:]:
            kvm.press(c)
    elif cmd == "keydown":
        _need(tok, 1, 1, "keydown COMBO")
        kvm.key_down_name(tok[1])
    elif cmd == "keyup":
        _need(tok, 1, 1, "keyup COMBO")
        kvm.key_up_name(tok[1])
    elif cmd == "move":
        _need(tok, 2, 2, "move X Y")
        do_move(kvm, float(tok[1]), float(tok[2]), ctx.relative, ctx.screen)
    elif cmd == "click":
        _need(tok, 0, 2, "click [BOTON] [VECES]")
        kvm.click(tok[1] if len(tok) > 1 else "left", int(tok[2]) if len(tok) > 2 else 1, ctx.relative)
    elif cmd in ("down", "up"):
        _need(tok, 0, 1, f"{cmd} [BOTON]")
        (kvm.button_down if cmd == "down" else kvm.button_up)(tok[1] if len(tok) > 1 else "left", ctx.relative)
    elif cmd == "scroll":
        _need(tok, 1, 1, "scroll N")
        kvm.scroll(int(tok[1]), ctx.relative)
    elif cmd == "sleep":
        _need(tok, 1, 1, "sleep SEGUNDOS")
        (ctx.sleep or kvm._sleep)(float(tok[1]))
    elif cmd == "release":
        kvm.release_all()
    elif cmd == "status":
        ctx.results.append(("status", kvm.status()))
    elif cmd == "raw":
        _need(tok, 1, 99, "raw ORDEN")
        ctx.results.append(("raw", kvm.raw(" ".join(tok[1:]))))
    elif cmd == "layout":
        _need(tok, 1, 1, "layout es|us")
        kvm.layout = get_layout(tok[1])
    elif cmd == "mouse-mode":
        _need(tok, 1, 1, "mouse-mode abs|rel")
        if tok[1] not in ("abs", "rel"):
            raise UsageError("mouse-mode: abs o rel")
        ctx.relative = tok[1] == "rel"
    elif cmd == "screen":
        _need(tok, 1, 1, "screen ANCHOxALTO")
        ctx.screen = parse_screen(tok[1])
    else:
        raise UsageError(f"orden de script desconocida: {cmd!r}")


def parse_script(text: str) -> list[tuple[int, list[str]]]:
    """Devuelve [(nº de línea, tokens)] ignorando vacías y comentarios. Valida la sintaxis de comillas."""
    out = []
    for n, line in enumerate(text.splitlines(), 1):
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        try:
            tok = shlex.split(s, comments=True)
        except ValueError as e:
            raise UsageError(f"línea {n}: {e}") from None
        if tok:
            out.append((n, tok))
    return out


def run_script(ctx: Context, text: str, on_step=None):
    """Ejecuta el script. Al acabar (o fallar) suelta teclas y botones."""
    steps = parse_script(text)
    try:
        for n, tok in steps:
            if on_step:
                on_step(n, tok)
            try:
                execute(ctx, tok)
            except UsageError as e:
                raise UsageError(f"línea {n}: {e}") from None
            except ValueError as e:
                raise UsageError(f"línea {n}: argumento inválido ({e})") from None
    finally:
        try:
            ctx.kvm.release_all()
        except Exception:
            pass
