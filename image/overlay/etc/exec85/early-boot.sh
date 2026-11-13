#!/bin/sh
# Early boot: mount the virtual filesystems, set the host name, load drivers.
# Kept as one small script (instead of many OpenRC services) so boot stays fast.
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /dev/pts /dev/shm /run /tmp
mount -t devpts devpts /dev/pts
mount -t tmpfs -o mode=755 tmpfs /run
mount -t tmpfs -o mode=1777 tmpfs /tmp
[ -s /etc/hostname ] && hostname -F /etc/hostname

# Drivers: ask every device what module it needs ("modalias") and load those.
# Same method Alpine uses; no udev daemon needed.
echo /sbin/mdev > /proc/sys/kernel/hotplug
mdev -s
find /sys -name modalias -type f -print0 2>/dev/null | xargs -0 cat 2>/dev/null | sort -u | xargs modprobe -b -a 2>/dev/null
# Input drivers explicitly too (harmless if built in or already loaded).
modprobe -a evdev i8042 atkbd hid_generic usbhid 2>/dev/null
# Disk drivers explicitly. The disks themselves only appear AFTER their controller driver has loaded,
# so the one-pass scan above cannot find the drivers for them (sd_mod for SATA/USB disks, and so on).
modprobe -a sd_mod sr_mod ahci ata_piix ata_generic nvme virtio_blk virtio_scsi usb_storage uas 2>/dev/null
mdev -s
# Installed systems: mount /boot (and anything else) from /etc/fstab. The live disc has no fstab.
if ! grep -qw exec85.live /proc/cmdline; then
  mount -o remount,rw / 2>/dev/null   # the initramfs mounts the installed root read-only; services and package installs need it writable
  mount -a 2>/dev/null
fi
# The loopback interface (127.0.0.1): programs on this computer talk to each other through it (a website and its client, a database ...).
ip link set lo up 2>/dev/null
# Network and sound drivers by name (the one-pass scan above only finds drivers whose device is already visible).
modprobe -a e1000 e1000e r8169 virtio_net igb 2>/dev/null
modprobe -a snd_hda_intel snd_intel8x0 snd_ac97_codec snd_hda_codec_generic snd_hda_codec_realtek snd_hda_codec_hdmi 2>/dev/null
mdev -s
# Network: apply the saved settings (automatic addresses by default, fixed ones or Wi-Fi if chosen) in the background, so boot is not delayed.
/usr/bin/exec85 --net-apply >/dev/null 2>&1 &
exit 0
