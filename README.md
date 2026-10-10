# rwsm

A Rust rewrite of [uwsm](https://github.com/Vladimir-csp/uwsm), the Universal
Wayland Session Manager. It installs as `uwsm` and aims to be a drop-in
replacement: same commands and flags, same systemd unit names, same runtime
and config paths, same compositor plugin API.

**Status:** early. The full command line is in place, but subcommands are not
implemented yet. See [docs/uwsm-parity-spec.md](docs/uwsm-parity-spec.md) for
the behavior being matched and the parity checklist.

## What is rewritten and what is reused

Only uwsm's Python executable is rewritten. Its shell environment loader,
signal handler, compositor plugins and static systemd units are vendored
unchanged under [`data/`](data/README.md), so plugins keep working as is.

## Build and install

```sh
make
sudo make install            # PREFIX=/usr by default
```

`make install` puts the binary at `$PREFIX/bin/uwsm`, the helper scripts in
`$PREFIX/libexec/uwsm/`, plugins in `$PREFIX/share/uwsm/plugins/` and units in
`$PREFIX/lib/systemd/user/`. Install paths are compiled into the binary, so
build and install with the same `PREFIX`.

## License

MIT. Files under `data/` come from uwsm and are covered by its MIT license
(`data/LICENSE.uwsm`).
