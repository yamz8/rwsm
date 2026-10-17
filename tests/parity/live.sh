#!/bin/bash
# usage: live.sh IMPL [ENTRY]  -> full start, inspect, stop (ENTRY defaults to fakecomp.desktop)
# Headless wlroots compositors work with WLR_BACKENDS=headless WLR_RENDERER=pixman in the environment.
export XDG_RUNTIME_DIR=/run/user/0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
I=$1; E=${2:-fakecomp.desktop}; L=${PARITY_OUT:-/tmp/uwsm-parity}/live-$I-${E%.desktop}; rm -rf $L; mkdir -p $L
cd /tmp
XDG_SEAT=seat0 XDG_SESSION_ID=1 XDG_VTNR=1 USER=root LOGNAME=root setsid $I start -g -1 $E > $L/start.log 2>&1 < /dev/null &
SP=$!
for i in $(seq 1 30); do systemctl --user is-active -q wayland-wm@$E.service && break; sleep 0.5; done
sleep 2
systemctl --user list-units --all --no-legend 'wayland-*' 'graphical-session*' | awk '{print $1, $3, $4}' | sed 's/^● //; s/bindpid@[0-9]*\./bindpid@PID./' > $L/units
systemctl --user show-environment | grep -E '^(WAYLAND_DISPLAY|DISPLAY|SWAYSOCK|LABWC_PID|FAKECOMP_VAR|XDG_CURRENT_DESKTOP|XDG_SESSION_DESKTOP|UWSM_|DESKTOP_SESSION|XDG_MENU_PREFIX)' | sed 's/UWSM_WAIT_VARNAMES_SETTIME=.*/UWSM_WAIT_VARNAMES_SETTIME=X/' > $L/env
$I check is-active -v > $L/isactive 2>&1; echo "rc=$?" >> $L/isactive
ls $XDG_RUNTIME_DIR/uwsm/ > $L/runtime-files 2>&1
# PYTHONPATH and LC_CTYPE come from the pyuwsm wrapper
grep -vxE 'PYTHONPATH|LC_CTYPE' $XDG_RUNTIME_DIR/uwsm/env_cleanup.list 2>/dev/null | tr '\n' ' ' > $L/cleanup.list
$I stop > $L/stop.log 2>&1; echo "rc=$?" >> $L/stop.log
sleep 2
wait $SP; echo "start rc=$?" >> $L/start.log
systemctl --user list-units --all --no-legend 'wayland-*' 'graphical-session*' | awk '{print $1, $3, $4}' > $L/units-after
systemctl --user show-environment | grep -E '^(WAYLAND_DISPLAY|DISPLAY|SWAYSOCK|LABWC_PID|FAKECOMP_VAR|XDG_CURRENT_DESKTOP|UWSM_)' > $L/env-after
journalctl --user --no-pager -o cat --since "-60s" > $L/journal 2>&1
