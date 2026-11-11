#!/usr/bin/env python3
"""End-to-end test of an Exec85 ISO in QEMU, driven like a user would drive it.

  Scenario 1 (live):    boot the ISO, choose "Try without installing", exercise the shell (menus, files, themes).
  Scenario 2 (install): boot the ISO with an empty virtual disk, walk through the whole installer by keyboard,
                        restart, boot FROM THE DISK (no ISO), log in with a wrong then the right password,
                        check that the shell runs as the new unprivileged user, then log out.

Every step reads the state lines the programs print on the serial port (EXEC85_STATE / EXEC85_INSTALLER /
EXEC85_LOGIN) and checks screenshot pixels. Screenshots go to the output folder.

  Scenario 3 (testing): only with --testing. Boot the TESTING ISO with NO disk, walk the wizard, choose "Skip installation",
                        log in, and open every app from the Apps menu, checking each one's EXEC85_APP state line.

usage: boot-test.py ISO bios|uefi OUTDIR [--accel kvm|tcg] [--testing]      exit code 0 = everything passed
"""
import glob, json, os, re, socket, struct, subprocess, sys, time, zlib

iso, firmware, outdir = sys.argv[1:4]
accel = sys.argv[sys.argv.index("--accel") + 1] if "--accel" in sys.argv else ("kvm" if os.access("/dev/kvm", os.W_OK) else "tcg")
os.makedirs(outdir, exist_ok=True)
SLOW = accel == "tcg"
MEM_MB = int(os.environ.get("BOOT_TEST_MEM", "1024"))   # the install image runs from RAM: it needs more than a 512 MB machine
STEP_TIMEOUT = 60 if SLOW else 20

STATE_KEYS = ("dir", "index", "name", "count", "drive", "focus", "menu", "item", "dialog", "theme", "sort", "hidden", "layout", "details", "user", "uid", "host")
INSTALLER_KEYS = ("step", "theme", "layout", "tz", "host", "user", "pw", "disk", "filter", "sel", "field", "error", "progress", "result")
LOGIN_KEYS = ("host", "user", "field", "pw_len", "failures", "msg", "layout", "power", "theme")
KEYSETS = {"EXEC85_STATE": STATE_KEYS, "EXEC85_INSTALLER": INSTALLER_KEYS, "EXEC85_LOGIN": LOGIN_KEYS}

# palette (see ui/src/gfx.rs)
DARK = {"bar": (0x1F, 0x3B, 0x4D), "panel": (0x10, 0x28, 0x38), "bg": (0x0B, 0x1D, 0x2A)}
LIGHT = {"bar": (0xC9, 0xBF, 0xA3), "panel": (0xFB, 0xF7, 0xEA), "bg": (0xF3, 0xEB, 0xD8)}

result = {"firmware": firmware, "accel": accel, "vm_mem_mb": MEM_MB, "steps": [], "passed": False}


def step(name, ok, detail=""):
    result["steps"].append({"step": name, "ok": bool(ok), "detail": detail if not ok else ""})
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""), flush=True)
    if not ok: raise AssertionError(f"{name}: {detail}")


# ---- PNG / PPM helpers -------------------------------------------------------------------------
def read_ppm(path):
    data = open(path, "rb").read()
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", data)
    return int(m[1]), int(m[2]), data[m.end():]

def write_png(path, w, h, rgb):
    raw = b"".join(b"\x00" + rgb[y*w*3:(y+1)*w*3] for y in range(h))
    def chunk(t, d): c = struct.pack(">I", len(d)) + t + d; return c + struct.pack(">I", zlib.crc32(t + d))
    open(path, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))

def px(shot, x, y):
    w, h, rgb = shot
    o = (y * w + x) * 3
    return tuple(rgb[o:o + 3])

