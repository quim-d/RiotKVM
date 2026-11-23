"""Pruebas sin hardware: python -m unittest discover -s tests (desde client/)."""
import json
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from click.testing import CliRunner

from riotkvm import protocol as P
from riotkvm.cli import cli
from riotkvm.errors import DeviceError, TransportError, UsageError
from riotkvm.input import Kvm, abs_from_pixels
from riotkvm.keys import parse_combo
from riotkvm.layouts import get_layout
from riotkvm.script import Context, parse_script, run_script
from riotkvm.transport import MockTransport, SerialTransport


def kvm(layout="es"):
    t = MockTransport()
    t.open()
    return Kvm(t, layout, sleep=lambda s: None), t


def taps(t):
    """Lista de usos pulsados en orden (kd)."""
    return [e[1] for e in t.events if e[0] == "kd"]


class ProtocolTests(unittest.TestCase):
    def test_encoding(self):
        self.assertEqual(P.key_down(0x04), "kd 0x04")
        self.assertEqual(P.mouse_rel(-5, 3, 0), "mm -5 3 0")
        self.assertEqual(P.mouse_abs(32767, 0), "ma 32767 0 0")
        self.assertEqual(P.buttons_abs(5), "mba 5")

    def test_ranges(self):
        for bad in (lambda: P.mouse_rel(128, 0), lambda: P.mouse_abs(-1, 0), lambda: P.mouse_abs(32768, 0),
                    lambda: P.buttons_abs(8), lambda: P.key_down(0), lambda: P.type_text("a\nb")):
            with self.assertRaises(UsageError):
                bad()

    def test_status(self):
        self.assertEqual(P.parse_status("host=1 leds=0x02 hidready=0"),
                         {"host": True, "leds": 2, "hidready": False})
        with self.assertRaises(ValueError):
            P.parse_status("basura")


class LayoutTests(unittest.TestCase):
    def test_us(self):
        us = get_layout("us")
        self.assertEqual(us.strokes("a"), [(0x04, 0)])
        self.assertEqual(us.strokes("A"), [(0x04, 0x02)])
        self.assertEqual(us.strokes("@"), [(0x1F, 0x02)])
        self.assertEqual(us.strokes(":"), [(0x33, 0x02)])

    def test_es(self):
        es = get_layout("es")
        self.assertEqual(es.strokes("@"), [(0x1F, 0x40)])      # AltGr+2
        self.assertEqual(es.strokes("ñ"), [(0x33, 0)])
        self.assertEqual(es.strokes('"'), [(0x1F, 0x02)])      # Shift+2
        self.assertEqual(es.strokes("{"), [(0x34, 0x40)])      # AltGr+´
        self.assertEqual(es.strokes("\\"), [(0x35, 0x40)])     # AltGr+º
        self.assertEqual(es.strokes("-"), [(0x38, 0)])
        self.assertEqual(es.strokes("á"), [(0x34, 0), (0x04, 0)])   # tecla muerta + a
        self.assertEqual(es.strokes("Ü"), [(0x34, 0x02), (0x18, 0x02)])
        self.assertEqual(es.strokes("`"), [(0x2F, 0), (0x2C, 0)])

    def test_unsupported(self):
        with self.assertRaises(UsageError):
            get_layout("us").strokes("ñ")
        with self.assertRaises(UsageError):
            get_layout("es").strokes("漢")

    def test_all_ascii_printable_covered(self):
        for name in ("us", "es"):
            lay = get_layout(name)
            for c in map(chr, range(32, 127)):
                lay.strokes(c)


class KeysTests(unittest.TestCase):
    def test_combo(self):
        es = get_layout("es")
        self.assertEqual(parse_combo("ctrl+alt+del", es), [(0xE0, 0), (0xE2, 0), (0x4C, 0)])
        self.assertEqual(parse_combo("F5", es), [(0x3E, 0)])
        self.assertEqual(parse_combo("win+r", es), [(0xE3, 0), (0x15, 0)])
        self.assertEqual(parse_combo("ctrl+@", es), [(0xE0, 0), (0x1F, 0x40)])
        self.assertEqual(parse_combo("ctrl++", es)[-1], es.strokes("+")[0])

    def test_errors(self):
        es = get_layout("es")
        for bad in ("", "ctrl+", "ctrl+nada", "0xZZ"):
            with self.assertRaises(UsageError):
                parse_combo(bad, es)


