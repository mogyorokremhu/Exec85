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
    def __init__(self, tag, cdrom=None, disk=None, mem=None, extra=()):
        self.tag = tag
        self.log = os.path.join(outdir, f"serial-{firmware}-{tag}.log")
        self.sock = os.path.join(outdir, f"mon-{firmware}-{tag}.sock")
        for p in (self.log, self.sock):
            if os.path.exists(p): os.remove(p)
        cmd = ["qemu-system-x86_64", "-machine", "q35", "-accel", accel, "-m", str(mem or MEM_MB), "-smp", "2", "-vga", "std", "-display", "none",
               "-no-reboot", "-serial", f"file:{self.log}", "-monitor", f"unix:{self.sock},server,nowait"]
        cmd += list(extra)
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
        reply = b""
        try: self.mon.settimeout(0.2); reply = self.mon.recv(65536)
        except Exception: pass
        self.mon.settimeout(10)
        return reply.decode(errors="replace")

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
    vm = VM("live", cdrom=iso, extra=["-audiodev", "none,id=snd0", "-device", "intel-hda", "-device", "hda-duplex,audiodev=snd0"])
    try:
        m = vm.wait_for(r"EXEC85_READY uptime=([\d.]+)", 600 if SLOW else 120)
        step("live: first screen drawn", m is not None, "no EXEC85_READY on serial")
        result.update(booted=True, kernel_uptime_at_ready_s=float(m[1]), host_seconds_to_ready=round(time.time() - vm.t0, 2))
        d = vm.wait_for(r"EXEC85_DISPLAY (\w+) (\d+)x(\d+)", 10)
        if d: result.update(display_driver=d[1], screen=f"{d[2]}x{d[3]}")
        step("live: starts in the installer", vm.wait_for(r"EXEC85_SCREEN installer", 10) is not None)
        t = vm.text()
        # On this fast machine the first real screen is ready as soon as the display exists, so the boot screen may be stopped before it
        # has drawn a frame; what must hold is that it starts first and never gets in the way of the real screen.
        step("live: the boot screen starts first and gets out of the way", "EXEC85_SPLASH starting" in t and t.index("EXEC85_SPLASH starting") < t.index("EXEC85_SCREEN installer") and "EXEC85_ERROR" not in t, "no EXEC85_SPLASH starting line before the installer (or a display error)")
        step("live: the kernel is quiet on the console", "Linux version" not in t and "Command line:" not in t, "kernel start-up messages are still printed")
        mm = vm.wait_for(r"EXEC85_MEM total_kb=(\d+) used_kb=(\d+) exec85_rss_kb=(\d+)", 120)
        step("live: idle memory report", mm is not None)
        result.update(mem_total_kb=int(mm[1]), mem_used_kb=int(mm[2]), exec85_rss_kb=int(mm[3]))
        step("live: installer welcome screen (dark)", bar_pixel(vm.screenshot("1-welcome")) == DARK["bar"])
        d = vm.wait_line("EXEC85_INSTALLER", lambda d: d.get("music") in ("on", "off"), 30)
        step("live: the installer plays its music when there is a sound card", d is not None and d.get("music") == "on", str(vm.lines("EXEC85_INSTALLER")[-1:]))
        press(vm, "f8", "EXEC85_INSTALLER", music="off")
        press(vm, "f8", "EXEC85_INSTALLER", music="on")

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
        press(vm, "up", item="16")
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


PW_KEYS = ["s", "e", "c", "r", "e", "t", "7", "minus"]   # => "secret7-" (the installed keyboard is the UK one, which the terminal tests below also type on)

