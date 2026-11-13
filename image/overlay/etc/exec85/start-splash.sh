#!/bin/sh
# The boot screen runs in the background while the system starts; the supervisor stops it when the first real screen is ready.
/usr/bin/exec85 --splash >/dev/null 2>&1 &
exit 0
