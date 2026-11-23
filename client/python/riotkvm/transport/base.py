"""Interfaz de transporte. Serie hoy, BLE mañana, mock para pruebas.

Contrato (síncrono, una línea de texto por orden):
  send(line, wait=True)  -> respuesta ('ok', 'host=1 ...'); con wait=False devuelve None
  flush()                -> espera las respuestas pendientes de los envíos wait=False
  Una respuesta 'err ...' lanza DeviceError (en flush, la primera que aparezca).
Un transporte BLE implementará lo mismo; si el protocolo BLE es binario, la
traducción línea -> trama vive dentro de ese transporte (o en una capa de
codificación), sin tocar input/cli.
"""
from __future__ import annotations

from abc import ABC, abstractmethod

from ..errors import DeviceError


class Transport(ABC):
    name = "?"

    @abstractmethod
    def open(self) -> None: ...

    @abstractmethod
    def close(self) -> None: ...

    @abstractmethod
    def send(self, line: str, wait: bool = True) -> str | None: ...

    def flush(self) -> None:
        """Por defecto no hay nada pendiente."""

    def __enter__(self):
        self.open()
        return self

    def __exit__(self, exc_type, exc, tb):
        try:
            self.flush()
        except Exception:
            if exc_type is None:
                raise
        finally:
            self.close()


def check_reply(line: str, reply: str) -> str:
    if reply.startswith("err"):
        raise DeviceError(f"el dongle rechazó {line!r}: {reply}")
    return reply
