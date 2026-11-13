# Exec85 build plan

Status: **draft, waiting for owner approval.** Nothing below is built yet.

This document answers four questions: what we build on, what language the
interface is written in, how the repo is laid out, and what the milestones are.
It also lists the places where the original wish list conflicts with the speed
goal or is not fully realistic, so those decisions are made openly.

---

## 0. Dev environment check (do this first)

I (Claude) run in a cloud container, not on your PC, so I **cannot** see your
BIOS, your Windows version, or whether WSL2/Docker work there. You need to run
a few commands and paste me the output. What I *can* see about my own
container: it has no `/dev/kvm` (no hardware virtualization), Docker is
installed, and its network policy currently blocks the Alpine download server
(a fixable setting, see "Open risks").

### What to run on your PC (Windows assumed; tell me if you're on something else)

Open PowerShell and run these one at a time:

```powershell
# 1. Windows edition and version
systeminfo | findstr /B /C:"OS Name" /C:"OS Version"

# 2. Is virtualization turned on? Look at the last lines of the output.
systeminfo | findstr /C:"Hyper-V Requirements" /C:"Virtualization Enabled In Firmware" /C:"A hypervisor has been detected"

# 3. WSL2
wsl --status
wsl -l -v

# 4. Docker
docker version
```

Quicker visual check: **Task Manager -> Performance -> CPU**, bottom right,
field "Virtualization". It says `Enabled` or `Disabled`.

### How to read the result

| What you see | Meaning | Fix |
|---|---|---|
| Task Manager says `Virtualization: Disabled` | The CPU supports VT-x (the i5-10400F does) but it is switched off in the BIOS. Most likely cause. | Reboot, enter BIOS/UEFI (usually `Del` or `F2`), find **Intel Virtualization Technology** (sometimes under Advanced -> CPU Configuration, or "VT-x"), set to **Enabled**, save. |
| `Virtualization: Enabled` but VMs still fail | Something in Windows is in the way (Windows Features "Virtual Machine Platform" / "Windows Hypervisor Platform" off, or an app such as an old VirtualBox/VMware clashing with Hyper-V). | I'll guide you from the exact error message. |
| `systeminfo` says "A hypervisor has been detected" | Windows is already using the hypervisor (normal when WSL2/Hyper-V is on). Not a problem. | Nothing. |

### Simplest working way to build and test (recommendation)

Ordered from simplest, and none of it depends on your PC supporting VMs:

1. **Build in GitHub Actions.** Every push builds the ISO in the cloud and
   uploads it as a downloadable file. Nothing to install on your PC. Works
   even if virtualization stays disabled.
2. **Test on your PC with QEMU for Windows** (install from qemu.weilnetz.de or
   `winget install SoftwareFreedomConservancy.QEMU`). I will give you the exact
   command per milestone. With virtualization enabled it can run accelerated
   (`-accel whpx`); with it disabled it uses **TCG software emulation**
   (`-accel tcg`), which is slower but fully works for a tiny OS. Expect the
   boot to take noticeably longer under TCG.
3. **Optional later: WSL2 + Docker** to build locally for faster iteration.
   Needs virtualization enabled.

Important about measurements: **boot times measured under TCG are not
representative** of real hardware. We will use TCG/QEMU numbers for
before/after comparisons (same settings, same machine) and treat the real
laptop (milestone 6) as the source of truth for the public claim.

---

## 1. Base system: Alpine Linux (userland) + Linux kernel, assembled by our own build scripts

### Recommendation

Use **Alpine Linux's packages and package manager (`apk`)** as the base, and
build the Exec85 image ourselves from them with scripts in this repo.

### Why not Buildroot

Buildroot makes very small images, but it produces a *fixed* system: there is
no package manager on the finished machine. Your requirement is that users can
install the software they want, with a good selection. Buildroot cannot give
that without us rebuilding the whole OS for each change.

### Why Alpine fits

| Need | Alpine gives |
|---|---|
| Small and fast | busybox (one tiny binary for the standard commands), musl libc, a base install of roughly a hundred-odd MB on disk with a small RAM footprint. (Numbers to be *measured*, not assumed.) |
| Real package manager, good selection | `apk` with a large repository: OpenJDK, Node.js, Python, nginx, Caddy, Docker/Podman, Samba, and more. |
| No systemd | Uses **OpenRC**, a lightweight init. |
| Services that restart on crash and don't need a login | OpenRC's `supervise-daemon` supervises a process and restarts it if it dies; OpenRC starts services at boot independently of any UI. |
| Stable, widely trusted | Mature project, security updates, used heavily in containers. |

