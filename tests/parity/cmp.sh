#!/bin/bash
# usage: cmp.sh args...  -> compares output and generated units of pyuwsm vs uwsm
export XDG_RUNTIME_DIR=/run/user/0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
[ -n "${SHIM:-}" ] && export PATH=$SHIM:$PATH
D=${PARITY_OUT:-/tmp/uwsm-parity}/out; rm -rf $D; mkdir -p $D
for impl in pyuwsm uwsm; do
  rm -rf /run/user/0/systemd/user ~/.config/systemd/user 2>/dev/null
  # mask random unit suffixes; sort validation errors (uwsm keeps them in a set)
  $impl "$@" 2>&1 | sed -E 's/-[0-9a-f]{8}\.scope/-RAND.scope/g; s/@[0-9a-f]{8}\.service/@RAND.service/g' |
    awk '/^(<3>)?  err: /{print | "sort"; next} {close("sort"); print} END{close("sort")}' > $D/$impl.out
  echo "rc=${PIPESTATUS[0]}" >> $D/$impl.out
  mkdir -p $D/$impl.units
  [ -d /run/user/0/systemd/user ] && cp -a /run/user/0/systemd/user/. $D/$impl.units/
done
diff -r $D/pyuwsm.units $D/uwsm.units && diff $D/pyuwsm.out $D/uwsm.out && echo "SAME: $*"
