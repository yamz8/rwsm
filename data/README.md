# Vendored uwsm files

These files are copied unchanged from
[uwsm](https://github.com/Vladimir-csp/uwsm) v0.27.0 (commit `f82dece`) and are
covered by its MIT license (see `LICENSE.uwsm`). rwsm replaces only uwsm's
Python executable; everything else is reused as is so plugins, units and docs
stay identical.

- `libexec/`: `prepare-env.sh` (environment loader, plugin API) and
  `signal-handler.sh` (start-time signal handling).
- `plugins/`: compositor plugins, installed to `$datadir/uwsm/plugins/`.
- `scripts/`: the optional extras `uwsm-app` (app daemon client, also
  installed as the `uwsm-terminal*` symlinks), `uuctl`, `wait-tray`, `fumon`
  and `ttyautolock`.
- `systemd/user/`: static units; `*.in` files get `@BIN_PATH@`, `@BIN_NAME@`
  and `@WAITPID_BIN@` substituted at install time (see `Makefile`).
- `systemd/user-preset/`: presets for the `fumon` and `ttyautolock` units.
- `man/`: scdoc sources of uwsm's man pages.
- `example-units/` and `README.uwsm.md`: uwsm's docs, installed to
  `$datadir/doc/uwsm/`.
