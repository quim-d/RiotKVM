"""CLI de riotkvm. Códigos de salida: 0 ok, 1 error, 2 uso, 3 dongle no encontrado, 4 el dongle dijo 'err', 5 fallo de E/S/timeout."""
from __future__ import annotations

import json
import os
import sys
from functools import wraps

import click

from . import __version__
from .errors import RiotKvmError, UsageError
from .input import Kvm, parse_screen
from .script import Context, do_move, run_script
from .transport import MockTransport, SerialTransport, list_ports


class App:
    def __init__(self, port, layout, as_json, verbose, screen):
        self.port, self.layout, self.json, self.verbose, self.screen = port, layout, as_json, verbose, screen
        self.transport_factory = None   # inyectable en tests

    def make_transport(self):
        if self.transport_factory:
            return self.transport_factory()
        echo = (lambda l: click.echo(f"> {l}", err=True)) if self.verbose else None
        if self.port == "mock":
            return MockTransport(echo=echo or (lambda l: click.echo(f"> {l}", err=True)))
        return SerialTransport(self.port)

    def emit(self, data: dict, text: str | None = None):
        if self.json:
            click.echo(json.dumps({"ok": True, **data}, ensure_ascii=False))
        elif text:
            click.echo(text)


def _fail(app_json: bool, e: Exception, code: int):
    if app_json:
        click.echo(json.dumps({"ok": False, "error": str(e), "code": code}, ensure_ascii=False))
    else:
        click.echo(f"riotkvm: error: {e}", err=True)
    sys.exit(code)


def with_kvm(fn):
    """Abre el transporte, ejecuta y cierra; traduce excepciones a códigos de salida."""
    @wraps(fn)
    @click.pass_obj
    def wrapper(app: App, *a, **kw):
        try:
            with app.make_transport() as t:
                kvm = Kvm(t, app.layout)
                fn(app, kvm, *a, **kw)
        except RiotKvmError as e:
            _fail(app.json, e, e.exit_code)
    return wrapper


@click.group(context_settings={"help_option_names": ["-h", "--help"]})
@click.option("--port", "-p", envvar=["RIOTKVM_PORT", "DKVM_PORT"], default=None, help="Puerto serie (COM5, /dev/ttyACM0) o 'mock'. Por defecto: autodetecta el dongle.")
@click.option("--layout", "-l", envvar=["RIOTKVM_LAYOUT", "DKVM_LAYOUT"], default="es", show_default=True, help="Distribución de teclado del equipo DESTINO (es, us).")
@click.option("--screen", envvar=["RIOTKVM_SCREEN", "DKVM_SCREEN"], default=None, help="Resolución del destino ANCHOxALTO: las coordenadas absolutas pasan a ser píxeles.")
@click.option("--json", "as_json", is_flag=True, help="Salida JSON en stdout (errores incluidos).")
@click.option("--verbose", "-v", is_flag=True, help="Vuelca las órdenes enviadas a stderr.")
@click.version_option(__version__, prog_name="riotkvm")
@click.pass_context
def cli(ctx, port, layout, screen, as_json, verbose):
    """riotkvm: controla el dongle KVM (teclado y ratón del servidor)."""
    try:
        scr = parse_screen(screen) if screen else None
    except UsageError as e:
        raise click.UsageError(str(e))
    ctx.obj = App(port, layout, as_json, verbose, scr)


@cli.command()
@click.pass_obj
def scan(app: App):
    """Lista los puertos serie y marca el dongle (VID 1209:0001)."""
    ports = list_ports()
    if app.json:
        click.echo(json.dumps({"ok": True, "ports": ports}, ensure_ascii=False))
    else:
        for p in ports:
            mark = "*" if p["is_dongle"] else " "
            vp = f"{p['vid']:04X}:{p['pid']:04X}" if p["vid"] is not None else "----:----"
            click.echo(f"{mark} {p['port']:<10} {vp}  {p['description']}")
        if not any(p["is_dongle"] for p in ports):
            click.echo("(ningún dongle detectado)", err=True)
    if not any(p["is_dongle"] for p in ports):
        sys.exit(3)


@cli.command()
@with_kvm
def status(app, kvm):
    """Estado del dongle: host USB visto, LEDs del teclado, HID listo."""
    s = kvm.status()
    app.emit(s, " ".join(f"{k}={int(v) if isinstance(v, bool) else v}" for k, v in s.items()))


@cli.command("type")
@click.argument("text", required=False)
@click.option("--delay", type=float, default=0.0, help="Pausa entre caracteres (s).")
@click.option("--enter", is_flag=True, help="Pulsa Enter al final.")
@click.option("--fw", is_flag=True, help="Usa 'type' del firmware (layout US fijo) en vez de kd/ku con el layout.")
@with_kvm
def type_(app, kvm, text, delay, enter, fw):
    """Escribe TEXTO en el servidor. Sin TEXTO o con '-', lee stdin."""
    if text is None or text == "-":
        text = sys.stdin.read()
    if enter:
        text += "\n"
    if fw:
        kvm.type_text_firmware(text)
    else:
        kvm.type_text(text, delay)
    app.emit({"typed": len(text)}, None)