class KvmTests(unittest.TestCase):
    def test_type_text_us(self):
        k, t = kvm("us")
        k.type_text("Hi")
        self.assertEqual(t.sent, ["kd 0xE1", "kd 0x0B", "ku 0x0B", "ku 0xE1", "kd 0x0C", "ku 0x0C"])
        self.assertEqual(t.keys, set())

    def test_type_validates_before_sending(self):
        k, t = kvm("us")
        with self.assertRaises(UsageError):
            k.type_text("ok ñ")
        self.assertEqual(t.sent, [])

    def test_newline_is_enter(self):
        k, t = kvm("es")
        k.type_text("a\r\nb")
        self.assertEqual(taps(t), [0x04, 0x28, 0x05])

    def test_press_releases_in_reverse(self):
        k, t = kvm()
        k.press("ctrl+alt+del")
        self.assertEqual(t.sent, ["kd 0xE0", "kd 0xE2", "kd 0x4C", "ku 0x4C", "ku 0xE2", "ku 0xE0"])
        self.assertEqual(t.keys, set())

    def test_press_releases_on_error(self):
        k, t = kvm()
        t.fail_next = None
        orig = t._handle
        calls = {"n": 0}

        def flaky(line):
            calls["n"] += 1
            return "err boom" if line == "kd 0x4C" else orig(line)
        t._handle = flaky
        with self.assertRaises(DeviceError):
            k.press("ctrl+del")
        self.assertIn("ku 0xE0", t.sent)

    def test_move_rel_chunks(self):
        k, t = kvm()
        k.move_rel(300, -200)
        self.assertEqual(t.rel_total, [300, -200])
        self.assertTrue(all(abs(e[1]) <= 127 and abs(e[2]) <= 127 for e in t.events))

    def test_abs_mapping(self):
        self.assertEqual(abs_from_pixels(0, 0, 1920, 1080), (0, 0))
        self.assertEqual(abs_from_pixels(1919, 1079, 1920, 1080), (32767, 32767))
        self.assertEqual(abs_from_pixels(5000, -4, 1920, 1080), (32767, 0))
        x, y = abs_from_pixels(960, 540, 1920, 1080)
        self.assertAlmostEqual(x, 16384, delta=10)

    def test_click_and_buttons(self):
        k, t = kvm()
        k.click("right", 2)
        self.assertEqual(t.sent, ["mba 2", "mba 0", "mba 2", "mba 0"])
        k.click("left", relative=True)
        self.assertEqual(t.sent[-2:], ["mbr 1", "mbr 0"])
        k.button_down("left")
        k.button_down("middle")
        self.assertEqual(t.abs_buttons, 5)
        k.release_all()
        self.assertEqual((t.abs_buttons, t.rel_buttons, k.buttons), (0, 0, 0))

    def test_scroll_abs_keeps_position(self):
        k, t = kvm()
        k.move_abs(100, 200)
        k.scroll(-3)
        self.assertEqual(t.events[-1], ("ma", 100, 200, -3))

    def test_status(self):
        k, t = kvm()
        self.assertTrue(k.status()["hidready"])

    def test_unknown_command_is_error(self):
        k, t = kvm()
        with self.assertRaises(DeviceError):
            k.raw("nope")


