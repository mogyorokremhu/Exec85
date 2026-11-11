# Building and testing Exec85

You do **not** need a Linux machine, and you do **not** need virtualization (VT-x) enabled.

## Easiest: let GitHub build it

Every push runs `.github/workflows/build.yml`, which

1. runs the unit tests,
2. builds `exec85.iso` inside an Alpine Linux container,
3. boots the ISO in QEMU twice (BIOS and UEFI), waits for the Exec85 screen, takes a screenshot and checks it.

Open the repo's **Actions** tab, pick the latest run, and download:

- `exec85-iso` - the bootable ISO
- `boot-bios` / `boot-uefi` - screenshot (`screen-*.png`), the serial log and the measured numbers (`result-*.json`)

## Run the ISO on your own PC (Windows)

1. Install QEMU: `winget install SoftwareFreedomConservancy.QEMU`
2. In PowerShell, in the folder with `exec85.iso`:

BIOS boot, software emulation (works even with virtualization disabled; slower):

```powershell
& "C:\Program Files\qemu\qemu-system-x86_64.exe" -m 512 -smp 2 -cdrom exec85.iso -boot d -vga std -accel tcg
```

If virtualization is enabled in your BIOS, replace `-accel tcg` with `-accel whpx` for near-native speed.

UEFI boot additionally needs firmware: add
`-drive if=pflash,format=raw,readonly=on,file="C:\Program Files\qemu\share\edk2-x86_64-code.fd"`.

## Build locally (Linux, or Windows with WSL2 + Docker)

```bash
mkdir -p out
docker run --rm -v "$PWD":/src -v "$PWD/out":/out alpine:3.22 sh /src/scripts/build-image.sh
python3 scripts/boot-test.py out/exec85.iso bios results
python3 scripts/boot-test.py out/exec85.iso uefi results
```

The UI program alone can be tested anywhere Rust is installed:

```bash
cd ui && cargo test
cargo run --release -- --ppm /tmp/screen.ppm   # renders the screen to an image file
```

## Trying the shell (milestone 2)

Click into the QEMU window so it receives your keyboard (press `Ctrl+Alt+G` to give the mouse back).

| Keys | What happens |
|---|---|
| Up / Down, PgUp / PgDn, Home / End | move through the file list |
| Enter or Right | open the selected folder; Backspace or Left goes up |
| a letter, e.g. `e` | jump to the next name starting with it |
| F10, or Alt+F / Alt+V / Alt+W / Alt+H | open the menu bar; arrows move, Enter chooses, Esc closes |
| View > Light theme / Dark theme | the whole interface switches theme |
| Tab / Shift+Tab | move between the drive row, Files and Details |
| Ctrl+L, Ctrl+D, Ctrl+H | stack the panes, hide the details pane, show hidden files |
| F1 | the full shortcut list |
| File > Shut down / Restart | asks for confirmation first (Y / N) |

You start in `/`, the live system's own files (it runs from RAM; nothing on your PC is touched).

## Trying the installer (milestone 3)

> **The installer erases the whole disk you choose.** Use a virtual machine. It has not been tested on real
> hardware yet.

You need a virtual disk and at least 1 GB of RAM for the installer (it runs from memory). On Windows:

```powershell
& "C:\Program Files\qemu\qemu-img.exe" create -f qcow2 exec85-disk.qcow2 16G
& "C:\Program Files\qemu\qemu-system-x86_64.exe" -m 1024 -smp 2 -cdrom exec85.iso -boot d -drive file=exec85-disk.qcow2,if=ide -vga std -accel tcg
```

Choose **Install Exec85** and follow the steps (everything works from the keyboard; type to search the
keyboard-layout and time-zone lists). When it says the installation is complete, press Enter to restart, close
the window, and start it again **without the CD** to boot from the disk:

```powershell
& "C:\Program Files\qemu\qemu-system-x86_64.exe" -m 1024 -smp 2 -drive file=exec85-disk.qcow2,if=ide -vga std -accel tcg
```

You get the login screen. Log in with the name and password you chose. File > Log out returns to the login screen.
Delete `exec85-disk.qcow2` to start over. To try the system without installing, pick **Try Exec85 without installing**.

For UEFI, add `-drive if=pflash,format=raw,readonly=on,file="C:\Program Files\qemu\share\edk2-x86_64-code.fd"`.
