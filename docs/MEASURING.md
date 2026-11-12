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

### M3 - installer and login (CI run 20, commit d6ddf9b)

GitHub runner with KVM, QEMU q35, **1 GB RAM**, a virtual 8 GB SATA disk. The install image runs from RAM,
so the VM was raised from 512 MB (see below). Same scenario on both firmware types: install by keyboard,
restart from the disk, log in, log out.

| | BIOS | UEFI |
|---|---|---|
| **Installed system**, kernel clock at the login screen | 2.42 s | 2.77 s |
| **Installed system**, QEMU start to login screen | 3.21 s | 4.41 s |
| **Installed system**, memory used at the login screen (15 s after) | 151,124 kB of 988,804 kB | 153,716 kB of 983,080 kB |
| Login process RSS | 5,620 kB | 5,924 kB |
| Live install image, memory used (everything is in RAM) | 299,120 kB | 293,192 kB |
| Installation itself (confirm to finished) | 1.2 s | 1.4 s |

Reading these honestly:
- The installed-system row is the one to compare with other distributions; the live-image row is not.
  Nothing has been measured against Debian yet, so there is still **no "lighter than Debian" claim**.
- ~150 MB used at the login screen is more than I want. The likely culprits are the full set of kernel modules
  being loaded for the VM's devices and the kernel's own memory; trimming them is a known next step.
- The 1.2 s install is on a host-cached virtual disk. A real disk will be much slower.
- **The live install image needs more than 512 MB of RAM**: the boot image is now 154 MB compressed and 180 MB
  unpacked, and a 512 MB machine panics with "Unable to mount root fs". A 1 GB VM is fine. Making the installer
  image smaller (load the system from the disc instead of unpacking it all into RAM) is planned.

### M6 features (CI run for commit 0692064)

Same setup as M3 (GitHub runner with KVM, QEMU q35, 1 GB RAM for the installed-system scenario, BIOS and UEFI).

| | BIOS |
|---|---|
| **Installed system**, kernel clock at the login screen | 2.35 s |
| **Installed system**, QEMU start to login screen | 3.11 s |
| **Installed system**, memory used at the login screen (15 s after) | 151,180 kB |
| Live image kernel clock at the first screen | 1.94 s |

The new features did not change idle memory or boot time noticeably (they run only when used; the helper's
USB check costs one look at /sys every two seconds). Still **no comparison with Debian yet**.
