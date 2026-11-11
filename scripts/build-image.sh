#!/bin/sh
# Build the Exec85 live ISO. Runs INSIDE an Alpine Linux container (see docs/BUILDING.md):
#   docker run --rm -v "$PWD":/src -v "$PWD/out":/out alpine:3.22 sh /src/scripts/build-image.sh
set -eu

SRC=${SRC:-/src}
OUT=${OUT:-/out}
W=/work
CMDLINE=${KERNEL_CMDLINE:-"console=tty0 console=ttyS0,115200 rdinit=/sbin/init"}

echo "==> installing build tools"
apk add --no-cache rust cargo build-base xorriso kmod cpio gzip >/dev/null
# Limine bootloader. Alpine splits it: the tool, the x86_64 EFI file, the BIOS file, and the CD images.
apk add --no-cache limine limine-x86_64 limine-cd limine-sys >/dev/null
for p in $(apk info | grep -i limine); do echo "--- files of $p"; apk info -L "$p" | grep -v -e doc -e man; done
command -v limine >/dev/null || { echo "ERROR: limine not installed"; exit 1; }

echo "==> building the Exec85 program (static)"
cd "$SRC/ui"
RUSTFLAGS="-C target-feature=+crt-static" CARGO_TARGET_DIR="$W/target" cargo build --release
BIN="$W/target/release/exec85"
ls -l "$BIN"

echo "==> assembling the root filesystem"
R=$W/rootfs
rm -rf "${R:?}"; mkdir -p "$R/etc/apk" "$R/var/cache/apk"
cp /etc/apk/repositories "$R/etc/apk/"
apk add --root "$R" --initdb --no-cache --keys-dir /etc/apk/keys \
    alpine-baselayout alpine-release busybox busybox-openrc openrc mdev-conf kmod >/dev/null
# The kernel package would try to build its own initramfs; we don't want that, so skip its scripts.
apk add --root "$R" --no-cache --no-scripts --keys-dir /etc/apk/keys linux-lts >/dev/null
KVER=$(ls "$R/lib/modules" | head -n1)
echo "kernel: $KVER"
depmod -b "$R" "$KVER"
cp "$R/boot/vmlinuz-lts" "$W/vmlinuz"
# Firmware blobs are huge (hundreds of MB); they become an optional package set later.
rm -rf "${R:?}/lib/firmware"
echo "--- biggest directories in the rootfs (KB) ---"
du -sk "$R"/* "$R"/lib/modules/*/kernel/* 2>/dev/null | sort -rn | head -15
rm -rf "${R:?}/boot" "${R:?}/var/cache/apk" "${R:?}/usr/share/man" "${R:?}/usr/share/apk"

install -D -m 755 "$BIN" "$R/usr/bin/exec85"
cp -a "$SRC/image/overlay/." "$R/"
mkdir -p "$R/proc" "$R/sys" "$R/dev" "$R/run" "$R/tmp" "$R/root"
# OpenRC: only the 'default' runlevel is used for now (user services live there from milestone 5).
mkdir -p "$R/etc/runlevels/default"

ROOTFS_KB=$(du -sk "$R" | cut -f1)
echo "rootfs size: ${ROOTFS_KB} KB"

echo "==> packing the initramfs"
(cd "$R" && find . | cpio -o -H newc 2>/dev/null | gzip -9) > "$W/initramfs"
ls -l "$W/vmlinuz" "$W/initramfs"

echo "==> building the bootable ISO (BIOS + UEFI)"
ISO=$W/iso
rm -rf "${ISO:?}"; mkdir -p "$ISO/boot/limine" "$ISO/EFI/BOOT"
cp "$W/vmlinuz" "$ISO/boot/vmlinuz"
cp "$W/initramfs" "$ISO/boot/initramfs"
sed "s|@CMDLINE@|$CMDLINE|" "$SRC/image/boot/limine.conf" > "$ISO/boot/limine/limine.conf"
findone() { find /usr -name "$1" 2>/dev/null | head -n1; }
for f in limine-bios.sys limine-bios-cd.bin limine-uefi-cd.bin BOOTX64.EFI; do [ -n "$(findone $f)" ] || { echo "ERROR: $f not found"; exit 1; }; done
cp "$(findone limine-bios.sys)" "$(findone limine-bios-cd.bin)" "$(findone limine-uefi-cd.bin)" "$ISO/boot/limine/"
cp "$(findone BOOTX64.EFI)" "$ISO/EFI/BOOT/"
mkdir -p "$OUT"
xorriso -as mkisofs -R -r -J \
    -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
    --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label \
    "$ISO" -o "$OUT/exec85.iso" 2>&1 | tail -3
limine bios-install "$OUT/exec85.iso"

{
  echo "iso_bytes=$(stat -c %s "$OUT/exec85.iso")"
  echo "binary_bytes=$(stat -c %s "$BIN")"
  echo "rootfs_kb=$ROOTFS_KB"
  echo "initramfs_bytes=$(stat -c %s "$W/initramfs")"
  echo "kernel_bytes=$(stat -c %s "$W/vmlinuz")"
} > "$OUT/sizes.txt"
cat "$OUT/sizes.txt"
echo "==> done: $OUT/exec85.iso"
