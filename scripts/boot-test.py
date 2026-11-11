#!/usr/bin/env python3
"""Boot an Exec85 ISO in QEMU and test it like a user would.

  1. wait for the first screen (EXEC85_READY) and the idle memory report (EXEC85_MEM)
  2. press real keys through QEMU's virtual keyboard (monitor `sendkey`)
  3. after every key, check the shell's state line (EXEC85_STATE ...) on the serial port
  4. take screenshots and check pixels (theme colours)

usage: boot-test.py ISO bios|uefi OUTDIR [--accel kvm|tcg]
Exit code 0 = everything passed.
"""
import glob, json, os, re, socket, struct, subprocess, sys, time, zlib

iso, firmware, outdir = sys.argv[1:4]
accel = sys.argv[sys.argv.index("--accel") + 1] if "--accel" in sys.argv else ("kvm" if os.access("/dev/kvm", os.W_OK) else "tcg")
os.makedirs(outdir, exist_ok=True)
serial_log = os.path.join(outdir, f"serial-{firmware}.log")
mon_path = os.path.join(outdir, f"mon-{firmware}.sock")
for p in (serial_log, mon_path):
    if os.path.exists(p): os.remove(p)

cmd = ["qemu-system-x86_64", "-machine", "q35", "-accel", accel, "-m", "512", "-smp", "2",
       "-cdrom", iso, "-boot", "d", "-vga", "std", "-display", "none", "-no-reboot",
       "-serial", f"file:{serial_log}", "-monitor", f"unix:{mon_path},server,nowait"]
if firmware == "uefi":
    code = next(iter(glob.glob("/usr/share/OVMF/OVMF_CODE_4M.fd") + glob.glob("/usr/share/OVMF/OVMF_CODE.fd") + glob.glob("/usr/share/ovmf/OVMF.fd") + glob.glob("/usr/share/qemu/edk2-x86_64-code.fd")), None)
    if not code: sys.exit("OVMF firmware not found")
    cmd += ["-drive", f"if=pflash,format=raw,readonly=on,file={code}"]

SLOW = accel == "tcg"
STEP_TIMEOUT = 60 if SLOW else 20
t0 = time.time()
q = subprocess.Popen(cmd)
result = {"firmware": firmware, "accel": accel, "booted": False, "steps": []}


def log_text():
    try: return open(serial_log, errors="replace").read()
    except FileNotFoundError: return ""

STATE_KEYS = ("dir", "index", "name", "count", "drive", "focus", "menu", "item", "dialog", "theme", "sort", "hidden", "layout", "details")

def states():
    """Every complete EXEC85_STATE line so far. A line that a kernel message cut in half is ignored."""
    out = []
    for line in log_text().splitlines():
        if "EXEC85_STATE " in line:
            d = dict(kv.split("=", 1) for kv in line.split("EXEC85_STATE ", 1)[1].split() if "=" in kv)
            if all(k in d for k in STATE_KEYS) and d["details"] in ("true", "false"):
                out.append(d)
    return out

def wait_for(pattern, timeout):
    end = time.time() + timeout
    while time.time() < end:
        m = re.search(pattern, log_text())
        if m: return m
        if q.poll() is not None: break
        time.sleep(0.1)
    return None

mon = None
def monitor(command):
    global mon
    if mon is None:
        mon = socket.socket(socket.AF_UNIX); mon.settimeout(10); mon.connect(mon_path); mon.recv(4096)
    mon.sendall((command + "\n").encode()); time.sleep(0.15)
    try: mon.settimeout(0.2); mon.recv(65536)
    except Exception: pass
    mon.settimeout(10)

def read_ppm(path):
    data = open(path, "rb").read()
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", data)
    return int(m[1]), int(m[2]), data[m.end():]

def write_png(path, w, h, rgb):
    raw = b"".join(b"\x00" + rgb[y*w*3:(y+1)*w*3] for y in range(h))
    def chunk(t, d): c = struct.pack(">I", len(d)) + t + d; return c + struct.pack(">I", zlib.crc32(t + d))
    open(path, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))

def screenshot(name):
    ppm = os.path.join(outdir, f"{firmware}-{name}.ppm")
    if os.path.exists(ppm): os.remove(ppm)
    monitor(f"screendump {ppm}")
    last, end = -1, time.time() + 15
    while time.time() < end:
        size = os.path.getsize(ppm) if os.path.exists(ppm) else 0
        if size > 0 and size == last: break
        last = size; time.sleep(0.4)
    w, h, rgb = read_ppm(ppm)
    write_png(os.path.join(outdir, f"{firmware}-{name}.png"), w, h, rgb)
    os.remove(ppm)
    return w, h, rgb