def scenario_install():
    disk = os.path.join(outdir, f"disk-{firmware}.img")
    with open(disk, "wb") as f: f.truncate(8 << 30)
    I = "EXEC85_INSTALLER"
    vm = VM("install", cdrom=iso, disk=disk)
    KEYBOARD["layout"] = "uk"   # this scenario installs with the UK keyboard
    t_install = None
    try:
        step("install: installer is up", vm.wait_for(r"EXEC85_SCREEN installer", 600 if SLOW else 120) is not None)
        press(vm, "ret", I, step="theme")
        press(vm, "down", I, theme="light")
        step("install: theme changes live", bar_pixel(vm.screenshot("i1-theme-light")) == LIGHT["bar"])
        press(vm, "ret", I, step="keyboard")
        typed(vm, ["u", "k"], I, sel="English_(UK)_(uk)", layout="uk", filter="uk")
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
        step("installed: login shows the chosen settings", d is not None and d["host"] == "lab85" and d["layout"] == "uk" and d["theme"] == "light" and d["user"] == "tester" and d["field"] == "password", str(d))
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
        terminal_run(vm, "exec85 --svc add ticker -- sleep 100000; echo TA=$?", "TA=0", 90, "installed: a service is defined")
        terminal_run(vm, "exec85 --svc start ticker; echo TK=$?", "TK=0", 90, "installed: the service starts")
        d = svc_line(vm, "ticker", lambda d: d["status"] == "running" and d["pid"] != "-", 150)
        if d is None:   # diagnostics for the log: what is really running?
            for i, cmd in enumerate(["ps | grep supervise | grep -v grep | cut -c1-66; echo DIAG$((0+1))", "exec85 --svc log ticker | tail -1 | cut -c1-66; echo DIAG$((0+2))", "ps | grep -c sleep; echo DIAG$((0+3))"], 1):
                try: terminal_run(vm, cmd, f"DIAG{i}", 20, "diagnostic")
                except AssertionError: pass
                print("DIAG>", app_line(vm, "terminal", 1), flush=True)
            print("DIAG svc lines>", [l for l in vm.text().splitlines() if "EXEC85_SVC" in l][-6:], flush=True)
        step("installed: the service is running", d is not None)
        press(vm, "f4")
        press(vm, "f10", menu="file")
        press(vm, "up"); press(vm, "up", item="15")
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
        d = svc_line(vm, "ticker", lambda d: d["status"] == "running", 90)
        step("rebooted: the service is running with nobody logged in", d is not None, "\n".join(l for l in vm.text().splitlines() if "EXEC85_SVC" in l or "EXEC85_HELPER" in l))
        step("rebooted: still at the login screen", vm.lines("EXEC85_STATE") == [])
    finally:
        vm.stop()


APPS = ["notepad", "terminal", "paint", "monitor", "diskusage", "network", "clock", "calculator", "reversi", "snake", "services", "software", "minecraft", "discordbot", "remote"]

SHIFTED = {"+": "shift-equal", "_": "shift-minus", ":": "shift-semicolon", '"': "shift-apostrophe", "$": "shift-4", "?": "shift-slash", "|": "shift-backslash", ">": "shift-dot", "&": "shift-7", "*": "shift-8", "(": "shift-9", ")": "shift-0", "{": "shift-bracket_left", "}": "shift-bracket_right"}
PLAIN = {"[": "bracket_left", "]": "bracket_right", " ": "spc", "-": "minus", ".": "dot", "/": "slash", "=": "equal", "'": "apostrophe", ";": "semicolon", ",": "comma", "\\": "backslash"}

KEYBOARD = {"layout": "us"}   # the layout the system under test is using
UK_SHIFTED = {"|": "shift-less", '"': "shift-2"}   # the UK keyboard puts these elsewhere than the US one

def http_probe(port, timeout=5):
    """From outside the VM: does an HTTP server answer on this forwarded port?"""
    try:
        c = socket.create_connection(("127.0.0.1", port), timeout=timeout)
        c.sendall(b"GET / HTTP/1.0\r\n\r\n"); c.settimeout(timeout)
        data = c.recv(100); c.close()
        return data.startswith(b"HTTP/")
    except Exception:
        return False

def wait_probe(port, want, seconds=20):
    end = time.time() + seconds
    while time.time() < end:
        if http_probe(port, 3) == want: return True
        time.sleep(1)
    return False

def type_text(vm, text):
    """Type text into the focused window with real key presses."""
    for ch in text:
        if KEYBOARD["layout"] == "uk" and ch in UK_SHIFTED: k = UK_SHIFTED[ch]
        elif ch in PLAIN: k = PLAIN[ch]
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
        shown = d.get("l1", "") + d.get("l2", "") + d.get("l3", "")
        if (re.search(expect[3:], shown) if expect.startswith("re:") else expect in shown): break
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