### Init: OpenRC (not systemd, not something custom)

OpenRC is already the Alpine default, supports boot-time services, dependency
ordering and `supervise-daemon` auto-restart. I considered `s6` (even lighter,
very elegant) but it makes it harder for users to follow normal Alpine
documentation. We can revisit if measurements say OpenRC is a bottleneck.

### Kernel

Start with Alpine's `linux-lts` (broad driver coverage, kernel modules for
common hardware, both BIOS and UEFI). This gives us "drivers for common
hardware" on day one. Later, once there are measurements, we can build a
trimmed custom kernel to cut boot time. **That is an optimization, not a
starting point.**

### Honest trade-offs of Alpine

- **musl instead of glibc.** Most things (Java, Node, Python, Go, nginx) work
  fine. Some downloaded *binary-only* programs built for glibc will not run
  without the `gcompat` compatibility package. We will document this for
  users. Minecraft (Java) works with Alpine's OpenJDK packages.
- **Wi-Fi and some hardware need firmware blobs** (`linux-firmware`), which
  are large. We will make firmware a selectable package set rather than
  always-on, or the base image gets heavy.

### Bootloader

**Limine.** One small bootloader that supports both BIOS and UEFI, and handles
GPT disks (needed for your >2 TB drive). GRUB also works but is much bigger
and more complex. (Note: 4 TB needs a GPT partition table. On BIOS machines
that means a tiny "BIOS boot" partition. The installer will handle it.)

### Image types

1. **Live ISO** (hybrid, boots on BIOS and UEFI, USB-stick or CD): boots
   straight into the Exec85 installer, and can also run as a live system.
2. **Installed system**: ext4 root filesystem on the chosen disk plus
   Limine.

For fair measurements the **installed system** is the thing compared to
Debian, because a live ISO loads its files into RAM, which inflates memory
numbers.

---

## 2. Language for the interface: Rust

### Recommendation

Write the interface, and all the apps, in **Rust**, as **one small statically
linked program** (busybox-style: one binary, with the shell/login/installer/
apps as subcommands or built-in modules).

### Why

- **Small and fast with no runtime.** No garbage collector, no interpreter, no
  VM. A static musl binary is a few MB and starts instantly, which directly
  serves the idle-RAM and boot-time goals.
- **Memory safety.** The login screen and installer handle passwords and
  partition disks. Bugs in C there are dangerous. Rust prevents whole classes
  of them.
- **Great low-level libraries** for what we need: talking to DRM/KMS and
  evdev (keyboard/mouse), pseudo-terminals for the Cmd app, and so on.
- **Easy cross-compiling to musl** in CI.

### Alternatives I considered

| Language | Verdict |
|---|---|
| C | Smallest and most "obvious" for this domain, but you'd be learning low-level work on unsafe ground. Not recommended for a first OS project that handles passwords. |
| Go | Easier to learn, but the runtime and garbage collector cost several MB of RAM and add startup work. That works against the main goal. |
| Zig | Promising, but the ecosystem is younger and fewer people could contribute after public release. |

### Honest note for you

Rust has a steeper learning curve than Luau, mostly the "borrow checker". I
will keep the code simple (plain structs and functions, minimal cleverness)
and explain each new concept the first time it appears. You do not need to
know Rust to follow the milestones.

### How the screen works (no X11, no Wayland)

- **Drawing:** the program asks the kernel for a "dumb buffer" through
  **DRM/KMS** (the modern Linux display API), draws pixels into it itself in
  software, and flips it to the screen. If DRM is not available it falls back
  to the older **framebuffer** (`/dev/fb0`). The UI is simple 2D tiles and
  text, so we do **not** need GPU acceleration, and it therefore works on
  any GPU the kernel can set a video mode on (Intel, AMD, NVIDIA via the
  open `nouveau` driver, or the generic EFI/VESA fallback).
