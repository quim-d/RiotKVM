"""Flashea un firmware.factory.bin: `bootloader` por CDC -> esptool -> espera a que reaparezca VID_1209 PID_0001.
Uso: python tools/flash.py <bin>   (requiere .device.lock propio)"""
import subprocess
import sys
import time

import serial
import serial.tools.list_ports as lp

binf = sys.argv[1]


def find(vid, pid=None):
    for p in lp.comports():
        if p.vid == vid and (pid is None or p.pid == pid):
            return p.device


p = find(0x1209, 0x0001)
if p:
    s = serial.Serial(p, 115200, timeout=1)
    s.write(b"bootloader\n")
    time.sleep(0.5)
    try:
        s.close()
    except Exception:
        pass
rom = None
for _ in range(40):
    rom = find(0x303A)
    if rom:
        break
    time.sleep(0.25)
if not rom:
    sys.exit("no aparece el modo descarga (VID_303A)")
time.sleep(0.5)
r = subprocess.run([sys.executable, "-m", "esptool", "--port", rom, "--after", "watchdog-reset", "write-flash", "0x0", binf],
                   capture_output=True, text=True)
print([l for l in r.stdout.splitlines() if "Wrote" in l or "Hash" in l])
t0 = time.time()
while time.time() - t0 < 30:
    if find(0x1209, 0x0001):
        print("de vuelta en", find(0x1209, 0x0001), "tras %.1f s" % (time.time() - t0))
        break
    time.sleep(0.25)
else:
    sys.exit("no reaparece VID_1209")
