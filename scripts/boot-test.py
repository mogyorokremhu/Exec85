#!/usr/bin/env python3
"""Boot an Exec85 ISO in QEMU, wait for the ready marker, take a screenshot, report numbers.

usage: boot-test.py ISO bios|uefi OUTDIR [--accel kvm|tcg]
Exit code 0 = booted and the screen looks right.
"""
import glob, json, os, re, socket, subprocess, sys, time, zlib, struct

iso, firmware, outdir = sys.argv[1:4]
accel = sys.argv[sys.argv.index("--accel") + 1] if "--accel" in sys.argv else ("kvm" if os.access("/dev/kvm", os.W_OK) else "tcg")
os.makedirs(outdir, exist_ok=True)
serial_log = os.path.join(outdir, f"serial-{firmware}.log")
mon_path = os.path.join(outdir, f"mon-{firmware}.sock")
shot_ppm = os.path.join(outdir, f"screen-{firmware}.ppm")
for p in (serial_log, mon_path, shot_ppm):
    if os.path.exists(p): os.remove(p)

cmd = ["qemu-system-x86_64", "-machine", "q35", "-accel", accel, "-m", "512", "-smp", "2",
       "-cdrom", iso, "-boot", "d", "-vga", "std", "-display", "none", "-no-reboot",
       "-serial", f"file:{serial_log}", "-monitor", f"unix:{mon_path},server,nowait"]
if firmware == "uefi":
    code = next(iter(glob.glob("/usr/share/OVMF/OVMF_CODE_4M.fd") + glob.glob("/usr/share/OVMF/OVMF_CODE.fd") + glob.glob("/usr/share/ovmf/OVMF.fd")), None)
    if not code: sys.exit("OVMF firmware not found (apt install ovmf)")
    cmd += ["-drive", f"if=pflash,format=raw,readonly=on,file={code}"]

timeout = 120 if accel == "kvm" else 600
t0 = time.time()
q = subprocess.Popen(cmd)

def log_text():
    try: return open(serial_log, errors="replace").read()
    except FileNotFoundError: return ""

ready = mem = None
ready_wall = mem_wall = None
try:
    while time.time() - t0 < timeout:
        txt = log_text()
        if ready is None and (m := re.search(r"EXEC85_READY uptime=([\d.]+) display=(\w+) (\d+)x(\d+)", txt)):
            ready, ready_wall = m, time.time() - t0
        if ready is not None and (m := re.search(r"EXEC85_MEM total_kb=(\d+) used_kb=(\d+) exec85_rss_kb=(\d+)", txt)):
            mem, mem_wall = m, time.time() - t0
            break
        if "EXEC85_ERROR" in txt: break
        if q.poll() is not None: break
        time.sleep(0.2)

    if ready is not None:
        s = socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(mon_path)
        s.recv(4096); s.sendall(f"screendump {shot_ppm}\n".encode()); time.sleep(1.5)
        s.sendall(b"quit\n"); s.close()
finally:
    try: q.wait(timeout=10)
    except Exception: q.kill()

def read_ppm(path):
    data = open(path, "rb").read()
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", data)
    w, h = int(m[1]), int(m[2]); return w, h, data[m.end():]

def write_png(path, w, h, rgb):
    raw = b"".join(b"\x00" + rgb[y*w*3:(y+1)*w*3] for y in range(h))
    def chunk(t, d): c = struct.pack(">I", len(d)) + t + d; return c + struct.pack(">I", zlib.crc32(t + d))
    open(path, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))

result = {"firmware": firmware, "accel": accel, "booted": ready is not None}
screen_ok = False
if os.path.exists(shot_ppm):
    w, h, rgb = read_ppm(shot_ppm)
    write_png(os.path.join(outdir, f"screen-{firmware}.png"), w, h, rgb)
    c = rgb[((h // 2) * w + w // 2) * 3:][:3]; bar = rgb[(5 * w + w - 20) * 3:][:3]
    result["screen"] = f"{w}x{h}"; result["centre_rgb"] = list(c); result["bar_rgb"] = list(bar)
    # dark theme: background 0B1D2A, bar 1F3B4D
    screen_ok = tuple(c) == (0x0B, 0x1D, 0x2A) and tuple(bar) == (0x1F, 0x3B, 0x4D)
result["screen_ok"] = screen_ok
if ready:
    result.update(kernel_uptime_at_ready_s=float(ready[1]), display_driver=ready[2], host_seconds_to_ready=round(ready_wall, 2))
if mem:
    result.update(mem_total_kb=int(mem[1]), mem_used_kb=int(mem[2]), exec85_rss_kb=int(mem[3]))
json.dump(result, open(os.path.join(outdir, f"result-{firmware}.json"), "w"), indent=1)
print(json.dumps(result, indent=1))
if not (ready and mem and screen_ok):
    print("---- serial log (last 60 lines) ----"); print("\n".join(log_text().splitlines()[-60:]))
    sys.exit(1)
