# rwsm

A Rust rewrite of [uwsm](https://github.com/Vladimir-csp/uwsm), the Universal
Wayland Session Manager. It installs as `uwsm` and aims to be a drop-in
replacement: same commands and flags, same systemd unit names, same runtime
and config paths, same compositor plugin API.

**Status:** every uwsm 0.27.0 command is implemented: `start`, `stop`,
`finalize`, `select`, `app`, `check` and the `aux` actions the units call,
including the app daemon. Behavior was compared against Python uwsm under a
real systemd user manager (see [tests/parity](tests/parity/README.md)),
including headless sway and labwc sessions. It has not yet been run on real
hardware (GPU, seats, VT switching). See
[docs/uwsm-parity-spec.md](docs/uwsm-parity-spec.md) for the behavior being
matched.

## What is rewritten and what is reused

Only uwsm's Python executable is rewritten. Its shell environment loader,
signal handler, compositor plugins, systemd units, helper scripts and man
pages are vendored unchanged under [`data/`](data/README.md), so plugins and
configs keep working as is. Startup no longer pays for a Python interpreter
(`uwsm check is-active` takes about 12 ms instead of 90 ms in the test
container), which matters most for `uwsm app` launches.

## Build and install

Needs Rust 1.88 or newer, and `scdoc` for the man pages. At runtime it uses
the same tools as uwsm: systemd, a POSIX `sh`, and optionally `whiptail` (for
`select`) and `waitpid`.

```sh
make
sudo make install                # PREFIX=/usr by default
sudo make install EXTRAS=all     # also uwsm-app, uuctl, wait-tray, fumon, ttyautolock
```

The installed files match a meson install of uwsm with the same options, file
for file, minus the Python modules. Install paths are compiled into the
binary, so build and install with the same `PREFIX`. The optional extras
mirror uwsm's meson options and can be picked one by one (`UWSM_APP=1`,
`UUCTL=1`, `WAIT_TRAY=1`, `FUMON=1`, `TTYAUTOLOCK=1`); see the `Makefile`.

## Switching from uwsm

Uninstall the uwsm package first (rwsm installs the same paths), then install
rwsm. Nothing in your setup needs to change: shell profile snippets, compositor
configs calling `uwsm finalize` or `uwsm app`, `~/.config/uwsm/` env files and
plugins, and unit drop-ins all keep working. Sessions that are already running
should be restarted once so the units pick up the new binary.

Differences you might notice, all listed in the spec's quirks table:
- Argument errors are worded by clap rather than argparse (exit code 2 in both).
- A few uwsm crashes are fixed instead of reproduced, for example `start -U home`
  without `~/.config/systemd`, `check may-start` hitting a D-Bus error, and
  the app daemon exiting on bad arguments.

## License

MIT. Files under `data/` come from uwsm and are covered by its MIT license
(`data/LICENSE.uwsm`).
