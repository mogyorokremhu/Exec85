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
