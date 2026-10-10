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

Known noise: `pyuwsm` adds `PYTHONPATH` and `LC_CTYPE` to the exported
environment, PIDs differ, and argparse usage errors are worded differently
from clap's (both exit 2).
