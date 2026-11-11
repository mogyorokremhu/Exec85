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

## Results log

### M1 - first working image (CI run 7, commit 02c6b7e)

Setup: GitHub-hosted runner with hardware acceleration (KVM), QEMU q35, 512 MB RAM, 2 vCPUs,
live ISO, kernel Alpine `linux-lts` 6.12.117, verbose kernel log on serial (not yet `quiet`).

| | BIOS | UEFI |
|---|---|---|
| Kernel clock when screen is drawn | 2.47 s | 3.23 s |
| Wall clock, QEMU start to screen | 4.61 s | 13.83 s (OVMF firmware is slow) |
| Memory used (live image, 15 s after boot) | 183,516 kB of 473,476 kB | 184,780 kB of 467,756 kB |
| Exec85 UI process RSS | 4,328 kB | 4,360 kB |
| Display driver used | DRM | DRM |
| Screenshot check (theme colours) | pass | pass |

Sizes: UI binary 434 kB (static), root filesystem 143 MB (almost all kernel modules), initramfs 119 MB (compressed), kernel 13.7 MB.

Reading these honestly:
- Most of the 183 MB "used" is the live root filesystem itself sitting in RAM, not running programs. The installed system (M3) will show the real idle figure.
- No Debian comparison exists yet, so no "faster/lighter than Debian" claim is made.
- The module set is not pruned yet; trimming it is the obvious first size and boot-time win.

### M2 - the shell (CI run 13, commit 6c0028e)

Same setup as M1 (GitHub runner with KVM, 512 MB VM, live ISO, verbose kernel log).

| | BIOS | UEFI |
|---|---|---|
| Kernel clock when the shell is drawn | 3.09 s | 3.74 s |
| Wall clock, QEMU start to shell | 5.13 s | 14.48 s (OVMF firmware is slow) |
| Memory used, idle (live image) | 185,916 kB of 473,476 kB | 185,324 kB of 467,756 kB |
| Exec85 UI process RSS | 4,708 kB | 4,708 kB |

Change from M1: +0.6 s kernel clock, +2.4 MB used, +0.4 MB UI process. The M1 and M2 runs are different
CI machines, so differences this small are within run-to-run noise; the point is that the full shell
costs about 4.7 MB of RAM. The UI binary is still a single static file with no dependencies.

The boot test now also drives the real ISO with QEMU's virtual keyboard: 37 checks per firmware
(type-ahead, opening folders, menus, theme switching verified by screenshot pixels, pane focus,
layout toggles, dialogs). Screenshots from each run are in the `boot-bios` / `boot-uefi` artifacts.
