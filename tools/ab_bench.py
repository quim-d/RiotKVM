"""Banco A/B pasivo de coste de la UI (lcd-dev). Solo CDC `status`/`stats`/`ui`/`cpu` + ficheros propios en <SD>:\\tests_tmp
con prioridad baja (tests/loadgen.py). NO inyecta teclado ni raton.
Uso: python tools/ab_bench.py <etiqueta> [segundos_por_fase=20] [salida.json]
Fases: idle (sin carga), write (carga SD de escritura), read (carga SD de lectura).
Por fase: RTT de `status` cada ~10 ms (p50/p99/max), throughput de la carga, `stats` (sd_cb_max_us, sd_op_max_us, snd_*), `ui`, `cpu`."""
import json
import os
import subprocess
import sys
import time

import serial

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tests", "lib"))
import harness as H  # noqa: E402

label = sys.argv[1]
secs = float(sys.argv[2]) if len(sys.argv) > 2 else 20
out = sys.argv[3] if len(sys.argv) > 3 else None

port, sn = H.find_dongle()
if not port:
    sys.exit("dongle (VID_1209 PID_0001) no encontrado")
ser = serial.Serial(port, 115200, timeout=0.5, write_timeout=2)
time.sleep(0.3)
ser.reset_input_buffer()


def cmd(c, n=1, to=1.0):
    """envia c y devuelve (rtt_ms, [lineas]) con n lineas de respuesta"""
    t0 = time.perf_counter()
    ser.write((c + "\n").encode())
    lines = []
    end = t0 + to
    while len(lines) < n and time.perf_counter() < end:
        ln = ser.readline()
        if ln:
            lines.append(ln.decode(errors="replace").strip())
    return (time.perf_counter() - t0) * 1000, lines


def kv(line):
    d = {}
    for tok in line.split():
        if "=" in tok:
            k, v = tok.split("=", 1)
            d[k] = v
    return d


def phase(name, load):
    cmd("stats reset")
    cmd("ui reset")
    cmd("cpu")                       # cebar la ventana de CPU
    proc = None
    stopf = os.path.join(os.environ.get("TEMP", "."), "ab_stop_%d" % os.getpid())
    if load:
        drv = H.find_msc_drive(sn)
        if not drv:
            sys.exit("unidad SD del dongle no encontrada")
        flags = 0x00004000  # BELOW_NORMAL_PRIORITY_CLASS
        proc = subprocess.Popen([sys.executable, os.path.join(ROOT, "tests", "loadgen.py"), drv, load, str(secs + 8), stopf],
                                stdout=subprocess.PIPE, text=True, creationflags=flags)
        proc.stdout.readline()       # "READY"
    rtts, lost = [], 0
    t_end = time.perf_counter() + secs
    while time.perf_counter() < t_end:
        ms, l = cmd("status")
        if l and l[0].startswith("host="):
            rtts.append(ms)
        else:
            lost += 1
        time.sleep(0.010)
    res = {"phase": name, "rtt": H.summarize(rtts, lost)}
    if proc:
        open(stopf, "w").close()
        o = proc.communicate(timeout=60)[0].strip().splitlines()
        os.remove(stopf)
        try:
            j = json.loads(o[-1])
            res["load_MBps"] = j["write_MBps"] if load == "write" else j["read_MBps"]
        except Exception:
            res["load_raw"] = o[-1:]
    _, st = cmd("stats", 2)
    res["stats"] = {**kv(st[0]), **kv(st[1])} if len(st) == 2 else st
    keep = ("sd_cb_max_us", "sd_op_max_us", "sd_kb", "sd_err", "sd_retry", "snd_n", "snd_fail", "snd_avg_us", "snd_max_us", "cmd_max_us")
    res["stats"] = {k: v for k, v in res["stats"].items() if k in keep} if isinstance(res["stats"], dict) else res["stats"]
    _, u = cmd("ui")
    res["ui"] = u[0] if u else None
    _, c = cmd("cpu")
    res["cpu"] = c[0] if c else None
    time.sleep(1.0)
    return res


_, info = cmd("info")
result = {"label": label, "secs": secs, "info": info[0] if info else None, "phases": []}
for name, load in (("idle", None), ("write", "write"), ("read", "read")):
    r = phase(name, load)
    result["phases"].append(r)
    print(json.dumps(r), flush=True)
ser.close()
if out:
    json.dump(result, open(out, "w"), indent=1)