- **Font:** a bitmap font compiled into the binary. I'll pick an
  openly-licensed one (e.g. an OFL-licensed font) and record its license; no
  Microsoft fonts.
- **Input:** read keyboard (and optional mouse) directly from `/dev/input`
  (evdev). We do our own keyboard layout tables (US, Hungarian, German, etc.).
  The set of layouts will start small and grow.
- **Windows:** a tiled layout engine. Windows are rectangles that divide the
  screen and never overlap, matching the original idea.
- **Theme:** one colour palette object (light or dark) used by every widget,
  which is how "light/dark applies to the whole interface" is guaranteed.

### Privileges and security model

- Login screen authenticates against `/etc/shadow`, then the session runs as
  that **unprivileged user**.
- Administrative actions (start/stop services, install packages, network
  changes) go through a tiny privileged helper (`doas`-style, permitting only
  users in an admin group) rather than running the whole UI as root.
- An empty password is allowed because you asked for "optional password", but
  the installer will warn and, by default, SSH password login for an
  empty-password account will be disabled.

---

## 3. Repo layout

```
Exec85/
  README.md
  LICENSE                  (to be chosen, see open questions)
  docs/
    PLAN.md                this file
    BUILDING.md            how to build and test (per OS)
    MEASURING.md           how boot time / idle RAM are measured
    INSTALL.md             public install instructions (milestone 7)
  ui/                      Rust workspace: the Exec85 program
    crates/
      gfx/                 DRM/fb drawing, bitmap font, theme palette
      input/               evdev keyboard/mouse, keymaps
      shell/               menu bar, drive row, file list, tiling windows
      login/               login screen + authentication
      installer/           installer logic and screens
      apps/                explorer, notepad, cmd, paint, monitor, disk usage,
                           network, clock, calculator, reversi, snake
      services/            service manager (talks to OpenRC)
      exec85/              the single binary that ties it together
  image/
    packages.list          apk packages in the base image
    overlay/               files copied as-is into the image (/etc, init scripts)
    kernel/                kernel config notes / later custom config
    boot/                  Limine config
  scripts/
    build-image.sh         builds the ISO (runs inside a container)
    run-qemu.sh / .ps1     boots the ISO in QEMU (Linux/macOS and Windows)
    measure.sh             boots in QEMU and records boot time + idle RAM
    baseline-debian.sh     the same measurement on a minimal Debian, for comparison
  tests/                   unit tests for ui crates, plus boot smoke tests
  .github/workflows/
    build.yml              build the ISO on every push, upload as artifact
    release.yml            tagged release (milestone 7)
```

Nothing in the repo contains a hardcoded user name, host name, disk, or
program. Those come from the installer (stored in `/etc/exec85/`) or from the
user at runtime.

---

## 4. Measuring "faster boot, less RAM" honestly

- **Boot time:** use the kernel's own timestamps plus a marker written to the
  serial console when the UI is ready. The script reports: firmware-to-kernel,
  kernel-to-init, init-to-login-screen. Total = power on to login screen.
- **Idle RAM:** after boot, wait 30 seconds, then read `MemTotal - MemAvailable`
  from `/proc/meminfo`, plus a per-process list.
- **Baseline:** the same numbers for a minimal Debian server install (netinst
  with only "standard system utilities" and SSH) in the *same* QEMU settings
  (same RAM size, CPU count, disk type), so the comparison is apples to
  apples.
- Every milestone report includes these numbers, plus the image size.
- Results are saved in `docs/MEASURING.md` and shown in the README at release
  time. Until we have measured on real hardware, I will label numbers as
  "QEMU" and will not claim a speed-up that has not been measured.

---

## 5. Milestones

Each milestone ends with: how to run it, the boot-time and idle-memory report,
and a stop for your OK.

### M1. Minimal image boots in QEMU to a blank Exec85 screen
- Docker-based build script creates a bootable ISO (Alpine rootfs + linux-lts
  + Limine, BIOS and UEFI).
- A first tiny Rust program draws a blank (themed) screen through DRM, with a
  framebuffer fallback.
- GitHub Actions builds the ISO and uploads it.
- Boot time and idle RAM measurement script, plus the Debian baseline.
- **Exit test:** ISO boots in QEMU under both BIOS and UEFI firmware to the
  blank screen.

