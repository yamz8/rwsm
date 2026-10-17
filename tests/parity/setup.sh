#!/bin/bash
# Sets up a throwaway container (as root, PID 1 not systemd) for comparing
# rwsm against Python uwsm under a real systemd user manager.
# Needs: systemd, dbus-daemon, python3-xdg, python3-dbus, a uwsm source tree.
# usage: setup.sh /path/to/uwsm-source PYTHON
set -e
SRC=$1; PY=${2:-python3}
HERE=$(cd "$(dirname "$0")" && pwd)

# reference implementation as `pyuwsm`, with the same paths as the installed rwsm
mkdir -p /opt/pyuwsm/uwsm
cp "$SRC"/uwsm/*.py /opt/pyuwsm/uwsm/
sed -e 's/@PROJECT_NAME@/uwsm/;s/@PROJECT_VERSION@/ref/;s/@BIN_NAME@/uwsm/' \
    -e 's|@BIN_PATH@|/usr/bin/uwsm|;s|@SH_BIN@|/bin/sh|;s/@WAITPID_BIN@/waitpid/' \
    -e 's/@WHIPTAIL_BIN@/whiptail/;s|@PKG_LIBEXEC_DIR@|/usr/libexec/uwsm|' \
    "$SRC/uwsm/params.py.in" > /opt/pyuwsm/uwsm/params.py
printf '#!/bin/sh\nPYTHONPATH=/opt/pyuwsm exec %s -c "import sys; sys.argv[0]=\\"uwsm\\"; from uwsm.main import main; main()" "$@"\n' "$PY" > /usr/local/bin/pyuwsm
chmod +x /usr/local/bin/pyuwsm

# stand-in compositor
install -m755 "$HERE/fakecomp" /usr/local/bin/fakecomp
install -Dm644 "$HERE/fakecomp.desktop" /usr/share/wayland-sessions/fakecomp.desktop

# buses, logind, journald and a user manager without systemd as PID 1.
# The user manager must not take over the session bus socket with its own
# dbus.socket (Arch's dbus-broker-units ship one), so mask those units.
mkdir -p /etc/systemd/user
for u in dbus.socket dbus.service dbus-broker.service; do
    ln -sf /dev/null /etc/systemd/user/$u
done
mkdir -p /run/systemd/system /var/run/dbus /run/user/0
chmod 700 /run/user/0
dbus-daemon --system --fork
dbus-daemon --session --address=unix:path=/run/user/0/bus --fork
setsid /usr/lib/systemd/systemd-journald >/dev/null 2>&1 &
setsid /usr/lib/systemd/systemd-logind >/dev/null 2>&1 &
XDG_RUNTIME_DIR=/run/user/0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus \
    setsid /usr/lib/systemd/systemd --user >/dev/null 2>&1 &
sleep 2
echo "ready: run cmp.sh <args> or live.sh pyuwsm|uwsm"
