# Vendored uwsm files

These files are copied unchanged from
[uwsm](https://github.com/Vladimir-csp/uwsm) v0.27.0 (commit `f82dece`) and are
covered by its MIT license (see `LICENSE.uwsm`). rwsm replaces only uwsm's
Python executable; these shell helpers, compositor plugins and systemd units
are reused as is so plugins and unit behavior stay identical.

- `libexec/`: `prepare-env.sh` (environment loader, plugin API) and
  `signal-handler.sh` (start-time signal handling).
- `plugins/`: compositor plugins, installed to `$datadir/uwsm/plugins/`.
- `scripts/`: `uwsm-app` (app daemon client, also installed as the
  `uwsm-terminal*` symlinks), `uuctl` and `wait-tray`.
- `systemd/user/`: static units; `*.in` files get `@BIN_PATH@`, `@BIN_NAME@`
  and `@WAITPID_BIN@` substituted at install time (see `Makefile`).

The optional `fumon` and `ttyautolock` extras (and their units) are not
vendored yet.