def bar_pixel(shot):
    w, h, rgb = shot
    o = (2 * w + w // 2) * 3          # top of the menu bar, horizontally centred
    return tuple(rgb[o:o + 3])

DARK_BAR, LIGHT_BAR = (0x1F, 0x3B, 0x4D), (0xC9, 0xBF, 0xA3)

def step(name, ok, detail=""):
    result["steps"].append({"step": name, "ok": bool(ok), "detail": detail})
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""))
    if not ok: raise AssertionError(f"{name}: {detail}")

def press(keyname, **expect):
    """Press a key in the VM, wait for the shell to report its new state, check it."""
    before = len(states())
    monitor(f"sendkey {keyname}")
    end = time.time() + STEP_TIMEOUT
    while len(states()) <= before and time.time() < end: time.sleep(0.1)
    st = states()
    ok = len(st) > before
    cur = st[-1] if st else {}
    for k, v in expect.items():
        ok = ok and cur.get(k) == v
    label = f"key {keyname}" + "".join(f" -> {k}={v}" for k, v in expect.items())
    step(label, ok, f"got {({k: cur.get(k) for k in expect} if expect else 'no state change')}" if not ok else "")
    return cur

failure = None
try:
    m = wait_for(r"EXEC85_READY uptime=([\d.]+)", 120 if not SLOW else 600)
    step("first screen drawn", m is not None, "no EXEC85_READY on serial")
    ready_wall = time.time() - t0
    result.update(booted=True, kernel_uptime_at_ready_s=float(m[1]), host_seconds_to_ready=round(ready_wall, 2))
    d = wait_for(r"EXEC85_DISPLAY (\w+) (\d+)x(\d+)", 10)
    if d: result.update(display_driver=d[1], screen=f"{d[2]}x{d[3]}")
    mm = wait_for(r"EXEC85_MEM total_kb=(\d+) used_kb=(\d+) exec85_rss_kb=(\d+)", 120)
    step("idle memory report", mm is not None, "no EXEC85_MEM")
    result.update(mem_total_kb=int(mm[1]), mem_used_kb=int(mm[2]), exec85_rss_kb=int(mm[3]))

    # Establish a known baseline with two real key presses (does not depend on the very first state line).
    press("tab", focus="details", dir="/", theme="dark", menu="none", dialog="none")
    press("shift-tab", focus="files")
    step("dark theme on screen", bar_pixel(screenshot("1-files-dark")) == DARK_BAR)

    # --- file list: type-ahead, open folder, arrows, parent ---
    press("e", name="etc")
    press("ret", dir="/etc", index="0", name="..")
    press("down", index="1")
    press("backspace", dir="/", name="etc")
    # --- menu bar by keyboard ---
    press("f10", menu="file")
    press("right", menu="view")
    press("esc", menu="none")
    press("alt-v", menu="view", item="0")
    step("view menu drawn", bar_pixel(screenshot("2-view-menu")) == DARK_BAR)
    press("down", item="1")
    press("ret", theme="light", menu="none")
    step("light theme on screen", bar_pixel(screenshot("3-light")) == LIGHT_BAR)
    press("alt-v", menu="view")
    press("ret", theme="dark", menu="none")
    step("back to dark theme on screen", bar_pixel(screenshot("4-dark-again")) == DARK_BAR)
    # --- panes and layout ---
    press("tab", focus="details")
    press("tab", focus="drives")
    press("tab", focus="files")
    press("ctrl-l", layout="stacked")
    step("stacked layout drawn", bar_pixel(screenshot("5-stacked")) == DARK_BAR)
    press("ctrl-l", layout="side")
    press("ctrl-d", details="false")
    press("ctrl-d", details="true")
    # --- hidden files and help ---
    press("ctrl-h", hidden="true")
    press("ctrl-h", hidden="false")
    press("f1", dialog="keys")
    step("help dialog drawn", bar_pixel(screenshot("6-help")) == DARK_BAR)
    press("esc", dialog="none")
    # --- power menu asks first (we do NOT confirm) ---
    press("f10", menu="file")
    press("up", item="5")
    press("ret", dialog="confirm_poweroff_")
    press("n", dialog="none")
except Exception as e:
    failure = str(e)
finally:
    try:
        if mon: mon.sendall(b"quit\n")
    except Exception: pass
    try: q.wait(timeout=10)
    except Exception: q.kill()

result["passed"] = failure is None
result["failure"] = failure
json.dump(result, open(os.path.join(outdir, f"result-{firmware}.json"), "w"), indent=1)
print(json.dumps({k: v for k, v in result.items() if k != "steps"}, indent=1))
if failure:
    print("---- serial log (last 80 lines) ----"); print("\n".join(log_text().splitlines()[-80:]))
    sys.exit(1)
