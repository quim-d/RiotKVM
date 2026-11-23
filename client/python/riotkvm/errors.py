"""Errores del cliente y su código de salida de la CLI."""


class RiotKvmError(Exception):
    exit_code = 1


class UsageError(RiotKvmError):
    exit_code = 2


class DeviceNotFound(RiotKvmError):
    exit_code = 3


class DeviceError(RiotKvmError):
    """El dongle respondió 'err ...'."""
    exit_code = 4


class TransportError(RiotKvmError):
    """Fallo de E/S, timeout o respuesta ilegible."""
    exit_code = 5
