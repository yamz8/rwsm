#!/bin/bash
# Runs every parity check without supervision and exits non-zero on any
# difference. Run as root in a throwaway container with rwsm installed
# (make install PREFIX=/usr); see the parity job in .github/workflows/ci.yml.
# usage: ci.sh /path/to/uwsm-source [PYTHON]
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
export PARITY_OUT=${PARITY_OUT:-/tmp/uwsm-parity}
"$HERE/setup.sh" "$1" "${2:-python3}" || exit 1

# Every check below depends on the session bus and the user manager; when
# they are broken both implementations fail the same way and diffs pass.
export XDG_RUNTIME_DIR=/run/user/0 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
for i in $(seq 1 20); do
  systemctl --user is-system-running >/dev/null 2>&1 && break
  sleep 0.5
done
if ! systemctl --user show-environment >/dev/null; then
  echo "user manager is not reachable on the session bus" >&2
  systemctl --user --no-pager status 2>&1 | head -40 >&2
  exit 1
fi
echo "user manager: $(systemctl --user is-system-running 2>&1)"

install -m644 "$HERE"/fixtures/*.desktop /usr/share/applications/
mkdir -p /etc/xdg && echo faketerm.desktop > /etc/xdg/xdg-terminals.list
edge=$("$HERE/make-edge-entries.sh" /usr/share/applications)
shim=$(mktemp -d) && install -m755 "$HERE/fixtures/systemd-run" "$shim/"

n=0 failed=0
check() {
  local name=$1 out; shift
  n=$((n + 1))
  if out=$("$@" 2>&1); then
    echo "ok: $name"
  else
    failed=$((failed + 1))
    echo "FAIL: $name"
    printf '%s\n' "$out" | sed 's/^/    /'
  fi
}

cd /tmp
while IFS= read -r line; do
  case $line in '' | '#'*) continue ;; esac
  eval "set -- $line"
  if [ "$1" = app ]; then
    check "$line" env SHIM="$shim" "$HERE/cmp.sh" "$@"
  else
    check "$line" "$HERE/cmp.sh" "$@"
  fi
done < "$HERE/cases.txt"
for id in $edge; do
  check "app $id" env SHIM="$shim" "$HERE/cmp.sh" app "$id" /x
done

check "user manager still on the bus" systemctl --user show-environment

# Whole session with the stand-in compositor. Units and the compositor call
# /usr/bin/uwsm (aux exec, finalize, readiness), so it points at Python uwsm
# for the pyuwsm run. Logs and the journal hold PIDs and timestamps, so only
# the state files are compared.
mv /usr/bin/uwsm /usr/bin/uwsm.rwsm
printf '#!/bin/sh\nexec pyuwsm "$@"\n' > /usr/bin/uwsm && chmod +x /usr/bin/uwsm
"$HERE/live.sh" pyuwsm
mv -f /usr/bin/uwsm.rwsm /usr/bin/uwsm
"$HERE/live.sh" uwsm
check "live: session started" grep -q 'wayland-wm@fakecomp.desktop.service active' \
  "$PARITY_OUT/live-uwsm-fakecomp/units"
for f in units env isactive runtime-files cleanup.list stop.log units-after env-after; do
  check "live: $f" diff "$PARITY_OUT/live-pyuwsm-fakecomp/$f" "$PARITY_OUT/live-uwsm-fakecomp/$f"
done

# App daemon replies, up to the Q12 request where Python's daemon dies.
"$HERE/daemon.sh" pyuwsm
"$HERE/daemon.sh" uwsm
check "daemon answered" grep -qx pong "$PARITY_OUT/daemon-uwsm.out"
check "daemon replies" diff <(sed '/^# Q12/,$d' "$PARITY_OUT/daemon-pyuwsm.out") \
  <(sed '/^# Q12/,$d' "$PARITY_OUT/daemon-uwsm.out")
check "daemon survives Q12" grep -q "Stopping app daemon" "$PARITY_OUT/daemon-uwsm.out"

echo "$((n - failed))/$n parity checks passed"
[ "$failed" = 0 ]
