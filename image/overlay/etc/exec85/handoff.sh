#!/bin/sh
# The install disc starts an installed Exec85 without rebooting. busybox init runs this in its own place (it is PID 1 now)
# after it stopped the services and every other process. The installed disk is in /handoff-root.
say() { echo "EXEC85_HANDOFF $*" > /dev/console 2>/dev/null; }
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
dev=$(cat /handoff-root 2>/dev/null)
mkdir -p /newroot
if [ -n "$dev" ] && mount -t ext4 -o rw "$dev" /newroot; then
  mkdir -p /newroot/etc/exec85
  echo 1 > /newroot/etc/exec85/handed-over
  for m in dev proc sys; do mkdir -p /newroot/$m; mount -o move /$m /newroot/$m; done
  say "switching to $dev"
  exec switch_root /newroot /sbin/init
fi
say "failed (device: $dev); starting the disc's system again"
exec /sbin/init
