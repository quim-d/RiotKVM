"""Errores del cliente y su código de salida de la CLI."""


class DkvmError(Exception):
    exit_code = 1


class UsageError(DkvmError):
    exit_code = 2


class DeviceNotFound(DkvmError):
    exit_code = 3


class DeviceError(DkvmError):
    """El dongle respondió 'err ...'."""
    exit_code = 4


class TransportError(DkvmError):
    """Fallo de E/S, timeout o respuesta ilegible."""
    exit_code = 5