@cli.command()
@click.argument("combos", nargs=-1, required=True)
@click.option("--hold", type=float, default=0.0, help="Segundos que se mantiene cada combinación.")
@click.option("--down", is_flag=True, help="Solo pulsar (no soltar).")
@click.option("--up", is_flag=True, help="Solo soltar.")
@with_kvm
def key(app, kvm, combos, hold, down, up):
    """Pulsa combinaciones: riotkvm key ctrl+alt+del  |  riotkvm key win+r  |  riotkvm key f5 enter"""
    if down and up:
        raise UsageError("--down y --up son excluyentes")
    for c in combos:
        if down:
            kvm.key_down_name(c)
        elif up:
            kvm.key_up_name(c)
        else:
            kvm.press(c, hold)
    app.emit({"keys": list(combos)})


@cli.command()
@with_kvm
def release(app, kvm):
    """Suelta todas las teclas y botones (por si algo se quedó pulsado)."""
    kvm.release_all()
    app.emit({"released": True})


@cli.command(context_settings={"ignore_unknown_options": True})
@click.argument("x", type=float)
@click.argument("y", type=float)
@click.option("--rel", "relative", is_flag=True, help="Movimiento relativo (dx dy) en vez de posición absoluta.")
@with_kvm
def mouse(app, kvm, x, y, relative):
    """Mueve el ratón. Absoluto: X Y en 0..32767 (o píxeles con --screen). Relativo: dx dy."""
    do_move(kvm, x, y, relative, app.screen)
    app.emit({"x": x, "y": y, "relative": relative})


@cli.command("click")
@click.argument("button", default="left", type=click.Choice(["left", "right", "middle"]))
@click.option("--count", "-n", type=int, default=1, show_default=True)
@click.option("--at", nargs=2, type=float, default=None, metavar="X Y", help="Mueve a X Y (absoluto) antes de hacer clic.")
@click.option("--rel", "relative", is_flag=True, help="Usa los botones del ratón relativo.")
@click.option("--interval", type=float, default=0.05, show_default=True, help="Segundos entre clics múltiples.")
@with_kvm
def click_(app, kvm, button, count, at, relative, interval):
    """Clic: riotkvm click [left|right|middle] [-n 2] [--at X Y]"""
    if at:
        do_move(kvm, at[0], at[1], False, app.screen)
    kvm.click(button, count, relative, interval)
    app.emit({"button": button, "count": count})


@cli.command(context_settings={"ignore_unknown_options": True})
@click.argument("amount", type=int)
@click.option("--rel", "relative", is_flag=True)
@with_kvm
def scroll(app, kvm, amount, relative):
    """Rueda del ratón: positivo = arriba."""
    kvm.scroll(amount, relative)
    app.emit({"scroll": amount})


@cli.command()
@click.argument("line", nargs=-1, required=True)
@with_kvm
def raw(app, kvm, line):
    """Envía una orden cruda del protocolo del dongle (depuración): riotkvm raw kd 0x04"""
    reply = kvm.raw(" ".join(line))
    app.emit({"reply": reply}, reply)


@cli.command()
@click.argument("script", type=click.File("r", encoding="utf-8"))
@click.option("--dry-run", is_flag=True, help="No toca el dongle: valida y muestra las órdenes que enviaría.")
@click.option("--mouse-mode", type=click.Choice(["abs", "rel"]), default="abs", show_default=True)
@click.pass_obj
def run(app: App, script, dry_run, mouse_mode):
    """Ejecuta una macro (fichero o '-' para stdin). Formato en 'riotkvm run --help' y riotkvm/script.py."""
    text = script.read()
    try:
        if dry_run:
            app.transport_factory = lambda: MockTransport(echo=lambda l: click.echo(f"> {l}", err=app.json))
        with app.make_transport() as t:
            kvm = Kvm(t, app.layout)
            ctx = Context(kvm, relative=mouse_mode == "rel", screen=app.screen)
            run_script(ctx, text, on_step=(lambda n, tok: click.echo(f"# {n}: {' '.join(tok)}", err=True)) if app.verbose else None)
        app.emit({"results": [{"cmd": c, "value": v} for c, v in ctx.results]},
                 "\n".join(f"{c}: {v}" for c, v in ctx.results) or None)
    except RiotKvmError as e:
        _fail(app.json, e, e.exit_code)


@cli.command()
@click.argument("shell", type=click.Choice(["bash", "zsh", "fish"]))
def completion(shell):
    """Imprime el script de autocompletado. Ej.: eval "$(riotkvm completion bash)"."""
    from click.shell_completion import get_completion_class
    cls = get_completion_class(shell)
    click.echo(cls(cli, {}, "riotkvm", "_RIOTKVM_COMPLETE").source())


def main():
    cli(prog_name="riotkvm")


if __name__ == "__main__":
    main()
