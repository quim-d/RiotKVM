"""Transporte por el CDC del dongle (pyserial).

- Autodescubrimiento por VID 0x1209 / PID 0x0001 (modo normal del dongle).
- Pipelining: send(wait=False) escribe sin esperar el 'ok'; las respuestas se
  consumen en flush() o antes de la siguiente orden con respuesta. La ventana
  (max_pending) acota lo que hay en vuelo para no desbordar el búfer RX del dongle.
- NO se baja DTR: el CDC nativo de ESP32 solo escribe con DTR activo.
"""
from __future__ import annotations

from ..errors import DeviceError, DeviceNotFound, TransportError
from .base import Transport

VID, PID = 0x1209, 0x0001


def list_ports() -> list[dict]:
    from serial.tools import list_ports as lp
    return [{
        "port": p.device,
        "vid": p.vid,
        "pid": p.pid,
        "description": p.description,
        "serial_number": p.serial_number,
        "is_dongle": p.vid == VID and p.pid == PID,
    } for p in lp.comports()]


def find_dongle_port() -> str:
    found = [p["port"] for p in list_ports() if p["is_dongle"]]
    if not found:
        raise DeviceNotFound("no hay ningún dongle (VID 1209:0001) conectado; usa --port o 'riotkvm scan'")
    if len(found) > 1:
        raise DeviceNotFound(f"varios dongles ({', '.join(found)}); elige con --port")
    return found[0]


class SerialTransport(Transport):
    name = "serial"

    def __init__(self, port: str | None = None, timeout: float = 1.0, max_pending: int = 16):
        self.port = port
        self.timeout = timeout
        self.max_pending = max_pending
        self._ser = None
        self._pending: list[str] = []   # órdenes enviadas cuya respuesta aún no se ha leído

    def open(self):
        import serial
        if self.port is None:
            self.port = find_dongle_port()
        try:
            self._ser = serial.Serial(self.port, 115200, timeout=self.timeout, write_timeout=self.timeout)
        except serial.SerialException as e:
            raise DeviceNotFound(f"no se pudo abrir {self.port}: {e}") from e
        self._ser.reset_input_buffer()
        self._pending.clear()

    def close(self):
        if self._ser is not None:
            try:
                self._ser.close()
            finally:
                self._ser = None

    def _read_reply(self) -> str:
        raw = self._ser.readline()
        if not raw.endswith(b"\n"):
            raise TransportError("timeout esperando la respuesta del dongle")
        return raw.decode("utf-8", "replace").strip()

    def _drain(self, keep: int = 0) -> str:
        """Lee respuestas hasta dejar 'keep' pendientes. Devuelve la última leída;
        lanza DeviceError con el primer 'err' (tras leer todas, para no desincronizar)."""
        first_err = None
        last = ""
        while len(self._pending) > keep:
            line = self._pending.pop(0)
            last = self._read_reply()
            if last.startswith("err") and first_err is None:
                first_err = DeviceError(f"el dongle rechazó {line!r}: {last}")
        if first_err:
            raise first_err
        return last

    def send(self, line: str, wait: bool = True):
        if self._ser is None:
            raise TransportError("transporte serie no abierto")
        if "\n" in line or "\r" in line:
            raise TransportError("la orden no puede contener saltos de línea")
        try:
            self._ser.write(line.encode("utf-8") + b"\n")
            self._pending.append(line)
            if wait:
                return self._drain()
            self._drain(keep=self.max_pending)
            return None
        except OSError as e:
            raise TransportError(f"error de E/S en {self.port}: {e}") from e

    def flush(self):
        if self._ser is not None:
            self._drain()
