#!/bin/bash
# usage: daemon.sh IMPL -> run app daemon under the user manager, send requests
export XDG_RUNTIME_DIR=/run/user/0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
I=$1; O=${PARITY_OUT:-/tmp/uwsm-parity}/daemon-$I.out; : > $O
systemctl --user stop rwsm-test-daemon 2>/dev/null; systemctl --user reset-failed rwsm-test-daemon 2>/dev/null
rm -f $XDG_RUNTIME_DIR/uwsm-app-daemon-*
systemd-run --user --unit=rwsm-test-daemon --setenv=PATH=$PATH "$(command -v $I)" aux app-daemon >/dev/null 2>&1
sleep 1.5
send() { timeout 3 sh -c 'cat > "$1"' _ $XDG_RUNTIME_DIR/uwsm-app-daemon-in; timeout 12 cat $XDG_RUNTIME_DIR/uwsm-app-daemon-out | sed -E 's/-[0-9a-f]{8}\.scope/-RAND.scope/g; s/@[0-9a-f]{8}\.service/@RAND.service/g' >> $O; }
req() { printf '\0%s' app "$@" | send; }
printf ping | send
req fakecomp
req -t service -S out -- fakecomp "a b"
req viewer.desktop /a /b
req -T --app-id=x -- fakecomp
req nonexist
req -s bogus fakecomp
printf '' | send
printf '\0bogus\0thing' | send
echo '# Q12: below here only rwsm keeps answering' >> $O
req -x fakecomp
printf stop | send
sleep 0.5
systemctl --user is-active rwsm-test-daemon >> $O