def bar_pixel(shot):      # top bar, horizontally centred (text never reaches here)
    return px(shot, shot[0] // 2, 2)

def window_pixel(shot):   # blank cell inside the left window's bottom margin (shell only; other screens show the desktop colour)
    w, h, _ = shot
    scale = max(2, (h + 200) // 400)
    ch = 10 * scale
    rows = h // ch
    return px(shot, w // 4, (rows - 2) * ch + ch // 2)


class VM:
    def __init__(self, tag, cdrom=None, disk=None):
        self.tag = tag
        self.log = os.path.join(outdir, f"serial-{firmware}-{tag}.log")
        self.sock = os.path.join(outdir, f"mon-{firmware}-{tag}.sock")
        for p in (self.log, self.sock):
            if os.path.exists(p): os.remove(p)
        cmd = ["qemu-system-x86_64", "-machine", "q35", "-accel", accel, "-m", str(MEM_MB), "-smp", "2", "-vga", "std", "-display", "none",
               "-no-reboot", "-serial", f"file:{self.log}", "-monitor", f"unix:{self.sock},server,nowait"]
        if cdrom: cmd += ["-cdrom", cdrom, "-boot", "d"]
        else: cmd += ["-boot", "c"]
        if disk: cmd += ["-drive", f"file={disk},format=raw,if=ide,cache=unsafe"]
        if firmware == "uefi":
            code = next(iter(glob.glob("/usr/share/OVMF/OVMF_CODE_4M.fd") + glob.glob("/usr/share/OVMF/OVMF_CODE.fd") + glob.glob("/usr/share/ovmf/OVMF.fd") + glob.glob("/usr/share/qemu/edk2-x86_64-code.fd")), None)
            if not code: sys.exit("OVMF firmware not found")
            cmd += ["-drive", f"if=pflash,format=raw,readonly=on,file={code}"]
        self.t0 = time.time()
        self.p = subprocess.Popen(cmd)
        self.mon = None

    def text(self):
        try: return open(self.log, errors="replace").read()
        except FileNotFoundError: return ""

    def lines(self, prefix):
        keys = KEYSETS[prefix]
        out = []
        for line in self.text().splitlines():
            if prefix + " " in line:
                d = dict(kv.split("=", 1) for kv in line.split(prefix + " ", 1)[1].split() if "=" in kv)
                if all(k in d for k in keys): out.append(d)
        return out

    def wait_for(self, pattern, timeout):
        end = time.time() + timeout
        while time.time() < end:
            m = re.search(pattern, self.text())
            if m: return m
            if self.p.poll() is not None: break
            time.sleep(0.1)
        return None

    def wait_line(self, prefix, pred, timeout, after=0):
        end = time.time() + timeout
        while time.time() < end:
            for d in self.lines(prefix)[after:]:
                if pred(d): return d
            if self.p.poll() is not None: break
            time.sleep(0.1)
        return None

    def monitor(self, command):
        if self.mon is None:
            for _ in range(100):
                if os.path.exists(self.sock): break
                time.sleep(0.1)
            self.mon = socket.socket(socket.AF_UNIX); self.mon.settimeout(10); self.mon.connect(self.sock); self.mon.recv(4096)
        self.mon.sendall((command + "\n").encode()); time.sleep(0.12)
        try: self.mon.settimeout(0.2); self.mon.recv(65536)
        except Exception: pass
        self.mon.settimeout(10)

    def screenshot(self, name):
        ppm = os.path.join(outdir, f"{firmware}-{name}.ppm")
        if os.path.exists(ppm): os.remove(ppm)
        self.monitor(f"screendump {ppm}")
        last, end = -1, time.time() + 15
        while time.time() < end:
            size = os.path.getsize(ppm) if os.path.exists(ppm) else 0
            if size > 0 and size == last: break
            last = size; time.sleep(0.4)
        shot = read_ppm(ppm)
        write_png(os.path.join(outdir, f"{firmware}-{name}.png"), *shot)
        os.remove(ppm)
        return shot

    def stop(self):
        try:
            if self.mon: self.mon.sendall(b"quit\n")
        except Exception: pass
        try: self.p.wait(timeout=10)
        except Exception: self.p.kill()


def press(vm, key, prefix="EXEC85_STATE", **expect):
    """Press a key in the VM; wait for the program to report its new state; check it."""
    before = len(vm.lines(prefix))
    vm.monitor(f"sendkey {key}")
    d = vm.wait_line(prefix, lambda d: True, STEP_TIMEOUT, after=before)
    cur = (vm.lines(prefix)[before:] or [{}])[-1] if d else {}
    ok = d is not None and all(cur.get(k) == v for k, v in expect.items())
    label = f"key {key}" + "".join(f" -> {k}={v}" for k, v in expect.items())
    step(label, ok, f"got {({k: cur.get(k) for k in expect} if expect else 'no state line')}")
    return cur

def typed(vm, keys, prefix, **final):
    """Press several keys; only the last one is checked."""
    for i, k in enumerate(keys):
        if i == len(keys) - 1: return press(vm, k, prefix, **final)
        press(vm, k, prefix)


def scenario_live():
    vm = VM("live", cdrom=iso)
    try:
        m = vm.wait_for(r"EXEC85_READY uptime=([\d.]+)", 600 if SLOW else 120)
        step("live: first screen drawn", m is not None, "no EXEC85_READY on serial")
        result.update(booted=True, kernel_uptime_at_ready_s=float(m[1]), host_seconds_to_ready=round(time.time() - vm.t0, 2))
        d = vm.wait_for(r"EXEC85_DISPLAY (\w+) (\d+)x(\d+)", 10)
        if d: result.update(display_driver=d[1], screen=f"{d[2]}x{d[3]}")
        step("live: starts in the installer", vm.wait_for(r"EXEC85_SCREEN installer", 10) is not None)
        mm = vm.wait_for(r"EXEC85_MEM total_kb=(\d+) used_kb=(\d+) exec85_rss_kb=(\d+)", 120)
        step("live: idle memory report", mm is not None)
        result.update(mem_total_kb=int(mm[1]), mem_used_kb=int(mm[2]), exec85_rss_kb=int(mm[3]))
        step("live: installer welcome screen (dark)", bar_pixel(vm.screenshot("1-welcome")) == DARK["bar"])

        # Welcome -> "Try Exec85 without installing" -> the shell, as root
        n = len(vm.lines("EXEC85_INSTALLER"))
        press(vm, "down", "EXEC85_INSTALLER", step="welcome", sel="Try_Exec85_without_installing")
        vm.monitor("sendkey ret")
        d = vm.wait_line("EXEC85_STATE", lambda d: True, STEP_TIMEOUT)
        step("live: shell starts as root", d is not None and d["user"] == "root" and d["uid"] == "0" and d["dir"] == "/", str(d))
        # baseline with two real key presses
        press(vm, "tab", focus="details", dir="/", theme="dark", menu="none", dialog="none")
        press(vm, "shift-tab", focus="files")
        step("live: dark theme on screen", bar_pixel(vm.screenshot("2-shell-dark")) == DARK["bar"] and window_pixel(vm.screenshot("2b-shell")) == DARK["panel"])

        press(vm, "e", name="etc")
        press(vm, "ret", dir="/etc", index="0", name="..")
        press(vm, "down", index="1")
        press(vm, "backspace", dir="/", name="etc")
        press(vm, "f10", menu="file")
        press(vm, "right", menu="view")
        press(vm, "esc", menu="none")
        press(vm, "alt-v", menu="view", item="0")
        step("live: view menu drawn", bar_pixel(vm.screenshot("3-view-menu")) == DARK["bar"])
        press(vm, "down", item="1")
        press(vm, "ret", theme="light", menu="none")
        step("live: light theme on screen", bar_pixel(vm.screenshot("4-shell-light")) == LIGHT["bar"])
        press(vm, "alt-v", menu="view")
        press(vm, "ret", theme="dark", menu="none")
        step("live: back to dark", bar_pixel(vm.screenshot("5-dark-again")) == DARK["bar"])
        press(vm, "tab", focus="details")
        press(vm, "tab", focus="drives")
        press(vm, "tab", focus="files")
        press(vm, "ctrl-l", layout="stacked")
        press(vm, "ctrl-l", layout="side")
        press(vm, "ctrl-d", details="false")
        press(vm, "ctrl-d", details="true")
        press(vm, "ctrl-h", hidden="true")
        press(vm, "ctrl-h", hidden="false")
        press(vm, "f1", dialog="keys")
        step("live: help dialog drawn", bar_pixel(vm.screenshot("6-help")) == DARK["bar"])
        press(vm, "esc", dialog="none")
        press(vm, "f10", menu="file")
        press(vm, "up", item="15")
        press(vm, "ret", dialog="confirm_poweroff_")
        press(vm, "n", dialog="none")
        # Log out goes back to the installer
        press(vm, "f10", menu="file")
        for _ in range(3): press(vm, "up")
        n = len(vm.lines("EXEC85_INSTALLER"))
        vm.monitor("sendkey ret")
        d = vm.wait_line("EXEC85_INSTALLER", lambda d: d["step"] == "welcome", STEP_TIMEOUT, after=n)
        step("live: Log out returns to the installer", d is not None)
    finally:
        vm.stop()


PW_KEYS = ["s", "e", "c", "r", "e", "t", "7", "slash"]   # on the Hungarian layout, the key at the US "/" position types "-"  =>  "secret7-"

def scenario_install():
    disk = os.path.join(outdir, f"disk-{firmware}.img")
    with open(disk, "wb") as f: f.truncate(8 << 30)
    I = "EXEC85_INSTALLER"
    vm = VM("install", cdrom=iso, disk=disk)
    t_install = None
    try:
        step("install: installer is up", vm.wait_for(r"EXEC85_SCREEN installer", 600 if SLOW else 120) is not None)
        press(vm, "ret", I, step="theme")
        press(vm, "down", I, theme="light")
        step("install: theme changes live", bar_pixel(vm.screenshot("i1-theme-light")) == LIGHT["bar"])
        press(vm, "ret", I, step="keyboard")
        typed(vm, ["h", "u", "n", "g"], I, sel="Hungarian_(hu)", layout="hu", filter="hung")
        press(vm, "ret", I, step="timezone")
        typed(vm, ["b", "u", "d", "a", "p", "e", "s", "t"], I, sel="Europe/Budapest")
        press(vm, "ret", I, step="hostname", tz="Europe/Budapest")
        for _ in range(5): press(vm, "backspace", I)
        press(vm, "backspace", I, host="-")
        typed(vm, ["l", "a", "b", "8", "5"], I, host="lab85")
        press(vm, "ret", I, step="user")
        typed(vm, ["t", "e", "s", "t", "e", "r"], I, user="tester")
        press(vm, "ret", I, step="password")
        typed(vm, PW_KEYS, I, pw="set")
        press(vm, "tab", I, field="1")
        typed(vm, PW_KEYS, I)
        press(vm, "ret", I, step="disk")
        step("install: disk list drawn", bar_pixel(vm.screenshot("i2-disk")) == LIGHT["bar"])
        press(vm, "ret", I, step="confirm", disk="sda")
        typed(vm, ["s", "d", "a"], I)
        step("install: confirm screen drawn", bar_pixel(vm.screenshot("i3-confirm")) == LIGHT["bar"])
        t_install = time.time()
        press(vm, "ret", I, step="installing")
        r = vm.wait_for(r"EXEC85_INSTALL result=(ok|failed)(\S*)", 600 if SLOW else 300)
        step("install: installation finished", r is not None and r[1] == "ok", (r[0] if r else "no result line") + " | " + "\n".join(l for l in vm.text().splitlines() if "EXEC85_INSTALL" in l)[-600:])
        result["install_seconds"] = round(time.time() - t_install, 1)
        d = vm.wait_line(I, lambda d: d["step"] == "done" and d["result"] == "ok", STEP_TIMEOUT)
        step("install: 'complete' screen shown", d is not None)
        step("install: complete screen drawn", bar_pixel(vm.screenshot("i4-done")) == LIGHT["bar"])
        vm.monitor("sendkey ret")      # restart
        for _ in range(600):
            if vm.p.poll() is not None: break
            time.sleep(0.2)
        step("install: the computer restarts", vm.p.poll() is not None, "QEMU did not exit after Enter")
    finally:
        vm.stop()

    # ---- second boot: from the installed disk, no ISO ----
    L = "EXEC85_LOGIN"
    vm = VM("installed", disk=disk)
    try:
        step("installed: boots from the disk to the login screen", vm.wait_for(r"EXEC85_SCREEN login", 600 if SLOW else 120) is not None,
             "\n".join(vm.text().splitlines()[-25:]))
        m = vm.wait_for(r"EXEC85_READY uptime=([\d.]+)", 5)
        result.update(installed_kernel_uptime_at_login_s=float(m[1]) if m else None, installed_host_seconds_to_login=round(time.time() - vm.t0, 2))
        d = vm.wait_line(L, lambda d: True, 10)
        step("installed: login shows the chosen settings", d is not None and d["host"] == "lab85" and d["layout"] == "hu" and d["theme"] == "light" and d["user"] == "tester" and d["field"] == "password", str(d))
        shot = vm.screenshot("l1-login")
        step("installed: login screen in the chosen (light) theme", bar_pixel(shot) == LIGHT["bar"] and window_pixel(shot) == LIGHT["bg"])
        mm = vm.wait_for(r"EXEC85_MEM total_kb=(\d+) used_kb=(\d+) exec85_rss_kb=(\d+)", 120)
        step("installed: idle memory report", mm is not None)
        result.update(installed_mem_total_kb=int(mm[1]), installed_mem_used_kb=int(mm[2]), installed_exec85_rss_kb=int(mm[3]))

        # wrong password first
        typed(vm, ["w", "r", "o", "n", "g"], L, pw_len="5")
        press(vm, "ret", L, failures="1", msg="Login_incorrect", pw_len="0")
        step("installed: wrong password screen", bar_pixel(vm.screenshot("l2-wrong")) == LIGHT["bar"])
        time.sleep(1.6)   # the screen deliberately ignores keys for a second after a failure
        typed(vm, PW_KEYS, L, pw_len="8")
        n = len(vm.lines("EXEC85_STATE"))
        vm.monitor("sendkey ret")
        d = vm.wait_line("EXEC85_STATE", lambda d: True, STEP_TIMEOUT)
        step("installed: right password starts the shell as the new user", d is not None and d["user"] == "tester" and d["uid"] == "1000" and d["dir"] == "/home/tester" and d["host"] == "lab85" and d["theme"] == "light", str(d))
        shot = vm.screenshot("s1-shell")
        step("installed: shell is drawn (as an unprivileged user)", bar_pixel(shot) == LIGHT["bar"] and window_pixel(shot) == LIGHT["panel"])
        press(vm, "ctrl-h", hidden="true", user="tester", uid="1000")     # keyboard works for the non-root user
        press(vm, "ctrl-h", hidden="false")
        # Log out -> login again
        press(vm, "f10", menu="file")
        for _ in range(3): press(vm, "up")
        n = len(vm.lines(L))
        vm.monitor("sendkey ret")
        d = vm.wait_line(L, lambda d: True, STEP_TIMEOUT, after=n)
        step("installed: Log out returns to the login screen", d is not None and d["failures"] == "0" and d["pw_len"] == "0", str(d))
        # Log in again, define a service, then restart the computer
        typed(vm, PW_KEYS, L, pw_len="8")
        vm.monitor("sendkey ret")
        vm.wait_line("EXEC85_STATE", lambda d: d.get("user") == "tester", STEP_TIMEOUT, after=len(vm.lines("EXEC85_STATE")))
        time.sleep(1)
        open_app(vm, 1)
        terminal_run(vm, "exec85 --svc add ticker -- sleep 100000; exec85 --svc start ticker; echo TK=$?", "TK=0", 90, "installed: a service is defined and started")
        step("installed: the service is running", svc_line(vm, "ticker", lambda d: d["status"] == "running" and d["pid"] != "-", 60) is not None)
        press(vm, "f4")
        press(vm, "f10", menu="file")
        press(vm, "up"); press(vm, "up", item="14")
        press(vm, "ret")
        vm.monitor("sendkey y")
        for _ in range(300):
            if vm.p.poll() is not None: break
            time.sleep(0.2)
        step("installed: the computer restarts from the Restart menu item", vm.p.poll() is not None)
    finally:
        vm.stop()

    # ---- third boot: nobody logs in, the service must be up anyway ----
    vm = VM("rebooted", disk=disk)
    try:
        step("rebooted: login screen is shown", vm.wait_for(r"EXEC85_SCREEN login", 600 if SLOW else 120) is not None)
        d = svc_line(vm, "ticker", lambda d: d["status"] == "running" and d["pid"] != "-", 90)
        step("rebooted: the service is running with nobody logged in", d is not None, "\n".join(l for l in vm.text().splitlines() if "EXEC85_SVC" in l or "EXEC85_HELPER" in l))
        step("rebooted: still at the login screen", vm.lines("EXEC85_STATE") == [])
    finally:
        vm.stop()


APPS = ["notepad", "terminal", "paint", "monitor", "diskusage", "network", "clock", "calculator", "reversi", "snake", "services", "software"]

SHIFTED = {"_": "shift-minus", ":": "shift-semicolon", '"': "shift-apostrophe", "$": "shift-4", "?": "shift-slash", "|": "shift-backslash", ">": "shift-dot", "&": "shift-7", "*": "shift-8", "(": "shift-9", ")": "shift-0", "{": "shift-bracket_left", "}": "shift-bracket_right"}
PLAIN = {" ": "spc", "-": "minus", ".": "dot", "/": "slash", "=": "equal", "'": "apostrophe", ";": "semicolon", ",": "comma", "\\": "backslash"}

def type_text(vm, text):
    """Type text into the focused window with real key presses."""
    for ch in text:
        if ch in PLAIN: k = PLAIN[ch]
        elif ch in SHIFTED: k = SHIFTED[ch]
        elif ch.isupper(): k = "shift-" + ch.lower()
        else: k = ch
        vm.monitor(f"sendkey {k}")

def terminal_run(vm, command, expect, timeout=120, label=None):
    """Type a command into the Terminal app, press Enter, and wait until the screen's last lines contain `expect`."""
    type_text(vm, command)
    vm.monitor("sendkey ret")
    end = time.time() + timeout
    d = {}
    while time.time() < end:
        time.sleep(0.7)
        d = app_line(vm, "terminal", 1) or {}
        if expect in (d.get("l1", "") + d.get("l2", "") + d.get("l3", "")): break
    else:
        step(label or f"terminal: {command[:50]}", False, str(d))
    step(label or f"terminal: {command[:50]}", True)

def svc_line(vm, name, pred, timeout=60, after=0):
    """Wait for a service status line printed by the system helper on the console."""
    end = time.time() + timeout
    while time.time() < end:
        found = []
        for line in vm.text().splitlines():
            if f"EXEC85_SVC name={name} " in line:
                found.append(dict(kv.split("=", 1) for kv in line.split("EXEC85_SVC ", 1)[1].split() if "=" in kv))
        for d in found[after:]:
            if pred(d): return d
        time.sleep(0.5)
    return None

def svc_count(vm, name): return sum(1 for l in vm.text().splitlines() if f"EXEC85_SVC name={name} " in l)

def app_line(vm, name, timeout=STEP_TIMEOUT):
    """Wait until the newest EXEC85_APP line is for `name`; return its fields."""
    end = time.time() + timeout
    while time.time() < end:
        found = None
        for line in vm.text().splitlines():
            if "EXEC85_APP " in line:
                d = dict(kv.split("=", 1) for kv in line.split("EXEC85_APP ", 1)[1].split() if "=" in kv)
                found = d
        if found and found.get("name") == name: return found
        time.sleep(0.1)
    return None

def app_press(vm, key, name, **expect):
    """Press a key while an app has the focus and check its next state."""
    n = len(vm.lines("EXEC85_STATE"))
    vm.monitor(f"sendkey {key}")
    vm.wait_line("EXEC85_STATE", lambda d: True, STEP_TIMEOUT, after=n)
    time.sleep(0.3)
    d = app_line(vm, name, 5) or {}
    step(f"app {name}: key {key}" + "".join(f" -> {k}={v}" for k, v in expect.items()), all(d.get(k) == v for k, v in expect.items()), str({k: d.get(k) for k in expect}))
    return d

def open_app(vm, index):
    press(vm, "alt-a", menu="apps")
    for _ in range(index): press(vm, "down")
    press(vm, "ret", menu="none")
    d = app_line(vm, APPS[index])
    step(f"app {APPS[index]} opens", d is not None, "no EXEC85_APP line")
    return d

def scenario_testing():
    I = "EXEC85_INSTALLER"
    vm = VM("testing", cdrom=iso)          # no disk at all
    try:
        step("testing: installer is up", vm.wait_for(r"EXEC85_SCREEN installer", 600 if SLOW else 120) is not None)
        press(vm, "ret", I, step="theme")
        press(vm, "ret", I, step="keyboard")
        press(vm, "ret", I, step="timezone")
        press(vm, "ret", I, step="hostname")
        press(vm, "ret", I, step="user")
        typed(vm, ["t", "e", "s", "t", "e", "r"], I, user="tester")
        press(vm, "ret", I, step="password")
        typed(vm, ["s", "e", "c", "r", "e", "t", "7", "minus"], I, pw="set")
        press(vm, "tab", I, field="1")
        typed(vm, ["s", "e", "c", "r", "e", "t", "7", "minus"], I)
        press(vm, "ret", I, step="disk")
        d = vm.lines(I)[-1]
        step("testing: the Skip choice is offered and selected", d["sel"].startswith("Skip_installation"), d["sel"])
        press(vm, "ret", I, step="confirm")
        vm.monitor("sendkey ret")   # the in-memory set-up is so fast that the 'installing' state can go by unseen
        r = vm.wait_for(r"EXEC85_INSTALL result=(ok|failed)", 60)
        step("testing: set-up in memory finished", r is not None and r[1] == "ok")
        d = vm.wait_line(I, lambda d: d["step"] == "done" and d["result"] == "ok", STEP_TIMEOUT)
        step("testing: complete screen shown", d is not None)
        vm.monitor("sendkey ret")
        step("testing: login screen follows", vm.wait_for(r"EXEC85_SCREEN login", 60) is not None)
        d = vm.wait_line("EXEC85_LOGIN", lambda d: d["user"] == "tester", 10)
        step("testing: login knows the new user", d is not None, str(vm.lines("EXEC85_LOGIN")[-1:]))
        typed(vm, ["s", "e", "c", "r", "e", "t", "7", "minus"], "EXEC85_LOGIN", pw_len="8")
        vm.monitor("sendkey ret")
        d = vm.wait_line("EXEC85_STATE", lambda d: True, STEP_TIMEOUT)
        step("testing: shell starts as the new user", d is not None and d["user"] == "tester" and d["uid"] == "1000", str(d))

        open_app(vm, 7)   # calculator
        for k in ["6", "shift-8", "7"]: app_press(vm, k, "calculator")
        app_press(vm, "ret", "calculator", result="42")
        press(vm, "f4")
        open_app(vm, 1)   # terminal
        for k in "echo":
            vm.monitor(f"sendkey {k}")
        vm.monitor("sendkey spc")
        for k in ["h", "i", "1", "2"]: vm.monitor(f"sendkey {k}")
        vm.monitor("sendkey ret")
        end = time.time() + STEP_TIMEOUT
        ok = False
        while time.time() < end and not ok:
            time.sleep(0.5)
            d = app_line(vm, "terminal", 1) or {}
            ok = "hi12" in (d.get("l1", "") + d.get("l2", "") + d.get("l3", ""))
        step("terminal runs a real shell command", ok, str(d))
        press(vm, "f4")
        for i in (3, 4, 5, 6, 8, 9, 2, 0):
            open_app(vm, i)
            press(vm, "f4")
        vm.screenshot("t1-shell")

        # ---- services and software, with nothing specific built in ----
        open_app(vm, 1)    # terminal
        step("services: the helper is running", vm.wait_for(r"EXEC85_HELPER ready", 30) is not None)
        terminal_run(vm, "exec85 --pkg update; echo UPD=$?", "UPD=0", 180, "software: package list refreshed over the network")
        terminal_run(vm, "exec85 --pkg add python3 nginx; echo PKG=$?", "PKG=0", 600, "software: python3 and nginx installed with apk")
        terminal_run(vm, "exec85 --svc add web --dir /tmp -- python3 -m http.server 8080; echo ADD=$?", "ADD=0", 60, "services: a web service is defined")
        terminal_run(vm, "exec85 --svc start web; echo START=$?", "START=0", 60, "services: it starts")
        d = svc_line(vm, "web", lambda d: d["status"] == "running" and d["pid"] != "-", 60)
        step("services: the web service is running", d is not None)
        pid1 = d["pid"]
        terminal_run(vm, "wget -q -O /dev/null http://127.0.0.1:8080/; echo HTTP=$?", "HTTP=0", 60, "services: the website answers")
        n = svc_count(vm, "web")
        terminal_run(vm, "pkill -f 8080; echo KIL$((1+1))", "KIL2", 30, "services: kill the process")
        d = svc_line(vm, "web", lambda d: d["status"] == "running" and d["pid"] not in ("-", pid1), 60, after=n)
        step("services: it comes back by itself with a new process", d is not None, f"old pid {pid1}")
        terminal_run(vm, "wget -q -O /dev/null http://127.0.0.1:8080/; echo HTTP2=$?", "HTTP2=0", 60, "services: the website answers again")
        terminal_run(vm, "exec85 --svc add nginx --user root -- nginx -g 'daemon off;'; exec85 --svc start nginx; echo NG=$?", "NG=0", 60, "services: nginx defined and started")
        d = svc_line(vm, "nginx", lambda d: d["status"] == "running", 60)
        step("services: nginx is running", d is not None)
        terminal_run(vm, "wget -S -O /dev/null http://127.0.0.1/ 2>&1 | head -1", "HTTP/1.1", 60, "services: nginx answers")
        terminal_run(vm, "exec85 --svc stop web; exec85 --svc remove web; echo GONE=$?", "GONE=0", 60, "services: a service is stopped and removed")
        d = svc_line(vm, "web", lambda d: d["status"] != "running", 30, after=0)
        press(vm, "f4")
        # the Services and Software apps can be opened and show the helper's data
        open_app(vm, 10)
        end = time.time() + 30; d = {}
        while time.time() < end:
            d = app_line(vm, "services", 1) or {}
            if d.get("count") == "1": break
            time.sleep(0.5)
        step("services app lists the service", d.get("count") == "1", str(d))
        press(vm, "f4")
        open_app(vm, 11)
        end = time.time() + 60; d = {}
        while time.time() < end:
            d = app_line(vm, "software", 1) or {}
            if d.get("view") == "installed" and int(d.get("installed", "0")) > 5: break
            time.sleep(0.5)
        step("software app lists installed programs", int(d.get("installed", "0")) > 5, str(d))
        press(vm, "f4")
        open_app(vm, 1)
        terminal_run(vm, "exec85 --pkg add openjdk21-jre-headless; echo JPKG=$?", "JPKG=0", 900, "software: Java installed with apk")
        terminal_run(vm, "java -version 2>&1 | head -1; echo JAVA=$?", "JAVA=0", 120, "software: Java runs")
    finally:
        vm.stop()

failure = None
try:
    if "--testing" in sys.argv:
        scenario_testing()
    else:
        scenario_live()
        scenario_install()
except Exception as e:
    failure = str(e)
result["passed"] = failure is None
result["failure"] = failure
json.dump(result, open(os.path.join(outdir, f"result-{firmware}.json"), "w"), indent=1)
print(json.dumps({k: v for k, v in result.items() if k != "steps"}, indent=1))
if failure:
    for name in sorted(glob.glob(os.path.join(outdir, f"serial-{firmware}-*.log"))):
        lines = open(name, errors="replace").read().splitlines()
        print(f"---- {os.path.basename(name)}: first 25 lines ----"); print("\n".join(lines[:25]))
        key = re.compile(r"initramfs|initrd|Unpacking|Memory:|Run /|command line|EXEC85|Kernel panic|No space|out of memory|Out of memory|error|failed", re.I)
        print(f"---- {os.path.basename(name)}: interesting lines ----"); print("\n".join(l for l in lines if key.search(l))[:6000])
        print(f"---- {os.path.basename(name)}: last 40 lines ----"); print("\n".join(lines[-40:]))
    sys.exit(1)