class ScriptTests(unittest.TestCase):
    SCRIPT = '''
# demo
layout us
screen 1920x1080
mouse-mode abs
move 960 540
click left 2
type "Hi there"
key ctrl+a
sleep 0.1
status
'''

    def test_run(self):
        k, t = kvm()
        ctx = Context(k, sleep=lambda s: None)
        run_script(ctx, self.SCRIPT)
        self.assertEqual(ctx.results[0][0], "status")
        self.assertEqual(t.keys, set())
        self.assertEqual(t.sent[-3:], ["kall", "mbr 0", "mba 0"])   # liberación final
        self.assertEqual(t.events[0][0], "ma")

    def test_errors_have_line_numbers_and_release(self):
        k, t = kvm()
        with self.assertRaises(UsageError) as cm:
            run_script(Context(k), "keydown shift\nfoo bar\n")
        self.assertIn("línea 2", str(cm.exception))
        self.assertEqual(t.keys, set())   # shift soltado pese al fallo

    def test_bad_quotes(self):
        with self.assertRaises(UsageError):
            parse_script('type "sin cerrar')


class FakeSerial:
    """Sustituye a serial.Serial: responde 'ok' a cada línea, con 'err' donde se pida."""
    instances = []

    def __init__(self, port, baud, timeout=None, write_timeout=None):
        self.port, self.buf, self.lines, self.closed = port, b"", [], False
        self.err_on = None
        self.silent = False
        FakeSerial.instances.append(self)

    def reset_input_buffer(self):
        self.buf = b""

    def write(self, data):
        for ln in data.decode().splitlines():
            self.lines.append(ln)
            if self.silent:
                continue
            if ln == "status":
                self.buf += b"host=1 leds=0x00 hidready=1\r\n"
            elif ln == self.err_on:
                self.buf += b"err comando desconocido\r\n"
            else:
                self.buf += b"ok\r\n"
        return len(data)

    def readline(self):
        i = self.buf.find(b"\n")
        if i < 0:
            return b""   # timeout
        out, self.buf = self.buf[:i + 1], self.buf[i + 1:]
        return out

    def close(self):
        self.closed = True


class SerialTransportTests(unittest.TestCase):
    def setUp(self):
        FakeSerial.instances.clear()
        p = mock.patch("serial.Serial", FakeSerial)
        p.start()
        self.addCleanup(p.stop)

    def test_pipelining_and_flush(self):
        t = SerialTransport("COMX", max_pending=4)
        t.open()
        for i in range(10):
            t.send(f"kd 0x{i + 4:02X}", wait=False)
        self.assertLessEqual(len(t._pending), 4)
        t.flush()
        self.assertEqual(t._pending, [])
        self.assertEqual(t.send("status"), "host=1 leds=0x00 hidready=1")
        t.close()
        self.assertTrue(FakeSerial.instances[0].closed)

    def test_err_reported_without_desync(self):
        t = SerialTransport("COMX")
        t.open()
        FakeSerial.instances[0].err_on = "ku 0x05"
        t.send("kd 0x04", wait=False)
        t.send("ku 0x05", wait=False)
        t.send("kd 0x06", wait=False)
        with self.assertRaises(DeviceError):
            t.flush()
        self.assertEqual(t.send("status"), "host=1 leds=0x00 hidready=1")   # sigue sincronizado

    def test_timeout(self):
        t = SerialTransport("COMX")
        t.open()
        FakeSerial.instances[0].silent = True
        with self.assertRaises(TransportError):
            t.send("status")

    def test_autodetect(self):
        ports = [{"port": "COM3", "is_dongle": False}, {"port": "COM9", "is_dongle": True}]
        with mock.patch("riotkvm.transport.serial_cdc.list_ports", return_value=ports):
            t = SerialTransport()
            t.open()
        self.assertEqual(t.port, "COM9")

    def test_no_dongle(self):
        from riotkvm.errors import DeviceNotFound
        with mock.patch("riotkvm.transport.serial_cdc.list_ports", return_value=[]):
            with self.assertRaises(DeviceNotFound):
                SerialTransport().open()


