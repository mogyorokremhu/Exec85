#!/bin/sh
# Build the Exec85 live ISO. Runs INSIDE an Alpine Linux container (see docs/BUILDING.md):
#   docker run --rm -v "$PWD":/src -v "$PWD/out":/out alpine:3.22 sh /src/scripts/build-image.sh
set -eu

SRC=${SRC:-/src}
OUT=${OUT:-/out}
W=/work
# "exec85.live" tells Exec85 it is running from the install image (so: show the installer).
CMDLINE=${KERNEL_CMDLINE:-"console=tty0 console=ttyS0,115200 rdinit=/sbin/init quiet loglevel=3 vt.global_cursor_default=0 exec85.live"}

echo "==> installing build tools"
apk add --no-cache rust cargo build-base xorriso kmod cpio gzip >/dev/null
# Limine bootloader. Alpine splits it: the tool, the x86_64 EFI file, the BIOS file, and the CD images.
apk add --no-cache limine limine-x86_64 limine-cd limine-sys >/dev/null
for p in $(apk info | grep -i limine); do echo "--- files of $p"; apk info -L "$p" | grep -v -e doc -e man; done
command -v limine >/dev/null || { echo "ERROR: limine not installed"; exit 1; }
findone() { find /usr -name "$1" 2>/dev/null | head -n1; }

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
    alpine-baselayout alpine-release busybox busybox-openrc openrc mdev-conf kmod \
    sfdisk e2fsprogs dosfstools tzdata doas mkinitfs ssl_client ca-certificates-bundle nftables >/dev/null
# Sound: a looping player for the installer's music, and the mixer tool. Optional: a missing package only means no music.
for pkg in mpg123 mpg123-alsa alsa-utils; do
    apk add --root "$R" --no-cache --keys-dir /etc/apk/keys "$pkg" >/dev/null 2>&1 || echo "WARNING: package $pkg is not available; the installer will have no music"
done
# Make sure the groups the installer puts the first user into exist (with a free group id).
for want in wheel:10 audio:18 video:27 input:105 users:100; do
    name=${want%%:*}; gid=${want##*:}
    grep -q "^$name:" "$R/etc/group" && continue
    while grep -q "^[^:]*:[^:]*:$gid:" "$R/etc/group"; do gid=$((gid + 100)); done
    echo "$name:x:$gid:" >> "$R/etc/group"
done
# The kernel package would try to build its own initramfs; we don't want that, so skip its scripts.
apk add --root "$R" --no-cache --no-scripts --keys-dir /etc/apk/keys linux-lts >/dev/null
KVER=$(ls "$R/lib/modules" | head -n1)
echo "kernel: $KVER"
depmod -b "$R" "$KVER"
cp "$R/boot/vmlinuz-lts" "$W/vmlinuz"

echo "==> building the boot image used by INSTALLED systems (mkinitfs)"
mkdir -p "$R/etc/mkinitfs"
echo 'features="base ata scsi usb virtio nvme ext4"' > "$R/etc/mkinitfs/mkinitfs.conf"
[ -e "$R/dev/null" ] || mknod -m 666 "$R/dev/null" c 1 3
chroot "$R" /sbin/mkinitfs -o /tmp/initramfs-install "$KVER"
mkdir -p "$R/usr/share/exec85/install"
mv "$R/tmp/initramfs-install" "$R/usr/share/exec85/install/initramfs"
cp "$W/vmlinuz" "$R/usr/share/exec85/install/vmlinuz"
cp "$(findone limine-bios.sys)" "$(findone BOOTX64.EFI)" "$R/usr/share/exec85/install/"
cp "$(command -v limine)" "$R/usr/bin/limine"
rm -f "$R/dev/null"
apk del --root "$R" mkinitfs >/dev/null 2>&1 || true
ls -l "$R/usr/share/exec85/install/"

# Firmware blobs are huge (hundreds of MB); they become an optional package set later.
rm -rf "${R:?}/lib/firmware"
echo "--- biggest directories in the rootfs (KB) ---"
du -sk "$R"/* "$R"/lib/modules/*/kernel/* 2>/dev/null | sort -rn | head -15
rm -rf "${R:?}/boot" "${R:?}/var/cache/apk" "${R:?}/usr/share/man" "${R:?}/usr/share/apk"

if [ -f "$SRC/image/assets/audio/installer.mp3" ]; then install -D -m 644 "$SRC/image/assets/audio/installer.mp3" "$R/usr/share/exec85/audio/installer.mp3"; fi
install -D -m 755 "$BIN" "$R/usr/bin/exec85"
# Our own terminal commands are this same program under other names.
for c in dir disk about get svc pkg mc bot fw; do ln -sf exec85 "$R/usr/bin/$c"; done
cp -a "$SRC/image/overlay/." "$R/"
mkdir -p "$R/proc" "$R/sys" "$R/dev" "$R/run" "$R/tmp" "$R/root"
# OpenRC: only the 'default' runlevel is used for now (user services live there from milestone 5).
mkdir -p "$R/etc/runlevels/default"

ROOTFS_KB=$(du -sk "$R" | cut -f1)
echo "rootfs size: ${ROOTFS_KB} KB"

echo "==> packing the initramfs"
(cd "$R" && find . | cpio -o -H newc 2>/dev/null | gzip -9) > "$W/initramfs"
ls -l "$W/vmlinuz" "$W/initramfs"

echo "==> building the bootable ISOs (BIOS + UEFI): normal and testing"
for f in limine-bios.sys limine-bios-cd.bin limine-uefi-cd.bin BOOTX64.EFI; do [ -n "$(findone $f)" ] || { echo "ERROR: $f not found"; exit 1; }; done
mkdir -p "$OUT"
# Same system image twice; only the kernel command line differs. The TESTING flavor adds "exec85.testing",
# which makes the installer offer "Skip installation" (set up in memory). It is released separately.
make_iso() {
    name=$1; cmdline=$2
    ISO=$W/iso-$name
    rm -rf "${ISO:?}"; mkdir -p "$ISO/boot/limine" "$ISO/EFI/BOOT"
    cp "$W/vmlinuz" "$ISO/boot/vmlinuz"
    cp "$W/initramfs" "$ISO/boot/initramfs"
    sed "s|@CMDLINE@|$cmdline|" "$SRC/image/boot/limine.conf" > "$ISO/boot/limine/limine.conf"
    cp "$(findone limine-bios.sys)" "$(findone limine-bios-cd.bin)" "$(findone limine-uefi-cd.bin)" "$ISO/boot/limine/"
    cp "$(findone BOOTX64.EFI)" "$ISO/EFI/BOOT/"
    xorriso -as mkisofs -R -r -J \
        -b boot/limine/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
        --efi-boot boot/limine/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label \
        "$ISO" -o "$OUT/$name" 2>&1 | tail -3
    limine bios-install "$OUT/$name"
}
make_iso exec85.iso "$CMDLINE"
make_iso exec85-testing.iso "$CMDLINE exec85.testing"

{
  echo "iso_bytes=$(stat -c %s "$OUT/exec85.iso")"
  echo "testing_iso_bytes=$(stat -c %s "$OUT/exec85-testing.iso")"
  echo "binary_bytes=$(stat -c %s "$BIN")"
  echo "rootfs_kb=$ROOTFS_KB"
  echo "initramfs_bytes=$(stat -c %s "$W/initramfs")"
  echo "kernel_bytes=$(stat -c %s "$W/vmlinuz")"
} > "$OUT/sizes.txt"
cat "$OUT/sizes.txt"
echo "==> done: $OUT/exec85.iso and $OUT/exec85-testing.iso"
