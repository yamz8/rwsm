# Parity harness

Runs rwsm (installed as `uwsm`) and Python uwsm (`pyuwsm`) side by side in a
throwaway root container and diffs what they do.

1. `make install PREFIX=/usr`
2. `tests/parity/setup.sh /path/to/uwsm-source python3.12`
3. `tests/parity/cmp.sh start -o fakecomp.desktop` compares output, exit code
   and generated units for any command line.
4. `tests/parity/live.sh pyuwsm` then `live.sh uwsm`, then
   `diff -r /tmp/uwsm-parity/live-pyuwsm /tmp/uwsm-parity/live-uwsm` runs a
   whole session (start, finalize from the stand-in compositor, is-active,
   stop) and compares units, activation environment and cleanup.

5. `daemon.sh pyuwsm` then `daemon.sh uwsm` runs the app daemon as a transient
   unit, sends it requests through the FIFOs and saves the replies.
6. For `app`, put a fake `systemd-run` that prints its arguments first in
   `PATH` (`cmp.sh` prepends `$SHIM` when set) so nothing is launched.
   `fixtures/systemd-run` is one.

`ci.sh /path/to/uwsm-source` does all of the above without supervision
and fails on any difference; CI runs it in an Arch container (the `parity`
job). It installs the desktop entries in `fixtures/` and the edge cases
from `make-edge-entries.sh`, runs every line of `cases.txt` through
`cmp.sh`, then the live session and the app daemon. Add a line to
`cases.txt` for each new behavior worth pinning.

`cmp.sh` masks the random suffix of unit names and sorts `err:` lines
(uwsm collects validation errors in a set, so their order varies).

Known noise: `pyuwsm` adds `PYTHONPATH` and `LC_CTYPE` to the exported
environment, PIDs differ, and argparse usage errors are worded differently
from clap's (both exit 2).