class CliTests(unittest.TestCase):
    def invoke(self, *args, input=None):
        mt = MockTransport()
        r = CliRunner()
        with mock.patch("riotkvm.cli.MockTransport", return_value=mt):
            res = r.invoke(cli, ["-p", "mock", *args], input=input)
        return res, mt

    def test_status_json(self):
        res, _ = self.invoke("--json", "status")
        self.assertEqual(res.exit_code, 0, res.output)
        d = json.loads(res.stdout)
        self.assertTrue(d["ok"] and d["hidready"])

    def test_type_stdin(self):
        res, mt = self.invoke("-l", "us", "type", input="ab")
        self.assertEqual(res.exit_code, 0, res.output)
        self.assertEqual(taps(mt), [0x04, 0x05])

    def test_type_enter_and_fw(self):
        res, mt = self.invoke("type", "--fw", "--enter", "hola")
        self.assertEqual(res.exit_code, 0, res.output)
        self.assertEqual(mt.typed, ["hola"])
        self.assertIn(0x28, taps(mt))

    def test_key(self):
        res, mt = self.invoke("key", "ctrl+alt+del", "enter")
        self.assertEqual(res.exit_code, 0, res.output)
        self.assertEqual(taps(mt), [0xE0, 0xE2, 0x4C, 0x28])

    def test_mouse_and_click(self):
        res, mt = self.invoke("--screen", "1920x1080", "click", "--at", "1919", "1079", "-n", "2")
        self.assertEqual(res.exit_code, 0, res.output)
        self.assertEqual(mt.abs_pos, (32767, 32767))
        self.assertEqual(mt.sent.count("mba 1"), 2)
        res, mt = self.invoke("mouse", "--rel", "200", "-5")
        self.assertEqual(mt.rel_total, [200, -5])

    def test_exit_codes(self):
        res, _ = self.invoke("--json", "key", "ctrl+nada")
        self.assertEqual(res.exit_code, 2)
        self.assertFalse(json.loads(res.stdout)["ok"])
        res, _ = self.invoke("raw", "nope")
        self.assertEqual(res.exit_code, 4)
        res, _ = self.invoke("mouse", "99999", "0")
        self.assertEqual(res.exit_code, 2)
        res, _ = self.invoke("--layout", "xx", "type", "a")
        self.assertEqual(res.exit_code, 2)

    def test_no_dongle_exit_3(self):
        from riotkvm.errors import DeviceNotFound
        with mock.patch("riotkvm.transport.serial_cdc.list_ports", return_value=[]):
            res = CliRunner().invoke(cli, ["status"])
        self.assertEqual(res.exit_code, 3)
        with mock.patch("riotkvm.cli.list_ports", return_value=[]):
            res = CliRunner().invoke(cli, ["--json", "scan"])
        self.assertEqual(res.exit_code, 3)

    def test_scan_finds(self):
        ports = [{"port": "COM9", "vid": 0x1209, "pid": 1, "description": "x", "serial_number": "1", "is_dongle": True}]
        with mock.patch("riotkvm.cli.list_ports", return_value=ports):
            res = CliRunner().invoke(cli, ["--json", "scan"])
        self.assertEqual(res.exit_code, 0)
        self.assertEqual(json.loads(res.stdout)["ports"][0]["port"], "COM9")

    def test_run_script_file_and_dry_run(self):
        with tempfile.NamedTemporaryFile("w", suffix=".riotkvm", delete=False, encoding="utf-8") as f:
            f.write('layout us\ntype "a"\nstatus\n')
        try:
            res = CliRunner().invoke(cli, ["--json", "run", "--dry-run", f.name])
            self.assertEqual(res.exit_code, 0, res.output)
            d = json.loads(res.stdout.strip().splitlines()[-1])
            self.assertEqual(d["results"][0]["cmd"], "status")
        finally:
            os.unlink(f.name)

    def test_run_bad_script_exit_2(self):
        res, _ = self.invoke("run", "-", input="nonsense 1\n")
        self.assertEqual(res.exit_code, 2)

    def test_completion(self):
        res = CliRunner().invoke(cli, ["completion", "bash"])
        self.assertEqual(res.exit_code, 0)
        self.assertIn("_RIOTKVM_COMPLETE", res.output)


if __name__ == "__main__":
    unittest.main()
