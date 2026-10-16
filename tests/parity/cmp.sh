#!/bin/bash
# usage: cmp.sh args...  -> compares output and generated units of pyuwsm vs uwsm
export XDG_RUNTIME_DIR=/run/user/0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
D=${PARITY_OUT:-/tmp/uwsm-parity}/out; rm -rf $D; mkdir -p $D
for impl in pyuwsm uwsm; do
  rm -rf /run/user/0/systemd/user ~/.config/systemd/user/wayland-* 2>/dev/null
  $impl "$@" > $D/$impl.out 2>&1; echo "rc=$?" >> $D/$impl.out
  mkdir -p $D/$impl.units
  [ -d /run/user/0/systemd/user ] && cp -a /run/user/0/systemd/user/. $D/$impl.units/
done
diff -r $D/pyuwsm.units $D/uwsm.units && diff $D/pyuwsm.out $D/uwsm.out && echo "SAME: $*"