### M2. The shell
- Menu bar, drive row, file list navigated with up/down arrows, Enter to
  open, Tab to move between areas, shortcuts for menus. Tiling window engine.
- Light/dark palette.
- Unit tests for layout and navigation logic.

### M3. Installer and login screen
- Installer: light/dark, username, optional password, device name, disk,
  timezone, keyboard layout. Handles GPT, UEFI and BIOS. A confirm screen
  before anything is erased.
- Login screen on every start.
- **Exit test:** automated install into a virtual disk in QEMU, reboot, log in.

### M4. Core apps
Explorer, Notepad, Cmd (terminal emulator running `ash`), Paint, system
monitor, disk usage, network settings, clock and calendar, calculator,
Reversi, Snake. Built in that order of importance; the games stay tiny.

### M5. Service manager and software installation
- Services UI: add any program as a service (command, working directory,
  user, environment), start, stop, restart, see status and logs.
  Implementation: generates OpenRC scripts using `supervise-daemon` (restart
  on crash, start at boot, independent of the UI and of anyone being logged
  in).
- Software installation UI on top of `apk` (search, install, remove, update).
- **Proof with your three workloads**, with nothing specific built into the OS:
  Minecraft Java server, a Discord bot (Node or Python), and a couple of
  websites behind nginx or Caddy. Test: kill each process and confirm it comes
  back; reboot with nobody logged in and confirm they are up.

### M6. Real hardware and everyday features
Features (built and tested in QEMU first, in this order):
1. **Pendrives.** Detect a USB drive when it is plugged in, show it in the Explorer's drive row, mount it,
   and support copy, cut, delete, rename and edit on it, plus a safe **Eject**. FAT32, exFAT and NTFS.
   Cutting between the pendrive and the computer copies first and deletes the original only after the copy
   succeeded. Never run anything from the drive automatically.
2. **Our own terminal commands**, written in Rust inside the `exec85` program, next to the normal shell and
   BusyBox (which stay, so scripts keep working): `help`, `dir`, `disk`, `top`, `about`, `get`, then more.
   Tab completion and history in the shell.
3. **Minecraft server helper** (a preset on top of the generic "download a file and run it as a service"
   feature; nothing Minecraft-specific in the OS): choose the **version** from Mojang's list, choose **2, 4 or
   8 GB** of RAM (only choices that fit the machine), install the right Java, and an **EULA tick box** (off by
   default, with the link to the text) that writes `eula=true` only when ticked.
4. **Sleep mode**: turns the screen off after a chosen time or from a menu item, wakes on any key, and keeps
   all services running. (Real suspend, which stops everything, stays off by default.)

Real hardware, on the Christmas laptop: BIOS or UEFI, drivers, Wi-Fi, 4 TB GPT disk, lid-close behaviour, real
boot time and idle RAM numbers. Fix the issues found. Collect a hardware compatibility list. Also: the Debian
comparison for boot time and idle RAM, and changing network settings from the Network app.

### M6.6. More quick installers (decided, to be built after M6.5)
Same recipe system and same look as the Minecraft server and Discord bot installers: pick the options, press Install,
it runs as a normal service. Each one gets an app, a short terminal command and a CI test (install, start, answer).
1. **Website** (nginx): a folder for your files, a port, a starter page; optional HTTPS with a self-made certificate.
2. **File sharing** (Samba): choose folders to share, a user and password; visible to Windows and phones on the network.
3. **Syncthing**: keeps folders in sync between your devices; its web page is limited to the local network.
4. **Git server** (Forgejo): a small private GitHub; admin account created during install.
5. **Media server**: music with Navidrome and videos/music over DLNA with MiniDLNA (Jellyfin needs a much heavier
   runtime that does not fit this OS; revisit later).
6. **Home-network ad blocker** (DNS blocker with a downloaded block list, kept up to date); the router or each device
   must be pointed at this computer to use it.

### M6.7. Remote access (decided, to be built)
Security rules for all of it: **nothing is reachable from outside the local network by default**; a simple firewall
(default: refuse incoming connections, allow only what an installed service needs); keys or strong passwords, no
default passwords; every remote feature can be switched off in one place.
1. **SSH server**: install and turn on from a screen; key-only login by default (with an easy "add my key" step, or a
   password option you must choose on purpose); SFTP works too.
