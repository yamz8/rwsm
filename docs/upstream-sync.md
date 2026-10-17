# Following uwsm releases

rwsm matches one uwsm release at a time: `uwsm --version` prints it, `data/`
is vendored from it, and the parity CI job compares against it. To move to a
new release:

1. Get a full uwsm checkout at the new tag (not `--depth 1`, the script
   needs the old commit to diff against):

   ```sh
   git clone https://github.com/Vladimir-csp/uwsm /tmp/uwsm
   git -C /tmp/uwsm checkout vX.Y.Z
   ```

2. See what changed:

   ```sh
   tools/sync-upstream.sh /tmp/uwsm
   ```

   It lists vendored files that changed, appeared or disappeared upstream,
   and the `uwsm/` Python diff since the commit recorded in
   `data/README.md`. Read the full diff with
   `git -C /tmp/uwsm diff <recorded commit> HEAD -- uwsm/`.

3. Copy the vendored files with `tools/sync-upstream.sh --update /tmp/uwsm`.
   A new plugin, script or unit may also need a line in the `Makefile`
   (new extras mirror uwsm's meson options).

4. Port each Python change to the Rust module that holds the same function
   (see the module list in the spec, §13). Match messages word for word: the
   parity harness diffs output.

5. Update the version in every place it appears:
   - `src/config.rs` (`upstream_version!`)
   - `data/README.md` (tag and commit)
   - `packaging/arch/PKGBUILD` (`_upstream`)
   - `.github/workflows/ci.yml` (`UWSM_REF`)
   - `README.md` (status line)

   `config::tests::upstream_version_matches_everywhere` fails until the
   first four agree.

6. Record the change in `docs/uwsm-parity-spec.md`: the version note at the
   top, the affected section, and a new quirk in §11 if upstream introduced
   one worth deciding on.

7. If the Python uwsm runs on moved to a new Unicode version, regenerate
   the table `py_repr` uses with
   `python3 -I tools/gen-nonprintable.py > src/nonprintable.rs`.

8. Add `tests/parity/cases.txt` lines for new behavior, push, and let the
   parity job compare against the new release.

## Log

| Release | Upstream changes | rwsm changes |
|---|---|---|
| 0.27.0 | Baseline of the rewrite | |
| 0.27.1 | `uwsm stop` stops the compositor unit with job mode `replace` instead of `fail`, overridable with `UWSM_STOP_JOB_MODE`; three preset warnings reworded; man page and README | `cli::resolve_stop_job_mode`, warnings in `cli.rs`, re-vendored `data/man/uwsm.1.scd` and `data/README.uwsm.md` |
