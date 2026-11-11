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
mdev -s
# Installed systems: mount /boot (and anything else) from /etc/fstab. The live disc has no fstab.
grep -qw exec85.live /proc/cmdline || mount -a 2>/dev/null
exit 0