2. **Web dashboard**: open the server's status, services, logs, disks and pendrives from a browser on another
   computer. Log in with your Exec85 account. Plain HTTP only on the local network; an HTTPS option (self-made
   certificate) follows. Needs a small web server inside the OS (no extra libraries), so it is a bigger job.
3. **VPN (WireGuard)**: create the server, show a QR code / config for each phone or laptop; the router must forward
   one port, and the screen explains how.
4. **Wake-on-LAN**: turn it on for this computer's network card and show the address needed to wake it from a phone
   (the sleeping computer cannot send the signal itself); also a "wake another computer" button.
Suggested order: SSH, firewall, WireGuard, Wake-on-LAN, then the web dashboard; installers: website, Samba,
Syncthing, Forgejo, ad blocker, media server.

### M6.8. Updates, uptime monitor with alerts, Vaultwarden (decided, to be built)
1. **Updates, detected automatically.** Two kinds, shown together in one Updates screen and as a notice in the status
   bar when something is waiting:
   - *Software updates* (`apk`): checked once a day; the screen lists what would change; install now or at a chosen
     time (never a surprise restart).
   - *Exec85 updates*: the system looks for a newer release in the release list, downloads the image, **checks its
     checksum (and a signature before the public release)**, unpacks it beside the running system, switches the boot
     entry to it, and keeps the previous version as a fallback boot entry. Your files, settings, services and `/srv`
     data are kept. A bad update must never leave the computer unbootable, so this is designed first and tested in CI by
     updating a running virtual machine from one build to the next, including a deliberately damaged download.
   - Honest limit: while the repository is private, the release list cannot be read without a token; the
     feed address is a setting, so this works fully once the project is public (or with a token until then).
2. **Uptime monitor with alerts**, built into Exec85 (a small background program, almost no RAM; Uptime Kuma is a
   heavy Node app and is not needed). Checks: a website (HTTP status and speed), a port, ping, one of your services,
   disk space, disk health and temperature. Each check has an interval. **Alerts** go to a Discord webhook or to ntfy
   (push notifications to your phone), with quiet hours, "only after N failures in a row" and a message when it
   recovers. The system also uses it for its own alerts: a service crashed, a disk is nearly full, an update is waiting.
3. **Vaultwarden** (your own password manager, works with the Bitwarden apps and browser extensions): quick installer
   with the same screens as the others; admin token typed in a hidden field; new sign-ups switched off after you
   have made your account; data kept in `/srv/vaultwarden`, with a reminder to back it up. The Bitwarden apps only work
   over **HTTPS with a certificate they trust**, so this depends on the website/reverse-proxy and DuckDNS items (or
   on reaching it through the VPN); the screen explains which option you chose and what is missing.

### Release plan after v0.7.0 (decided; order inside a release can change)
**v0.8.0 - "everyday server"** (released: Minecraft EULA button, firewall, SSH, network settings and Wi-Fi, App catalog with website / Syncthing / Transmission; the items marked *moved* went to v0.8.5)
- Minecraft: an "Accept the EULA and start" button on the done screen, clearer messages for too little memory or a failed download.
- The quick installers become *recipes* (see M6.5), plus an **App catalog** screen: one place that lists every installer, what it needs (memory, disk, network), and whether it is installed.
- Remote access, first part: **SSH server** (key-only by default) and the **firewall** (default: refuse incoming; each installer opens only its own port).
- Network: **settings and Wi-Fi** in the Network app (automatic or fixed address, turn an interface on/off, join a Wi-Fi network).
- New installers: **website** (nginx), **file sharing** (Samba), **Syncthing**, **PostgreSQL / MariaDB** (one click, a database and a user for your own projects), **Transmission** (torrents, with a web page), **print server** (CUPS).
- More **game servers** in the same style as Minecraft (version, memory, ports, restart on crash): **Terraria**, **Factorio**, **Valheim**, **Minecraft Bedrock** (the phone/console edition). Each is only released once its real download works in CI.
- **Local name**: `exec85.local` (or your device name) works on the home network without knowing the IP address.
- **Disk health and temperature**: a screen with each disk's health (SMART), its temperature and the CPU temperature, with a warning colour; the laptop will need it.
- **Scheduled backups**: choose folders and a destination (a pendrive or another disk), pick a time, and restore from a list of past backups.
- **Users**: add or remove users and change passwords from a screen (with the wheel/administrator choice made explicit).
- **Scheduled tasks**: a simple screen for "run this command every night / every hour".
- **Log viewer**: read the system's and any service's log in one place, with search.
Honest note: this was a lot, so the rest moved to a **v0.8.5** release rather than being rushed:
**v0.8.5 - "the rest of everyday"** *(moved from v0.8.0)*: file sharing (Samba), PostgreSQL / MariaDB, print server (CUPS), the extra game servers (Terraria, Factorio, Valheim, Bedrock), `exec85.local`, disk health and temperature, scheduled backups, user management, scheduled tasks, log viewer.