def dump_file(vm, path, lines=4, start=1):
    """Print lines of a file in the VM (diagnostics for a failed step)."""
    for i in range(start, lines + 1):
        try: terminal_run(vm, f"sed -n {i}p {path} | cut -c1-66; echo FD{i}", f"FD{i}", 20, "diagnostic")
        except AssertionError: pass
        print("FILE>", path, i, (app_line(vm, "terminal", 1) or {}).get("l1"), flush=True)

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
    usb = os.path.join(outdir, "usb.img")
    with open(os.path.join(outdir, "hello.txt"), "w") as f: f.write("hello from the pendrive\n")
    with open(usb, "wb") as f: f.truncate(64 << 20)
    subprocess.run(["mkfs.vfat", "-F", "32", "-n", "TESTUSB", usb], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["mcopy", "-i", usb, os.path.join(outdir, "hello.txt"), "::HELLO.TXT"], check=True)
    # the USB controller and the stick's disk exist from the start; the stick itself is plugged in later with device_add
    vm = VM("testing", cdrom=iso, mem=3072, extra=["-device", "qemu-xhci,id=xhci", "-drive", f"if=none,id=usbd,file={usb},format=raw",
                                                      "-netdev", "user,id=n0,hostfwd=tcp::2222-:22,hostfwd=tcp::8081-:8080,hostfwd=tcp::8090-:8090", "-device", "e1000e,netdev=n0"])   # no disk at all; more RAM, because the live system keeps its files in RAM (about 40% of it) and Java is big
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

        # ---- pendrives: plug in, browse, write, eject, pull out ----
        vm.monitor("device_add usb-storage,drive=usbd,id=usbdev,bus=xhci.0")
        m = vm.wait_for(r"EXEC85_USB added dev=(\S+) fs=vfat label=TESTUSB mount=(\S+)", 90)
        step("usb: the plugged-in pendrive is detected and mounted", m is not None, "\n".join(l for l in vm.text().splitlines() if "EXEC85_USB" in l))
        mount = m[2]
        d = vm.wait_line("EXEC85_STATE", lambda d: "TESTUSB" in d.get("drives", ""), 30)
        step("usb: the Explorer shows it in the drive row", d is not None)
        open_app(vm, 1)
        terminal_run(vm, "cat /media/TESTUSB/HELLO.TXT", "hello_from", 30, "usb: a file on the pendrive can be read")
        terminal_run(vm, "echo wr$((5-3))ten > /media/TESTUSB/new.txt; cat /media/TESTUSB/new.txt", "wr2ten", 30, "usb: a file can be written to the pendrive")
        terminal_run(vm, "cp /media/TESTUSB/HELLO.TXT /tmp/copy.txt; ls -l /tmp/copy.txt | cut -c1-30; echo CP$((5-3))", "CP2", 30, "usb: a file can be copied off the pendrive")
        terminal_run(vm, "rm /media/TESTUSB/new.txt; ls /media/TESTUSB; echo LS$((5-3))", "LS2", 30, "usb: a file on the pendrive can be deleted")
        press(vm, "f4")
        # eject from the Explorer: drive row, last drive, E
        press(vm, "tab", focus="details"); press(vm, "tab", focus="drives"); press(vm, "end")
        n = len(vm.lines("EXEC85_STATE"))
        vm.monitor("sendkey e")
        d = vm.wait_line("EXEC85_STATE", lambda d: "TESTUSB" not in d.get("drives", ""), 60, after=n)
        step("usb: Eject unmounts the drive and removes it from the drive row", d is not None, str(vm.lines("EXEC85_STATE")[-1:]))
        open_app(vm, 1)
        terminal_run(vm, "ls /media; echo MD$((5-3))", "MD2", 30, "usb: the drive's folder is gone after eject")
        press(vm, "f4")
        vm.monitor("device_del usbdev")
        step("usb: pulling the stick out afterwards is noticed", vm.wait_for(r"EXEC85_USB removed mount=", 60) is not None)
        time.sleep(3)
        vm.monitor(f"drive_add 0 if=none,id=usbd,file={usb},format=raw")   # unplugging the stick also released its virtual disk
        reply = vm.monitor("device_add usb-storage,drive=usbd,id=usbdev2,bus=xhci.0")
        print("device_add reply:", repr(re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", reply)[-300:]), flush=True)
        if vm.wait_for(r"EXEC85_USB added dev=\S+ fs=vfat label=TESTUSB mount=\S+(?s:.*)EXEC85_USB added dev=\S+ fs=vfat label=TESTUSB", 40) is None:
            open_app(vm, 1)
            for i, cmd in enumerate(["tail -2 /proc/partitions | tr -s ' ' '_'; echo DX1", "ls /sys/block | tr '\\n' ' '; echo DX2", "ls /dev | grep sd | tr '\\n' ' '; echo DX3", "grep media /proc/mounts | cut -c1-60; echo DX4"], 1):
                try: terminal_run(vm, cmd, f"DX{i}", 20, "diagnostic")
                except AssertionError: pass
                print("DIAG>", i, app_line(vm, "terminal", 1), flush=True)
        step("usb: plugging it in again mounts it again", vm.wait_for(r"EXEC85_USB added dev=\S+ fs=vfat label=TESTUSB mount=\S+(?s:.*)EXEC85_USB added dev=\S+ fs=vfat label=TESTUSB", 90) is not None, "\n".join(l for l in vm.text().splitlines() if "EXEC85_USB" in l) + " | " + repr(reply))

        # ---- our own terminal commands ----
        open_app(vm, 1)
        terminal_run(vm, "help | head -1", "Exec85_commands", 30, "commands: help")
        terminal_run(vm, "dir /etc | tail -1", "folders,", 30, "commands: dir")
        terminal_run(vm, "disk | head -2 | tail -1", "free_of", 30, "commands: disk")
        terminal_run(vm, "about | head -1", "Exec85_0.", 30, "commands: about")
        terminal_run(vm, "get | head -1", "usage:_get", 30, "commands: get explains itself")
        terminal_run(vm, "svc 2>&1 | head -1", "usage:_exec85", 30, "commands: svc is a short form of exec85 --svc")
        press(vm, "f4")

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
        terminal_run(vm, "pkill -f 8080; echo KIL$((3-1))", "KIL2", 30, "services: kill the process")
        d = svc_line(vm, "web", lambda d: d["status"] == "running" and d["pid"] not in ("-", pid1), 60, after=n)
        step("services: it comes back by itself with a new process", d is not None, f"old pid {pid1}")
        terminal_run(vm, "wget -q -O /dev/null http://127.0.0.1:8080/; echo HTTP2=$?", "HTTP2=0", 60, "services: the website answers again")

        # ---- firewall: a closed port is not reachable from outside, an opened one is ----
        step("firewall: a port nobody opened is not reachable from outside", not http_probe(8081), "the website on 8080 answered from outside although no rule opens it")
        terminal_run(vm, "fw allow web 8080/tcp; echo FW$((5-3))", "FW2", 60, "firewall: a port is opened with fw allow")
        step("firewall: the opened port is reachable from the home network", wait_probe(8081, True))
        terminal_run(vm, "fw remove web; echo FR$((5-3))", "FR2", 60, "firewall: a port is closed with fw remove")
        step("firewall: and it is closed again", wait_probe(8081, False))

        # ---- SSH: key-only by default, reachable from the home network ----
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "citest", "-f", os.path.join(outdir, "id_test")], check=True)
        pub = open(os.path.join(outdir, "id_test.pub")).read().strip()
        terminal_run(vm, "exec85 --ssh enable > /tmp/ssh.txt 2>&1; echo SE$((5-3))", "SE2", 600, "ssh: the server is installed and switched on")
        try: terminal_run(vm, f"exec85 --ssh addkey \"{pub}\" > /tmp/key.txt 2>&1; cat /tmp/key.txt | cut -c1-40", "Key_added", 60, "ssh: a public key is added")
        except AssertionError:
            dump_file(vm, "/tmp/ssh.txt", 4); dump_file(vm, "/tmp/key.txt", 3)
            raise
        ssh = ["ssh", "-p", "2222", "-i", os.path.join(outdir, "id_test"), "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "tester@127.0.0.1"]
        ok = False; out = ""
        for _ in range(8):
            r = subprocess.run(ssh + ["echo", "SSH_OK_$((1+1))"], capture_output=True, text=True); out = r.stdout + r.stderr
            if "SSH_OK_" in r.stdout: ok = True; break
            time.sleep(4)
        step("ssh: logging in with the key works from outside", ok, out[-300:])
        r = subprocess.run(["ssh", "-p", "2222", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "BatchMode=yes", "-o", "PreferredAuthentications=password", "-o", "PubkeyAuthentication=no", "-o", "ConnectTimeout=8", "tester@127.0.0.1", "echo", "BAD"], capture_output=True, text=True)
        step("ssh: a password login is refused", "BAD" not in r.stdout and "Permission denied" in r.stderr, (r.stdout + r.stderr)[-300:])
        for f in ("id_test", "id_test.pub"):   # the test key is thrown away (and must not end up in the saved results)
            try: os.remove(os.path.join(outdir, f))
            except OSError: pass

        # ---- network settings: a fixed address and back to automatic, with the network still working each time ----
        terminal_run(vm, "exec85 --net set eth0 static 10.0.2.15/24 10.0.2.2 10.0.2.3 > /tmp/n.txt 2>&1; echo N1$((5-3))", "N12", 90, "network: a fixed address is applied")
        terminal_run(vm, "ip route | head -1 | cut -c1-30", "default_via_10.0.2.2", 30, "network: the gateway is the one that was chosen")
        terminal_run(vm, "head -1 /etc/resolv.conf", "nameserver_10.0.2.3", 30, "network: the name server is the one that was chosen")
        terminal_run(vm, "wget -q -O /dev/null https://piston-meta.mojang.com/mc/game/version_manifest_v2.json; echo NW$?", "NW0", 90, "network: names and the internet still work with the fixed address")
        terminal_run(vm, "exec85 --net set eth0 dhcp > /tmp/n2.txt 2>&1; echo N2$((5-3))", "N22", 90, "network: back to an automatic address")
        terminal_run(vm, "ip addr show eth0 | grep -c 'inet 10.0.2.15'; echo A$((5-3))", "re:[1-9]A2", 60, "network: the automatic address is back")
        terminal_run(vm, "wget -q -O /dev/null https://piston-meta.mojang.com/mc/game/version_manifest_v2.json; echo NX$?", "NX0", 90, "network: and the internet works again")

        # ---- sleep mode: the screen goes black, the service keeps running, a key wakes it without being used ----
        n = svc_count(vm, "web")
        press(vm, "f8", asleep="true")
        step("sleep: the screen is black", bar_pixel(vm.screenshot("t2-asleep")) == (0, 0, 0))
        time.sleep(6)
        last = [l for l in vm.text().splitlines() if "EXEC85_SVC name=web " in l][-1]
        step("sleep: the web service keeps running while the screen is off", svc_count(vm, "web") == n and "status=running" in last, last)
        press(vm, "ctrl-h", asleep="false", hidden="false")
        step("sleep: a key wakes the screen", bar_pixel(vm.screenshot("t3-awake")) == DARK["bar"])
        terminal_run(vm, "wget -q -O /dev/null http://127.0.0.1:8080/; echo HTTP3=$?", "HTTP3=0", 60, "sleep: the website still answers after waking")
        terminal_run(vm, "exec85 --svc add nginx --user root -- nginx -g 'daemon off;'; exec85 --svc start nginx; echo NG=$?", "NG=0", 60, "services: nginx defined and started")
        d = svc_line(vm, "nginx", lambda d: d["status"] == "running", 60)
        step("services: nginx is running", d is not None)
        terminal_run(vm, "wget -S -O /dev/null http://127.0.0.1/ 2>&1 | grep -o 'HTTP/1.[1] [0-9]*'", "HTTP/1.1_", 60, "services: nginx answers")
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
        type_text(vm, "exec85 --pkg add openjdk21-jre-headless > /tmp/j.txt 2>&1; echo JPKG=$?"); vm.monitor("sendkey ret")
        end = time.time() + 900; d = {}
        while time.time() < end:
            time.sleep(1)
            d = app_line(vm, "terminal", 1) or {}
            if re.search(r"JPKG=\d", d.get("l1", "") + d.get("l2", "") + d.get("l3", "")): break
        print("JAVA install ended after", round(time.time() - (end - 900)), "s:", d, flush=True)
        if "JPKG=0" not in (d.get("l1", "") + d.get("l2", "") + d.get("l3", "")):
            for i in list(range(9, 19)):
                try: terminal_run(vm, f"sed -n {i}p /tmp/j.txt | cut -c1-66; echo DG{i}", f"DG{i}", 20, "diagnostic")
                except AssertionError: pass
                print("DIAG>", i, (app_line(vm, "terminal", 1) or {}).get("l1"), flush=True)
            for cmd, mark in [("wc -l /tmp/j.txt; echo DG9", "DG9"), ("tail -2 /tmp/j.txt | head -1 | cut -c1-66; echo DG10", "DG10"), ("df -m / | tail -1 | tr -s ' ' '_'; echo DG21", "DG21"), ("grep MemTotal /proc/meminfo; echo DG22", "DG22"), ("mount | grep ' / ' | cut -c1-66; echo DG23", "DG23")]:
                try: terminal_run(vm, cmd, mark, 20, "diagnostic")
                except AssertionError: pass
                print("DIAG>", (app_line(vm, "terminal", 1) or {}).get("l1"), flush=True)
        step("software: Java installed with apk", re.search(r"JPKG=0", d.get("l1", "") + d.get("l2", "") + d.get("l3", "")) is not None, str(d))
        terminal_run(vm, "java -version 2>&1 | head -1; echo JAVA=$?", "JAVA=0", 120, "software: Java runs")

        # ---- the Minecraft helper: real download from Mojang, checksum, Java, EULA ----
        terminal_run(vm, "mc versions | head -1", "latest_", 120, "minecraft: the version list comes from Mojang")
        terminal_run(vm, "mc create craft2 $(mc versions | head -1 | cut -d' ' -f2) 2 > /tmp/m2.txt 2>&1; echo M2=$((5-3))", "M2=2", 900, "minecraft: the install command finishes")
        try: terminal_run(vm, "tail -1 /srv/minecraft/craft2/eula.txt", "eula=false", 30, "minecraft: the EULA stays unaccepted unless you tick it")
        except AssertionError:
            dump_file(vm, "/tmp/m2.txt", 5)
            raise
        terminal_run(vm, "mc create craft $(mc versions | head -1 | cut -d' ' -f2) 2 --eula > /tmp/m3.txt 2>&1; echo M3=$((5-3))", "M3=2", 900, "minecraft: the install-with-EULA command finishes")
        d = svc_line(vm, "craft", lambda d: d["status"] == "running" and d["pid"] != "-", 120)
        if d is None:
            dump_file(vm, "/tmp/m3.txt", 14, 6)
            terminal_run(vm, "svc log craft | tail -3 | cut -c1-66; echo LG$((5-3))", "LG2", 20, "diagnostic")
            print("LOG>", app_line(vm, "terminal", 1), flush=True)
        step("minecraft: the server is running as a service", d is not None)
        try: terminal_run(vm, "sleep 40; cp /var/log/exec85/craft.log /tmp/c.log; grep -c 'minecraft server version' /tmp/c.log; echo C$((5-3))", "re:[1-9]\\d*C2", 240, "minecraft: the server program starts (Java and the downloaded jar work)")
        except AssertionError:
            dump_file(vm, "/tmp/c.log", 10)
            print("SVC>", [l for l in vm.text().splitlines() if "name=craft " in l][-3:], flush=True)
            raise
        # Paper and Fabric: the real lists and downloads, checked; installed without starting (the vanilla server above is running)
        for kind, folder in (("paper", "plugins"), ("fabric", "mods")):
            terminal_run(vm, f"mc versions {kind} | head -1", "latest_", 120, f"minecraft: the {kind} version list comes from its official source")
            terminal_run(vm, f"mc create {kind}1 $(mc versions {kind} | head -1 | cut -d' ' -f2) 2 --type {kind} > /tmp/{kind}.txt 2>&1; echo K{kind[0]}$((5-3))", f"K{kind[0]}2", 900, f"minecraft: the {kind} install command finishes")
            try: terminal_run(vm, f"ls /srv/minecraft/{kind}1 | tr '\\n' ' '", f"re:{folder}", 30, f"minecraft: {kind} is installed (download checked, {folder} folder made)")
            except AssertionError:
                dump_file(vm, f"/tmp/{kind}.txt", 6)
                raise

        # Discord bots, Python and Node.js, installed for real with a fake token: the real library must run and refuse the token
        FAKE = "fake-token-for-tests-only-0123456789"
        for lang, name, needle, tag in (("python", "pybot", "Improper token", "D1"), ("node", "jsbot", "invalid token", "D2")):
            terminal_run(vm, f"export DISCORD_TOKEN={FAKE}; bot create {name} {lang} > /tmp/{name}.txt 2>&1; echo B{tag}$((5-3))", f"B{tag}2", 900, f"bot: the {lang} bot install command finishes")
            try: terminal_run(vm, f"sleep 30; grep -c '{needle}' /var/log/exec85/{name}.log; echo C{tag}$((5-3))", f"re:[1-9]\\d*C{tag}2", 240, f"bot: the {lang} bot runs and the Discord library refuses the fake token")
            except AssertionError:
                dump_file(vm, f"/tmp/{name}.txt", 6)
                terminal_run(vm, f"tail -3 /var/log/exec85/{name}.log | cut -c1-66; echo L{tag}$((5-3))", f"L{tag}2", 20, "diagnostic")
                print("BOTLOG>", app_line(vm, "terminal", 1), flush=True)
                raise
            terminal_run(vm, f"ls -l /srv/bots/{name}/.env | cut -c1-10", "-rw-------", 30, f"bot: the {lang} bot's token file is private")
            terminal_run(vm, f"svc stop {name}; echo S{tag}$((5-3))", f"S{tag}2", 60, f"bot: the {lang} bot can be stopped")

        # Quick installers from the app catalog: each is installed for real and must answer
        terminal_run(vm, "mkdir -p /srv/www/site; echo catalog-site-ok > /srv/www/site/index.html; app install website; echo W$((5-3))", "W2", 600, "catalog: the website installer finishes")
        step("catalog: the website answers from outside", wait_probe(8090, True, 60))
        terminal_run(vm, "wget -qO- http://127.0.0.1:8090/; echo W$((6-3))", "catalog-site-ok", 30, "catalog: the website serves the page")
        terminal_run(vm, "app list | grep website; echo W$((6-2))", "installed", 30, "catalog: the website shows as installed")
        terminal_run(vm, "app install syncthing gui_password=longenough1 > /tmp/st.txt 2>&1; echo Y$((5-3))", "Y2", 900, "catalog: the Syncthing installer finishes")
        terminal_run(vm, "sleep 20; wget -S -O /dev/null http://127.0.0.1:8384/ 2>&1 | grep -c HTTP/; echo Z$((5-3))", "re:[1-9]\\d*Z2", 120, "catalog: Syncthing's web page answers")
        terminal_run(vm, "app install transmission password=longenough1 > /tmp/tr.txt 2>&1; echo T$((5-3))", "T2", 900, "catalog: the Transmission installer finishes")
        terminal_run(vm, "sleep 15; wget -S -O /dev/null http://127.0.0.1:9091/transmission/rpc 2>&1 | grep -c HTTP/; echo U$((5-3))", "re:[1-9]\\d*U2", 120, "catalog: Transmission's web page answers")
        terminal_run(vm, "app remove website; app list | grep website; echo R$((5-3))", "re:website +no.*R2|no.*R2", 60, "catalog: the website can be removed")
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
