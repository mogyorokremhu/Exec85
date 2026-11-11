# How Exec85 measures boot time and idle memory

The UI program prints two lines on the serial console:

- `EXEC85_READY uptime=<s> display=<drm|fbdev> <w>x<h>` when the first screen is drawn.
  `uptime` is the kernel's clock, so it covers kernel start to screen, **not** firmware or bootloader time.
- `EXEC85_MEM total_kb=.. used_kb=.. exec85_rss_kb=..` 15 seconds later.
  `used_kb` = `MemTotal - MemAvailable` from `/proc/meminfo`.

`scripts/boot-test.py` also records the wall-clock time from starting QEMU to the ready line.

## Caveats (read before quoting numbers)

- The ISO is a **live** image: the whole root filesystem sits in RAM, so `used_kb` includes it.
  Numbers for the installed-to-disk system (milestone 3) are the fair comparison with Debian.
- CI runners may use software emulation (TCG), which is far slower than real hardware. Compare
  runs against each other, not against a real PC. The real laptop (milestone 6) is the reference.
- A Debian baseline measured with identical QEMU settings is still to be added; until then no
  "faster than Debian" claim is made.