**v0.9.0 - "reach it from anywhere"**: Git server (Forgejo), **WireGuard** VPN, **Wake-on-LAN**, **dynamic DNS** (DuckDNS), **reverse proxy with HTTPS** (Caddy), the ad blocker (DNS blocker), the media server (Navidrome + MiniDLNA), Home Assistant (with a clear memory warning).

**v0.10.0 - "look after itself"**: **web dashboard**, **automatic updates** (software and Exec85 itself, with a fallback boot entry), the **uptime monitor with alerts** (Discord webhook / ntfy), **Vaultwarden**.

**Whenever the laptop arrives**: real-hardware testing (drivers, Wi-Fi, 4 TB GPT disk, lid-close, real numbers), the Debian comparison.

### M7. Public release
**Before going public:** the installer music is now our own (v0.8.0), but the earlier third-party song is still in the git history:
clean the history (or publish from a fresh one); see `image/assets/audio/README.md`.

Release workflow, signed checksums, install instructions (`docs/INSTALL.md`),
USB-writing guide for Windows, Linux and macOS, license and third-party
notices, then the repo goes public.

---

## 6. Items that conflict with a goal or are not fully realistic

1. **"Closing the lid must not suspend, as a setting."** Realistic and easy:
   without systemd's `logind`, Linux does not suspend on lid close unless we
   tell it to. We add a setting in `/etc/exec85/power.conf`
   (`lid_close = ignore | suspend | poweroff`) with default `ignore`, handled
   by `acpid`. Also screen blanking when the lid is closed to save power.
2. **"Drivers for common hardware" vs "tiny and fast".** Alpine's kernel is
   modular; modules load only for hardware present, so there is little
   boot-time cost. Wi-Fi firmware and some GPU/NIC firmware are large, so
   they will be a selectable package set. Needs measurement.
3. **"Faster boot than Debian".** Very likely, but not yet *proven*. The
   plan's measurement section exists so the claim is proven. A live ISO also
   loads into RAM, so the fair comparison is the installed system.
4. **Everything in the apps list is a lot of work.** Cmd (a real terminal
   emulator) and Paint are the largest. Order in M4 puts the useful ones
   first; Paint and the games come last and may be simplified.
5. **NVIDIA proprietary drivers are not planned.** The UI is 2D software
   rendering, so it does not need them. The kernel's open drivers are enough.
6. **musl vs glibc** (see section 1): a few binary-only programs need `gcompat`.
7. **No Microsoft names, logos, icons or fonts.** The look will be inspired,
   not copied: own icons (simple pixel art made for this project), own colour
   palette, own bitmap font choice.

---

## 7. Open risks for me, in my current cloud environment

- This container has **no KVM** and I currently could not find QEMU in its
  package list, and its network policy returned `403 Forbidden` for the
  Alpine download server. I might not be able to boot-test inside this
  container at all. If so, the testing loop is: I push -> GitHub Actions
  builds the ISO and runs an automated QEMU boot test (CI machines have
  better access) -> you download and try it. I will try the simplest fix
  first (allowing `dl-cdn.alpinelinux.org` in the environment's network
  settings) and report back at the start of M1.

---

## 8. Questions for the owner (answers needed before M1)

1. **License** for public release? Suggested: MIT or Apache-2.0 for our code
   (Linux kernel and Alpine packages keep their own licenses regardless).
2. **Language:** OK with Rust?
3. **Base:** OK with Alpine + OpenRC + Limine?
4. **Results of the environment check** in section 0 (Windows version,
   virtualization enabled?, WSL2/Docker status).
