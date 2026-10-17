# uwsm parity spec

This is the behavioral spec rwsm has to match to be a drop-in replacement for
[uwsm](https://github.com/Vladimir-csp/uwsm). It was written from uwsm
**v0.27.0** (commit `f82dece`, 2026-09-15): `uwsm/main.py` (5271 lines),
`uwsm/misc.py`, `uwsm/dbus.py`, the `uwsm-libexec/` and `uwsm-plugins/` shell
scripts, `scripts/uwsm-app.sh`, the unit files under `systemd/user/`, and the
man pages. It has since been updated for **v0.27.1** (commit `0727985`),
which only changes `uwsm stop`'s job mode (§4.7) and the wording of three
preset warnings; see [upstream-sync.md](upstream-sync.md) for how releases
are brought in.

Each requirement has an ID (`P-…`) so later steps can track parity against it.
The parity checklist at the end lists every ID.

"Drop-in" means that an existing uwsm setup keeps working unchanged: the same
command line, unit names, runtime file paths, config file paths, environment
variables, plugin API and exit codes. Text output does not need to match byte
for byte, but it should carry the same information. Generated unit files
should match byte for byte, because uwsm only reloads systemd when a file
actually changes.

---

## 1. Shape of the program

uwsm is one Python executable (`uwsm`) plus a few support files:

| Piece | Kind | Installed to | rwsm plan |
|---|---|---|---|
| `uwsm` | Python CLI | `$bindir/uwsm` | Rewrite in Rust |
| `prepare-env.sh` | POSIX sh | `$libexecdir/uwsm/` | **Keep as is.** It defines the plugin API (§7) |
| `signal-handler.sh` | POSIX sh | `$libexecdir/uwsm/` | Keep, or fold into Rust (§4.1 step 12) |
| `plugins/*.sh` | POSIX sh | `$datadir/uwsm/plugins/` | Keep as is |
| static units, slices, targets | systemd units | `$prefix/lib/systemd/user/` | Keep. `*.in` templates get `@BIN_PATH@` etc. substituted |
| `uwsm-app` | POSIX sh client | `$bindir` (optional) | Keep. It only talks to the app daemon over FIFOs |
| `uuctl`, `fumon`, `ttyautolock`, `wait-tray` | sh helpers | optional | Out of scope. Ship unchanged if wanted |
| man pages | scdoc | | Port later |

Build-time constants (`uwsm/params.py.in`): `BIN_NAME=uwsm`, `BIN_PATH`,
`SH_BIN` (default `sh`), `WAITPID_BIN` (default `waitpid`), `WHIPTAIL_BIN`
(default `whiptail`), `PKG_LIBEXEC_DIR`, `PROJECT_VERSION`. rwsm needs the same
set, set at compile time (env vars read by `build.rs`, or a `cfg` module).

**P-NAME:** For drop-in use the installed binary must be called `uwsm`, or
`BIN_NAME` must still render as `uwsm` everywhere it shows up in paths:
`$XDG_RUNTIME_DIR/uwsm/`, `$XDG_CONFIG_HOME/uwsm/`, `uwsm/plugins/`,
`SyslogIdentifier=uwsm_*`, the `# injected by uwsm` header, and
`X-UWSMMark=`. The `uwsm-app-daemon-*` FIFO names and the
`app_daemon_error` flag path are hardcoded to `uwsm` no matter what
`BIN_NAME` is. (Open question: ship as `uwsm` or as `rwsm` with a `uwsm`
symlink? Self-detection in §4.2 compares `basename(argv0)` to `BIN_NAME`.)

## 2. Command line

Argument parsing is argparse. rwsm should use clap, with the same flags,
positional behavior and `--` handling. Errors exit with code 2, which is
argparse's convention.

Top level: `uwsm [-h] [-v|--version] {select,start,stop,finalize,app,check,aux} ...`.
`--version` prints `uwsm <version>`.

### 2.1 `select`
No options. See §4.6.

### 2.2 `start`
```
uwsm start [-h] [-D name[:name...]] [-a|-e] [-N Name] [-C Comment]
           [-U {run,home}] [-t|-T] [-F] [-g S|-G S] [-o] [-n] -- args...
```
| Flag | Dest | Default | Notes |
|---|---|---|---|
| `args` (1 or more) | `wm_cmdline` | required | The first arg is the ID: an executable, `entry.desktop[:action]`, `select` or `default` |
| `-D` | desktop_names | `""` | `:`-separated |
| `-a` / `-e` | desktop_names_exclusive | False | Mutually exclusive. `-a` is store_false |
| `-N` | wm_name | `""` | |
| `-C` | wm_comment | `""` | |
| `-U` | unit_rung | `$UWSM_UNIT_RUNG` or `run` | An invalid env value warns and falls back to `run` |
| `-t` / `-T` | tweaks | `$UWSM_TWEAKS` (bool) or True | Mutually exclusive |
| `-F` | hardcode | False | |
| `-g S` | gst_warn_seconds | 60 | Mutually exclusive with `-G` |
| `-G S` | gst_abort_seconds | -1 | |
| `-o` | only_generate | False | |
| `-n` | dry_run | False | |

### 2.3 `stop`
`uwsm stop [-r [marks]] [-U {run,home}] [-n]`.
`-r` takes an optional value. Absent means `False`, `-r` alone means
remove everything, and `-r a,b` gives comma-separated marks (a compositor ID,
`tweaks` or `generic`). **P-CLI-STOP-R:** these three states have to stay
distinct.

### 2.4 `finalize`
`uwsm finalize [VAR_NAME | VAR_NAME=value ...]`

### 2.5 `app`
```
uwsm app [-s {a,b,s,custom.slice}] [-t {scope,service}] [-a app_name]
         [-u unit_name] [-d description] [-p Prop=val]... [-S {out,err,both}]
         [-T [terminal-opts...]] -- args...
```
- `args` needs at least 1 item, or 0 items when `-T` is given.
- `-t` defaults to `$UWSM_APP_UNIT_TYPE` or `scope`. An invalid env value warns.
- **P-CLI-APP-T (terminal option prefilter):** if argv is `app ...` and `-T`
  shows up before `--`, then every arg after `-T` and before `--` that starts
  with `-` and is not one of `-s -t -a -u -d -S -T` is removed from argv
  before parsing. Ones starting with `--print-` go to `Terminal.print_opts`,
  which are collected but unused. The rest go to `Terminal.opts`, which knows
  `--app-id=X`, `--title=X`, `--dir=X` and `--hold` (§6.4). This mimics the
  xdg-terminal-exec option syntax.

### 2.6 `check`
- `check is-active [-v] [wm]`
- `check may-start [-v|-q] [-i] [-r] [-g S] [N ...]`. `N` defaults to `[1]`,
  and `0` disables the VT checks. `-g` defaults to 60.

### 2.7 `aux` (only valid when run by the systemd user manager)
- `aux prepare-env [-D …] [-a|-e] [-N …] [-C …] ID [args...]`
- `aux cleanup-env`
- `aux exec ID [args...]`
- `aux waitpid PID`
- `aux waitenv [VAR_NAME ...]`
- `aux app-daemon`

**P-AUX-GUARD:** every `aux` action first checks that `$MANAGERPID` is set,
non-zero and equal to `getppid()`. Otherwise it prints
`Aux actions can only be run by systemd user manager!` and exits 1.

## 3. Environment variables read

| Var | Used by | Meaning |
|---|---|---|
| `UWSM_UNIT_RUNG` | start, stop | `run` or `home` |
| `UWSM_TWEAKS` | start | Bool (§3.1) |
| `UWSM_FINALIZE_VARNAMES` | finalize | Extra var names, whitespace-separated (plugins add to it) |
| `UWSM_WAIT_VARNAMES` | aux exec, aux waitenv | Extra vars to wait for |
| `UWSM_WAIT_VARNAMES_TIMEOUT` | start (drop-ins), waitenv | Int ≥ 1, **code default 30** (the man page wrongly says 10) |
| `UWSM_WAIT_VARNAMES_SETTLETIME` | aux exec, waitenv | Float, default 0.2. Invalid values warn and fall back to 0.2 |
| `UWSM_{ALWAYS,NEVER}_{EXPORT,CLEANUP}_VARNAMES` | env handling | Set modifiers (§5.2) |
| `UWSM_APP_UNIT_TYPE` | app | `scope` or `service` |
| `UWSM_STOP_JOB_MODE` | stop | `fail`, `replace` (default) or `replace-irreversibly` (v0.27.1, §4.7) |
| `UWSM_SILENT_START` | start | Bool or int. `1` silences stdout, `2` also silences warnings |
| `UWSM_DEBUG`, falling back to `DEBUG` | all | Bool or int. Debug output goes to stderr. An invalid value warns once |
| `XDG_*` base dirs | all | pyxdg semantics (§3.2) |
| `XDG_CURRENT_DESKTOP` | start, app, terminal lookup | |
| `DESKTOP_ENTRY_*` | app | Set by launchers (§6.2) |
| `MANAGERPID` | aux | Guard |
| `DBUS_SESSION_BUS_ADDRESS` | check | |

Set by uwsm: `IN_UWSM_ENV_PRELOADER=true` (in the preloader shell),
`UWSM_SH_NO_STDOUT` and `UWSM_SH_NO_WARN` (for signal-handler.sh).

### 3.1 `str2bool_plus`
- A numeric string becomes an int (in numeric mode) or `n > 0`.
- `""`, `no`, `false` and `n` (case-insensitive) mean false or 0.
- `yes`, `true` and `y` mean true or 1.
- Anything else is a ValueError.

### 3.2 XDG base dirs (pyxdg semantics)
- `load_config_paths(x)` yields `$XDG_CONFIG_HOME/x` then each
  `$XDG_CONFIG_DIRS/x` that **exists**.
- `load_data_paths(x)` does the same with the data dirs. Defaults:
  `~/.config`, `/etc/xdg`, `~/.local/share`, `/usr/local/share:/usr/share`.
- `get_runtime_dir(strict=True)` requires `XDG_RUNTIME_DIR`.
- `which()` searches `PATH`.

The `xdg` crates for Rust don't match exactly. Implement a small module that
copies pyxdg's ordering and existence filtering.

## 4. Session lifecycle

### 4.1 `start`
In order:

1. Logging: `LogFlag.log = !dry_run && !only_generate`, which also sends
   messages to syslog. `UWSM_SILENT_START` sets the nostdout and nowarn flags.
2. **graphical.target gate (P-START-GST).** Skipped for `-n` and `-o`.
   timeout is `-G` if it is ≥ 0, otherwise `-g`, and the gate runs when either
   is ≥ 0. Call `wait_for_unit("graphical.target", system bus,
   states={active,activating}, timeout)` (§8.3). If it fails: with `-G`,
   print an error and exit 1. With `-g`, and when warnings aren't silenced,
   print the multi-line warning and sleep 5 s. A D-Bus failure exits 1.
3. If `args[0]` is `select` or `default`: run selection (§4.6). `default`
   uses `just_confirm`, which returns the saved default without a menu when
   it is valid. Save it if it changed, and replace `wm_cmdline` with
   `[id]`. Nothing selected exits 1.
4. `fill_comp_globals()` (§4.2).
5. Print a summary: ID, command line (shell-joined), plugin/binary ID,
   initial desktop names, Name, Description.
6. **P-START-ACTIVE:** if `is_active(verbose_active=True)` (§8.4), print an
   error and exit 1, except in dry run, which continues.
7. `generate_dropins(rung)` (§4.3).
8. If tweaks are on, `generate_tweaks(rung)`. Otherwise
   `remove_units(["tweaks"], rung)`.
9. `remove_units(other_rung)` removes **all** marked files from the other rung.
10. If any unit file changed, reload systemd (§8.2). Otherwise print
    `Units unchanged.`
11. `-o` exits 0 with a warning. `-n` exits 0 with a warning.
12. Real start:
    1. `systemctl --user start wayland-session-bindpid@<own PID>.service`,
       which must succeed.
    2. `save_env("env_login")`: the whole process environment, filtered
       (§5.1), as NUL-joined `K=V` with no trailing newline, written to
       `$XDG_RUNTIME_DIR/uwsm/env_login`.
    3. `save_env("env_session.conf", {the session_specific vars that are
       set}, sep="\n")`, newline-joined plus a trailing `\n`.
    4. `dup2(1,3)` and `dup2(2,4)`. Set `UWSM_SH_NO_STDOUT` and
       `UWSM_SH_NO_WARN` to `"1"` or `""`.
    5. `exec systemd-cat --identifier=uwsm --stderr-priority=err -- <sh>
       $libexec/signal-handler.sh wayland-session-envelope@<id_unit_string>.target`

**signal-handler.sh** forks `systemctl --user start --wait <UNIT>` with TERM,
HUP and INT ignored. On TERM, HUP or INT it runs `systemctl --user stop
<UNIT> &`, then waits for the start PID and exits with its code. Its
messages also go to fd 3 and 4, which still point at the terminal. Under
kmscon (`TERM_SESSION_TYPE=kms`) it writes `\033]setBackground\a` and
`\033]setForeground\a` around the session, with a tmux passthrough variant.
**P-START-SIGHANDLER:** behavior must be identical. The simplest way to get
that is to keep exec'ing the same script.

Starting the envelope target pulls in everything else through the unit
graph (§9).

### 4.2 `fill_comp_globals()`
This works out the compositor identity. It runs in `start`, `aux
prepare-env` and `aux exec`.

**Main argument classification (`MainArg`, P-MAINARG):**
- If it ends with `.desktop` or contains `.desktop:`, it is a desktop entry.
  Split off the action after `.desktop:` (validated against
  `^[a-zA-Z0-9-]+$`).
  - If the entry ID contains `/`, it is a path: normalize it and expand `~`.
    The ID is the basename, unless the path sits under an `applications`
    data dir, in which case the ID is the relative path with `/` replaced by
    `-`.
  - The ID is validated against `^[a-zA-Z0-9_][a-zA-Z0-9_.-]*.desktop$`
    (the `.` before `desktop` is unescaped in upstream, so it matches any
    character). If the ID is invalid and a file with that name exists in
    the current directory, it becomes path `./ID`. If it is invalid and not
    a path, raise. Either way, print a warning when invalid.
- Otherwise it is an executable. Arguments containing `/` also get a
  normalized path.
- `check_exec`: `which(executable)` must resolve.
- `check_path`: the file must exist and be readable, and executable if it is
  an executable.

**ID:**
- In `start`, `id = basename(args[0])`. If `args[0]` is a path, hardcode
  mode is forced.
- In `aux`, `id = wm_id`, which must not be a path. If a raw cmdline with a
  non-empty `[0]` is given, it replaces main_arg and becomes the cmdline.
- The ID must match `^[a-zA-Z0-9_:.-]+$`.
- `id_unit_string = simple_systemd_escape(id, start=False)` (§8.1).
- `aux exec` with a cmdline already present stops here.

**Desktop entry branch:**
1. Load the entry, either from a path (parse plus `check_entry_basic`) or by
   searching `wayland-sessions` with `find_entries` (§6.1). Not found raises
   FileNotFoundError.
2. `entry_action_keys(entry, action)` gives Name, Exec (tokenized) and Icon.
   If the cmdline is empty, it becomes Exec. `bin_name =
   basename(cmdline[0])`.
3. **P-SELF-ENTRY (an entry that runs uwsm itself):** if `bin_name ==
   BIN_NAME`, then `cmdline[1]` must be `start`. Re-parse
   `cmdline[1:]` as full uwsm args. Reject if:
   - it points to itself (same entry and action),
   - it uses `-n` or `-o`,
   - its `-D` is malformed (`^[a-zA-Z0-9_.-]+(:[a-zA-Z0-9_.-]+)*$`).

   If its main arg is another entry, load it from `wayland-sessions`. It is
   an error if that entry's Exec also starts with uwsm. Then cmdline =
   inner Exec + nested args[1:]. Any error here prints and exits 1.
4. `cmdline += args[1:]`, the extra CLI args. `%` field codes are **not**
   expanded here.
5. Only in `start` and `prepare-env`:
   - Validate `-D` with the regex above, and exit 1 if it fails.
   - **Desktop names (P-DESKTOP-NAMES):**
     - `-e`: requires `-D` (exit 1 without it), and the names are exactly
       `-D`.
     - Else if the nested uwsm in the entry had `-e`: its `-D` (required).
     - Else concatenate, in order: `XDG_CURRENT_DESKTOP` (from the process
       env in `start`, or from saved `env_login` in `aux`), the entry's
       `DesktopNames`, the nested `-D`, then the CLI `-D`. If all are empty,
       use `[bin_name]`.
   - **Name:** CLI `-N`, else the nested `-N`, else
     `"<Name> - <GenericName>"` (non-empty parts joined).
   - **Description:** CLI `-C`, else the nested `-C`, else the entry Comment.
   - In `start`, a nested `-F` is inherited.
6. Re-classify `cmdline[0]` and check exec or path.

**Executable branch:**
- Check exec or path. cmdline is `wm_cmdline`. In aux, an empty cmdline
  becomes `[executable]`, and an empty `[0]` is filled with the executable.
- Desktop names: with `-e`, exactly `-D`. Otherwise `XDG_CURRENT_DESKTOP`
  (env or env_login) + `-D`, falling back to `[bin_name]`.
- Name and description come straight from `-N` and `-C`.

**Then:**
- After all that, main_arg must not still be an entry.
- In `start`, with hardcode or a path, set `cmdline[0]` to the absolute path
  (of the path, or of `which(executable)`).
- `cli_args = wm_cmdline[1:]`.
- In `start` and `prepare-env`, record `cli_desktop_names` and the exclusive
  flag, and **dedupe both name lists while keeping order**.
- `bin_id = lower(re.sub(r"(^[^a-zA-Z]|[^a-zA-Z0-9_])+", "_", bin_name))`.

### 4.3 Generated units (`generate_dropins`)
Files go under `<rung_dir>/systemd/user/`, where rung `run` is
`$XDG_RUNTIME_DIR` and `home` is `$XDG_CONFIG_HOME`. Each file starts with
`# injected by uwsm, do not edit\n[Unit]\nX-UWSMMark=<mark>\n`.

1. **Timeout drop-ins (mark `generic`):** if `get_waitenv_timeout() != 30`,
   write `wayland-wm@.service.d/50_timeout.conf` and
   `wayland-session-waitenv.service.d/50_timeout.conf` with
   `[Service]\nTimeoutStartSec=<t>\n`. Otherwise remove both.
2. **Per-compositor drop-ins (mark `<id>`):**
   `wayland-wm-env@<id_unit_string>.service.d/50_custom.conf` and
   `wayland-wm@<id_unit_string>.service.d/50_custom.conf`.
   - If Name or Description is set, add `Description=Environment preloader
     for <X>` and `Description=Main service for <X>`, where X is
     `(name or bin_name)` and description joined with `, `.
   - **Preloader ExecStart override:**
     - `-e` adds `-eD <cli names>`.
     - Else if `desktop_names != [bin_name]`, add `-D <names>`.
     - If anything was added, or `cmdline[0]` is absolute, append
       `-- %I`, then also append `cmdline[0]` when it is absolute.
     - Emit `[Service]\nExecStart=\nExecStart=<shlex.join([BIN_PATH, aux,
       prepare-env] + extra)>\n`.
   - **Service ExecStart override:** if `cmdline[0]` is absolute, use the
     whole cmdline. Else if there are cli_args, use `[""] + cli_args`.
     Emit `[Service]\nExecStart=\nExecStart=<shlex.join([BIN_PATH, aux,
     exec, --, %I] + that)>\n`.
   - If a file would only hold the header, remove it instead.

**P-UNIT-BYTES:** exact rendering, for example:
```
# injected by uwsm, do not edit
[Unit]
X-UWSMMark=sway.desktop
Description=Main service for Sway, An i3-compatible Wayland compositor
[Service]
ExecStart=
ExecStart=/usr/bin/uwsm aux exec -- %I '' --unsupported-gpu
```
`shlex.join` quoting has to be reproduced exactly: `''` for an empty string,
and `'…'` with `'"'"'` for embedded single quotes. Bare words match
`[\w@%+=:,./-]` (ASCII).

### 4.4 Tweaks (`generate_tweaks`, mark `tweaks`)
- `app-@autostart.service.d/slice-tweak.conf`: `PartOf=` and
  `After=xdg-desktop-autostart.target`, `[Service] Slice=app-graphical.slice`,
  `EnvironmentFile=-%t/uwsm/env_session.conf`.
- `app-flatpak-.scope.d/order-tweak.conf`: `PartOf=` and
  `After=graphical-session.target`, `[Scope] Slice=app-graphical.slice`.
- `plasma-xdg-desktop-portal-kde.service.d/order-tweak.conf`:
  `After=graphical-session.target`.

Copy the exact text, comments included, from `main.py:1572-1621`.

### 4.5 Unit file helpers
- **update_unit:** validate the name with the unit-extension regex
  (`…@?.(service|slice|scope|target|socket|d/….conf)$`). Relative paths may
  have at most one subdir. Create missing dirs. Write only if the content
  differs, and mark `UnitsState.changed`. Dry run prints "Will create" or
  "Will update" instead.
- **remove_unit:** delete the file. Remove its subdir if it is now empty,
  otherwise warn that it is not empty.
- **remove_units(only=None):** walk the rung dir in sorted order. A file is
  matched by a line starting with `X-UWSMMark=` (no filter), or by a line
  equal to `X-UWSMMark=<m>` for one of the given marks (stripped).
- **reload_systemd:** call D-Bus `Manager.Reload()` and poll `ListJobs`
  every 100 ms until the job is gone. Dry run only prints.

### 4.6 `select`
`get_default_comp_entry()` reads the first non-blank line of the first
existing `uwsm/default-id` in config paths, then in data paths.
`select_comp_entry(default, just_confirm)`:
- Collect valid `wayland-sessions` entries (`entry_parser_session` drops
  any that fail parsing or basic checks), sorted by ID without `.desktop`.
- Each entry becomes a choice: ID, `"Name GenericName"`, and Comment
  shortened to the terminal width (`textwrap.shorten`, with an overhead of
  10, falling back to 128 columns).
- Each valid action becomes a choice `ID:action` labeled ` └▶ <action
  name>` (it needs Exec, Name and a resolvable binary).
- A default that is not among the choices is dropped with a warning. With
  just_confirm and a valid default, return it.
- whiptail must exist, and stdin and stdout must both be TTYs. Otherwise
  raise.
- `whiptail --clear --backtitle "Universal Wayland Session Manager" --title
  "Choose compositor" [--default-item D] --menu "" 0 0 0 --notags <pairs>`
  runs with `NEWT_MONO=true`, and the choice is read from stderr.
- `save_default_comp_entry` writes `$XDG_CONFIG_HOME/uwsm/default-id` with
  a trailing newline.

`uwsm select` exits 0 when something is selected (printing "unchanged" if it
equals the old value) and 1 when cancelled.

### 4.7 `stop`
`LogFlag.log = !dry_run`. Find active or activating `wayland-wm@*.service`.
- None: print "Compositor is not running."
- More than one: warn and use the first.
- Dry run: print only.
- Otherwise `StopUnit(unit, mode)` and poll `ListJobs` until the job is
  gone. `mode` is `UWSM_STOP_JOB_MODE` if it is `fail`, `replace` or
  `replace-irreversibly`, else `replace` with the warning `Expected
  fail|replace|replace-irreversibly from UWSM_STOP_JOB_MODE, got "X",
  ignored and set to "replace".` (v0.27.0 always used `fail`).

Then handle `-r` (§2.3) on the selected rung and reload if anything changed.
A reload failure is only a warning. The exit code is 1 only if stopping
raised.

### 4.8 `finalize`
- `WAYLAND_DISPLAY` must be non-empty, else error.
- Vars: `WAYLAND_DISPLAY`, `DISPLAY`, then
  `sorted(set(args + $UWSM_FINALIZE_VARNAMES.split()))`.
  - `K=V` entries are taken literally, but `WAYLAND_DISPLAY` and `DISPLAY`
    can't be overridden this way (warn).
  - Others take their value from the env, and unset ones are skipped.
  - Names are filtered (§5.1), duplicates dropped, first wins.
- `wm_id`: active or activating compositor. `activating_id`: activating only.
  - No wm_id: error and exit 1.
  - activating_id ≠ wm_id: conflict error and exit 1.
- `append_to_cleanup_file(names, create=False)`. A missing file is an error
  ("Assuming env preloader failed.") and exits 1.
- `set_systemd_vars(vars)` (§5.4).
- If activating: `exec systemd-notify READY=1 NOTIFYACCESS=exec`.
- Else if the unit's `NotifyAccess == "all"`:
  `exec systemd-notify NOTIFYACCESS=exec`.
- Else exit 0.
- Exceptions print an error **with a desktop notification** (notify=1).

> Upstream quirk: `extract_wm_id` splits the unit name, then throws the result
> away and returns the whole unit name (`wayland-wm@….service`), decoded with
> `unicode_escape`. The comparisons still work because both sides go through
> the same function. rwsm should extract and unescape the instance properly,
> which gives the same comparison result and better messages.

### 4.9 `aux prepare-env`
`fill_comp_globals()`, then `prepare_env()` (§5.3). On an exception, print
it, try `cleanup_env()`, and exit 1.

### 4.10 `aux cleanup-env`
If any `wayland-wm@*.service` is active or activating, error ("A compositor
is running…") and exit 1. Otherwise `cleanup_env()` (§5.5).

### 4.11 `aux exec`
1. `fill_comp_globals()` and print `Starting: <cmdline>...`.
2. `env_pre` is the filtered systemd activation env.
3. **Autoready double fork (P-AUTOREADY).** The intermediate child exits at
   once, and the parent `waitpid`s it. The leaf process:
   - `start_timeout` is `wayland-wm@<id>.service` `TimeoutStartUSec // 1e6`
     (Service interface), or None, which means the default.
   - `waitenv(["WAYLAND_DISPLAY"] + $UWSM_WAIT_VARNAMES, start_timeout)`
     (§8.5), then sleep for the settle time.
   - `env_delta` is post minus pre, compared as (name, value) items. If it is
     not empty, `set_systemd_vars(delta, dbus_only=True)` and
     `append_to_cleanup_file(delta, skip_always_cleanup=True, create=True)`.
     On FileNotFoundError: `kill(main, SIGTERM)` and exit 1.
   - If the compositor is still activating: `exec systemd-notify READY=1
     NOTIFYACCESS=exec`. Else the same NotifyAccess branch as finalize.
   - Any exception: warn "Autoready failed:" and exit 1.
4. The parent `exec`s `cmdline` (execlp, so through PATH).

In Rust, fork in a multithreaded process is unsafe. Do the fork before
starting any threads or async runtime, or spawn a detached helper process
(`uwsm aux autoready …`, a private subcommand) instead.

### 4.12 `aux waitpid PID`
`pidfd_open(pid)` then `poll`. Exit 0 when the process ends, or right away if
it doesn't exist. This is only used when the `waitpid` binary is missing (see
the bindpid unit).

### 4.13 `aux waitenv [VARS]`
Wait for `WAYLAND_DISPLAY + args + $UWSM_WAIT_VARNAMES` with a timeout taken
from `wayland-session-waitenv.service` `TimeoutStartUSec`, then sleep for the
settle time.

> Upstream bug: this branch references an undefined `bus_session`, so the
> property lookup always throws and the timeout always falls back to
> `get_waitenv_timeout()`. That has the same practical effect, because the
> drop-in sets both to the same value. rwsm should read the property
> properly.

## 5. Environment management

### 5.1 Var name filtering (P-ENV-FILTER)
- Valid names match `^([a-zA-Z_][a-zA-Z0-9_]+|[a-zA-Z][a-zA-Z0-9_]*)$`.
  A lone `_` is invalid.
- `_`, `SHELL`, `PWD` and `OLDWD` are dropped silently, others are dropped
  with an `Encountered illegal var` warning. (`OLDWD` is a typo upstream,
  `OLDPWD` isn't dropped. Keep the same behavior.)

### 5.2 Var name sets (`Varnames`)
```
session_specific = {XDG_SEAT, XDG_SEAT_PATH, XDG_SESSION_ID, XDG_SESSION_PATH, XDG_VTNR}
always_unset   = {DISPLAY, WAYLAND_DISPLAY} ∪ session_specific
always_export  = {PATH, XDG_CURRENT_DESKTOP, XDG_MENU_PREFIX, XDG_SESSION_DESKTOP,
                  XDG_SESSION_CLASS, XDG_SESSION_TYPE}
never_export   = {PWD, LS_COLORS, INVOCATION_ID, SHLVL, SHELL, TERM, COLORTERM,
                  TERM_SESSION_TYPE, NOTIFY_SOCKET} ∪ session_specific
always_cleanup = {DISPLAY, LANG, PATH, WAYLAND_DISPLAY, XCURSOR_SIZE, XCURSOR_THEME,
                  XDG_CURRENT_DESKTOP, XDG_MENU_PREFIX, XDG_SESSION_DESKTOP,
                  XDG_SESSION_CLASS, XDG_SESSION_TYPE, NOTIFY_SOCKET} ∪ session_specific
never_cleanup  = {SSH_AGENT_LAUNCHER, SSH_AUTH_SOCK, SSH_AGENT_PID}
```
`update_varnames_sets(env)`: for each set, read
`UWSM_<SET>_VARNAMES.split()`. Bare and `+name` entries are added, `-name`
entries removed (all filtered), and each change is printed. It is called with
the post-shell env in prepare-env and with `os.environ` elsewhere.

### 5.3 `prepare_env()`
1. Print `Preparing environment for <name or bin_name>...`
2. Load `env_login` and **delete it**. `have_env_login` records whether it
   was non-empty.
3. **Session deduction (P-ENV-SESSION):** if `XDG_SEAT` or `XDG_SESSION_ID`
   is missing or empty in env_login:
   - The VT comes from env_login `XDG_VTNR` if it is numeric, else from
     `/sys/class/tty/tty0/active` (`ttyN`). Error if neither works.
   - `get_session_by_vt(vt)`: logind `ListSessionsEx`, matching a session of
     class `user`, with `user_name == getlogin()` and session `VTNr == vt`.
     It gives session_id and seat_id. Error if there is no match.
   - If no `wayland-session-bindpid@*.service` is active or activating, warn
     and `systemctl --user start wayland-session-bindpid@<session
     Leader>.service`.
   - Re-save `env_session.conf` from the amended env_login.
4. `env_pre` is the filtered systemd env. Save it to `env_pre`
   (NUL-separated). `env_merged = env_pre | env_login`, with login winning.
5. Write an aux vars file `$XDG_RUNTIME_DIR/uwsm/vars_<16 hex mark>`:
   ```
   # vars for plugins
   __SELF_NAME__=… __WM_ID__=… __WM_ID_UNIT_STRING__=… __WM_BIN_ID__=…
   __WM_DESKTOP_NAMES__=… __WM_FIRST_DESKTOP_NAME__=…
   __WM_DESKTOP_NAMES_EXCLUSIVE__=true|false __LOAD_PROFILE__=true|false
   __OIFS__=" \t\n"   (a literal backslash-t and backslash-n inside double quotes)
   # random mark for environment dump at the end
   __RANDOM_MARK__=<mark>
   # context marker for profile scripting
   IN_UWSM_ENV_PRELOADER=true
   ```
   Values are `shlex.quote`d, one per line. Take the exact layout from
   `main.py:2769-2784`.

   Note that the plugin man page also documents
   `__WM_DESKTOP_NAMES_LOWERCASE__` and
   `__WM_FIRST_DESKTOP_NAME_LOWERCASE__`, but v0.27.0 does **not** set
   them. Match the code, not the man page.
6. Plugins are `load_data_paths("uwsm/plugins/<bin_id>.sh")`, all existing
   matches in priority order.
7. Run `<sh> [-x if debug] $libexec/prepare-env.sh <auxfile> <plugins...>`
   with env = `env_merged` (**not** the process env), capturing output.
8. Find the mark in stdout.
   - Missing: print stdout and stderr, and raise.
   - Text before the mark is printed as messages. Stderr is printed as an
     error.
   - A non-zero exit raises.
   - The rest is `env -0` output. Strip trailing NULs, split on NUL, and
     drop `_` and entries without `=` (with an error). Then filter.
9. `update_varnames_sets(env_post)`.
10. `set_env = env_post − env_pre`, as items. Then:
    - Add `always_export − never_export − always_unset` when present in
      env_post.
    - Remove `never_export ∪ always_unset`.
11. `unset = ((pre.keys − post.keys) ∪ always_unset) ∩ systemd_varnames`.
12. `cleanup = set_env.keys − never_cleanup`. Call
    `append_to_cleanup_file(cleanup, create=True)`.
13. `set_systemd_vars(set_env)`. If `unset` is non-empty,
    `unset_systemd_vars(unset)`.

**prepare-env.sh behavior** (kept as is, listed for reference):
- Source the aux file and delete it. Source each plugin.
- If `__LOAD_PROFILE__=true`, source `/etc/profile` and `~/.profile`, then
  export PATH.
- Default every XDG base dir.
- Set `XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP`,
  `XDG_MENU_PREFIX=<lower first>-`, `XDG_SESSION_TYPE=wayland` and
  `XDG_BACKEND=wayland`.
- Run `quirks_<bin_id>`, then `load_wm_env[_<bin_id>]`.
- Run `xdg-user-dirs-update` unless its service is enabled.
- Print the mark, then `exec env -0`.

### 5.4 Activation environment export
- **set_systemd_vars(vars, dbus_only=False, verbose=True):** print the list,
  tagging each name `(already set)` (only when verbose) or `(updating)`. When
  not verbose and nothing changed, print `(All already set)`. Then:
  - Unless dbus_only, call `Manager.SetEnvironment(["K=V"...])`.
  - Then, **if `dbus.service` Id is not `dbus-broker.service`**, also call
    `org.freedesktop.DBus.UpdateActivationEnvironment(dict)` and print a
    hint to switch to dbus-broker.
- **unset_systemd_vars(names):** with a non-broker dbus, first send the
  names with empty values to `UpdateActivationEnvironment`. Then call
  `Manager.UnsetEnvironment(names)`.

### 5.5 `cleanup_env()`
- `update_varnames_sets()`.
- Read `env_cleanup.list` (warn if missing). `env_pre` comes from the
  `env_pre` file, if there is one.
- `cleanup = ((list ∪ always_cleanup) − never_cleanup) ∩ systemd_names −
  env_pre.keys`. Unset those.
- If env_pre is non-empty, `set_systemd_vars(env_pre, verbose=False)`.
- Delete `env_cleanup.list`, `env_pre` and `env_session.conf` if present.

### 5.6 `append_to_cleanup_file(names, skip_always_cleanup, create)`
- `update_varnames_sets()`. Names = filter(names) − never_cleanup, minus
  always_cleanup when skip is set. Return if empty.
- The file is `$XDG_RUNTIME_DIR/uwsm/env_cleanup.list`.
  - Missing: with create, make the dir and write the sorted names (one per
    line, trailing newline). Without create, raise FileNotFoundError.
  - Exists: append only new names, sorted.
- Print `Marking variables for later cleanup…` with the new names.

### 5.7 Runtime files (P-RUNTIME-FILES)
All live under `$XDG_RUNTIME_DIR/uwsm/`:

| File | Format | Writer | Reader |
|---|---|---|---|
| `env_login` | NUL-joined `K=V` | start | prepare-env, which deletes it. `fill_comp_globals` in aux reads XDG_CURRENT_DESKTOP from it |
| `env_session.conf` | `K=V\n` | start, prepare-env | units' `EnvironmentFile=`, cleanup deletes it |
| `env_pre` | NUL-joined | prepare-env | cleanup, which deletes it |
| `env_cleanup.list` | one name per line | prepare-env, finalize, autoready | cleanup, which deletes it |
| `vars_<mark>` | sh | prepare-env | prepare-env.sh, which deletes it |
| `app_daemon_error` | NUL args | app daemon | app daemon |

`load_env` parse errors only warn. Values are not escaped. Files are created
with `os.makedirs(exist_ok=True)` and default permissions.

FIFOs live directly in `$XDG_RUNTIME_DIR/`: `uwsm-app-daemon-in` and
`uwsm-app-daemon-out`. Lock: `uwsm-app.lock` (client side).

Cache: `$XDG_CACHE_HOME/uwsm-not-terminals`, one `path;mtime` per line.

## 6. `app`: launching apps as units

### 6.1 Desktop entry search and validation
**find_entries(subpath, parser, reject_pmt, reject_ids):**
- For each `load_data_paths(subpath)` dir, `os.walk(followlinks=True)` and
  consider `*.desktop` files.
  - Skip if `reject_pmt[path] == mtime`.
  - `id = relpath` with `/` replaced by `-`. Skip if it is in reject_ids,
    fails the ID regex, or has already been seen (dedup by ID, first data
    dir wins).
  - The parser returns `append`, `extend`, `return` (stop and return
    `[data]`) or drop.
- **Ordering caveat:** os.walk order inside a directory follows the
  filesystem. Sort it in rwsm for determinism; for the parity tests, dedup
  by ID is what matters.

**check_entry_basic(entry, action):** this needs a desktop-entry parser
equivalent to pyxdg's `DesktopEntry` plus `validate()`.
- Ignore validation errors that are: "is not a registered", `Invalid key:
  ExecArg|DesktopNames|DBusActivatable|SingleMainWindow|PrefersNonDefaultGPU|TargetEnvironment`,
  `Missing main category`, or invalid locale keys. Any other error raises.
- Raise if the entry is Hidden, its TryExec fails, the action or its group
  is missing, the action has no Name or Exec, Exec is missing, or
  `which(Exec[0])` fails.
- **P-ENTRY-VALIDATE:** reproduce the important pyxdg validation errors.
  Exact parity with pyxdg's whole validator isn't needed; at minimum,
  reject malformed files, missing `Type`/`Name`, and an invalid `Type`.
  Record any differences as a known gap.

**check_entry_showin:** applies OnlyShowIn and NotShowIn against
`XDG_CURRENT_DESKTOP`.

**Value handling (P-ENTRY-EXEC):**
- `entry_expand_str` handles `\s \n \t \r \\`. Unknown escapes keep only
  the character.
- `entry_tokenize_exec`: double quotes group, `\` escapes inside quotes, and
  runs of whitespace separate. It raises on an unescaped `` ` `` or `$`
  inside quotes, and on an unquoted ``\t \n ' \ > < ~ | & ; $ * ? # ( ) ` ``.
  A trailing empty arg is kept, which matches upstream: `"a "` gives `["a",
  ""]`. Note that the value is `.strip()`ped first.
- Add unit tests built from these rules.

### 6.2 `app()` flow (P-APP)
1. Every `-p` must contain `=`.
2. `MainArg(cmdline[0])`, which may be None when the cmdline is empty with
   `-T`. A path is checked.
3. **Entry:**
   - Load it from a path, or search `applications` with
     `entry_parser_by_ids`. Not found is a FileNotFoundError.
   - Add `SourcePath=<abs entry path>`.
   - `Terminal=true` forces terminal mode.
   - `Path=` becomes the working dir, which must be a directory.
   - app_name defaults to the ID without its extension.
   - The description defaults to `Name`, plus ` - GenericName` when
     GenericName is set and different.
   - Render with `gen_entry_args` (§6.3). For iterated results:
     - Drop `-u` with a warning.
     - Recursively call `app(fork=True)` per instance (or
       `return_cmdline`).
     - Poll the children every 100 ms and log each systemd-run exit (an
       error with a notification when non-zero).
     - Exit 1 if any failed, else 0.
4. **Non-entry:**
   - The app name comes from `DESKTOP_ENTRY_ID` (without `.desktop`), and
     SourcePath from `DESKTOP_ENTRY_PATH`.
   - The description is the first of `DESKTOP_ENTRY_NAME_L` or
     `DESKTOP_ENTRY_NAME`, joined with ` - ` to the first of
     `DESKTOP_ENTRY_GENERICNAME_L` or `DESKTOP_ENTRY_GENERICNAME`.
5. Terminal handling (§6.4).
6. The description falls back to `app_name` or `basename(cmd[0])`, and with
   no cmdline, `App launched by uwsm`.
   - Precedence quirk: upstream's expression is `(app_name or
     basename(cmdline[0])) if cmdline else "App launched by uwsm"`.
7. If there is a cmdline, `which(cmdline[0])` must succeed, else `Command not
   found`.
8. Slice: `a` is app-graphical, `b` background-graphical, `s`
   session-graphical, and anything ending in `.slice` is taken as is.
   Anything else is an error.
9. **Unit name (P-APP-UNITNAME):**
   - Default: `app-<desk>-<cmd>-<8hex>.scope` or
     `app-<desk>-<cmd>@<8hex>.service`.
     - `desk` is the escaped first `XDG_CURRENT_DESKTOP` item (default
       `uwsm`).
     - `cmd` is the escaped app_name or basename.
     - Escaping is `simple_systemd_escape(start=False)`.
     - Length limits: if `len("app---DEADBEEF.")+len(type)+len(desk) > 127`,
       trim desk to keep the total under 127, never splitting a `\xNN`.
       Then trim cmd so the whole name is under 255 the same way.
   - `-u`: must end with `.<type>` and be at most 255 characters.
10. **systemd-run argv:**
   ```
   systemd-run --user
     scope:   --scope
     service: --property=Type=exec --property=ExitType=cgroup
              --setenv=K=V for each set session_specific var (sorted)
              -S out:  --property=StandardOutput=null, plus
                       --property=StandardError=<DefaultStandardOutput> when the
                       manager's DefaultStandardError is "inherit"
              -S err:  --property=StandardError=null
              -S both: both =null
     --property=<p> for each -p (then SourcePath)
     --slice=<slice> --unit=<name> --description=<desc> --quiet --collect
     --working-directory=<dir> | --same-dir
     -- <terminal cmdline + execarg> <cmdline>
   ```
11. Unset all `DESKTOP_ENTRY_*` vars (the full list is at
    `main.py:3787-3799`).
12. With fork, use Popen. Otherwise, a scope with `-S` redirects fd 1 and/or
    2 to /dev/null, and then **exec**s systemd-run.

### 6.3 `gen_entry_args(entry, args, action)` field codes (P-FIELDCODES)
Go through the Exec args after `[0]`:
- More than one `%x` field in an arg raises.
- `%%` becomes `%`. (Upstream checks this first and only replaces `%%`, so
  `%%f` turns into `%f`, which is then **not** re-expanded.)
- `%[DdNnvm]` args are dropped.
- `%f`, `%u` (inside an arg):
  - 0 args: drop the arg.
  - 1 arg: substitute it (`%u` converts paths to `file://` with percent
    quoting).
  - More: keep it for iteration.
  - They conflict with any other f/F/u/U.
- `%F`, `%U` (must be the whole arg) expand to all args. `%U` converts to
  URLs. Inside a larger arg they raise.
- `%c` is the expanded Name, `%k` the entry file path, and `%i` becomes
  `--icon <Icon>` when Icon is set.
- When args are given but there is no f/F/u/U field, raise "does not support
  arguments".
- More than one arg with `%f` or `%u` gives a list of argv lists, one per
  arg, substituted at the remembered index.

`path2url`: if the string has a URL scheme, return it unchanged. Otherwise
return `file://` + `urllib.parse.quote(arg)`, where the safe chars are `/`
plus alphanumerics and `_.-~`.

### 6.4 Terminal lookup (P-TERMINAL)
**find_terminal_entry:**
1. The config dirs are `XDG_CONFIG_DIRS` (pyxdg's `xdg_config_dirs`, which
   **includes** `XDG_CONFIG_HOME` first). Add `<datadir>/xdg-terminal-exec`
   for each data dir that is not data_home and not under `$HOME`.
2. In each dir, read `<desktop>-xdg-terminals.list` for each
   `XDG_CURRENT_DESKTOP` item, then `xdg-terminals.list`.
3. Lines:
   - Comments and blanks are skipped.
   - `+id.desktop` protects an entry from exclusion, and `-id.desktop`
     excludes it from the fallback.
   - A plain `id.desktop[:action]` adds an explicit terminal.
   - `/execarg_default:id.desktop:ARG` sets the default execarg (first wins).
   - Anything else is ignored.
4. If there are explicit terminals, search `applications` with the explicit
   list (no ShowIn checks) and return the first match in list order.
5. Otherwise run a fallback search. It uses the negative cache, skips
   excluded IDs, requires `TerminalEmulator` in Categories, passes basic and
   ShowIn checks, and takes the first hit. The negative cache records
   entries that failed to parse, lack the category, or failed basic checks.
   ShowIn failures are not cached. Write the cache if it changed.
6. Nothing found raises "Could not find a Terminal Emulator application".

**Terminal cmdline:**
- Start from the terminal's (action) Exec.
- execarg is the first of the keys `TerminalArgExec`, `X-TerminalArgExec`,
  `ExecArg`, `X-ExecArg`, then the `/execarg_default` value, then `-e`.
  An empty value means no execarg.
- Options from §2.5:
  - `--app-id=` maps to `TerminalArgAppId` or `X-…`, `--title=` to
    `TerminalArgTitle` or `X-…`, and `--dir=` to `TerminalArgDir` or `X-…`.
    If the key value ends in `=`, it is concatenated with the value;
    otherwise they are two separate args. With no key, the option is
    dropped.
  - `--hold` maps to `TerminalArgHold` or `X-…`.
- If cmdline has more than one item and starts with execarg or `-e`, drop
  that first item.
- An empty cmdline, or just `[--]`, means launch the terminal alone:
  - app_name is the terminal ID, and the description its Name - GenericName.
  - There is no execarg.

### 6.5 App daemon (`aux app-daemon`, P-APPDAEMON)
- SIGINT, SIGTERM and SIGHUP exit 0.
- Loop:
  1. Make sure both FIFOs exist. A non-FIFO path is replaced.
  2. If `$XDG_RUNTIME_DIR/uwsm/app_daemon_error` exists, read the old args,
     delete the file, and reply ``error '<Invalid arguments: …>' 2``.
     This is a workaround for argparse exiting the process. rwsm doesn't
     need the crash workaround, since clap's `try_parse` doesn't exit, but
     it must produce the same reply for bad args.
  3. Read IN until EOF, strip leading NULs and split on NUL.
     - Empty: ``error 'No args given!' 2``.
     - `stop`: ``message 'Stopping app daemon.'`` then exit 0.
     - `ping`: `pong`.
     - `[0] != app`: ``error '<Invalid arguments: …>' 2``.
     - Parse error: ``error '<Invalid arguments: <msg>>' 2``.
  4. Reset the cached terminal. Call `app(..., return_cmdline=True)`.
     Note that the daemon path does **not** pass `silent` (`-S` is ignored
     for daemon launches upstream).
     - Single result: `exec <shlex.join(argv)>`.
     - Iterated: `<join> & <join> & … wait`, space-joined.
     - An exception gives ``error '<Error: msg>' 1``.
  5. Reply: open OUT with `O_WRONLY|O_NONBLOCK`, retrying on ENXIO every
     50 ms for up to 10 s, then switch to blocking. Write `<reply>\n`. A
     timeout drops the reply with a warning, and EPIPE also warns.
- The client is `uwsm-app` (kept as is). It also handles `-h` locally, takes
  a lock, enforces a 10 s timeout via SIGPIPE, and runs
  `uwsm-terminal[-service|-scope]`.

Unlike the Python version, a Rust `uwsm app` starts fast, so the daemon
matters less. It still has to exist for configs that use `uwsm-app`.

## 7. Plugin API (P-PLUGINS)
Plugins are shell files at `<datadir>/uwsm/plugins/<bin_id>.sh`, sourced by
prepare-env.sh. rwsm keeps prepare-env.sh, so plugin compatibility comes
free, provided rwsm writes the aux vars file exactly as in §5.3 and passes
the plugins in the same order. Bundled plugins: hyprland (and
start_hyprland), labwc, mango, niri (and niri_session), sway, wayfire.
Ship them unchanged.

The labwc plugin writes its own `55_reload.conf` drop-in marked
`X-UWSMMark=<id>`. That works because `remove_units` matches by mark (§4.5).

## 8. systemd / D-Bus interactions

rwsm should use zbus (blocking API) for these. Every call:

| Purpose | Bus | Call |
|---|---|---|
| Reload | session | `systemd1.Manager.Reload()`, then poll `ListJobs()` |
| Jobs | both | `Manager.ListJobs()`, which returns (id, unit, type, state, job_path, unit_path) |
| Units by pattern | both | `Manager.ListUnitsByPatterns(states, patterns)` |
| Unit property | session | `Manager.GetUnit(id)` → `Properties.Get("org.freedesktop.systemd1.Unit", p)`, falling back to `.Service`, `.Target` etc. (`skip_generic` goes straight to the type interface) |
| Manager props | session | `Properties.Get(Manager, "Environment" / "DefaultStandardOutput" / "DefaultStandardError")` |
| Set env | session | `Manager.SetEnvironment(as)` |
| Unset env | session | `Manager.UnsetEnvironment(as)` |
| D-Bus env | session | `org.freedesktop.DBus.UpdateActivationEnvironment(a{ss})` |
| Stop unit | session | `Manager.StopUnit(name, mode)` (§4.7) |
| Sessions | system | `login1.Manager.ListSessionsEx()`, `GetSession(id)` → `Session.VTNr` / `Remote` / `Leader` |
| Notify | session | `org.freedesktop.Notifications.Notify("UWSM", 0, icon, summary, body, [], {"urgency": byte}, -1)` |

External programs called: `systemctl --user start …`, `systemd-run`,
`systemd-notify`, `systemd-cat`, `whiptail`, `sh`. Keep calling
`systemd-notify` rather than speaking `sd_notify` natively, because
`NOTIFYACCESS=exec` is evaluated against the process that sends the message.

### 8.1 `simple_systemd_escape(s, start)`
- When `start` is true, a leading `.` becomes `\x2e`.
- `/` becomes `-`.
- `[._0-9:A-Za-z]` stays as is. Note that `:` is kept.
- Every other UTF-8 byte becomes `\xNN` (lowercase hex). Note that `-`
  becomes `\x2d`.

It is **not** `systemd-escape`. Copy it exactly.

### 8.2 Reload
See §4.5.

### 8.3 `wait_for_unit(unit, bus, timeout, states, quiet)`
- Only these state sets are allowed: {active,activating}, {active},
  {activating}, {inactive,deactivating}, {inactive}, {deactivating}.
- Return true at once if the unit is already in one of the states. With
  timeout 0, return false.
- Otherwise poll once per second while a matching job (`start` or `stop`)
  is queued. Progress is printed on the first check, then at multiples of
  10, at 15, and for each of the last 10 seconds.
- Re-check the state at the end. If the job was seen and the unit still
  isn't in the state, print "Timed out."

### 8.4 `is_active(check, verbose, verbose_active)`
- `""` checks `graphical-session-pre.target`,
  `wayland-session-pre@*.target`, `graphical-session.target`,
  `wayland-session@*.target` and `wayland-wm@*.service`.
- `"compositor-only"` checks `wayland-wm@*.service`.
- Any other ID checks `wayland-wm@<escaped>.service`.
- Active means state `active` or `activating`.
- Verbose output lists matched and inactive units. When nothing matched, it
  lists the generic set.

### 8.5 `waitenv(names, timeout, step=0.5, end_buffer=3)`
- Filter the names, then poll the systemd env every 0.5 s.
- On the first poll, list everything still expected, and report each name
  as it appears.
- At 80 % of the timeout, warn once.
- On timeout, sleep `end_buffer` (3 s) so systemd's own start timeout fires
  first, then raise TimeoutError.

### 8.6 `check may-start` (P-MAYSTART)
Evaluated in order. Without `-v`, it stops at the first failure.
1. Running as root fails silently.
2. Empty `DBUS_SESSION_BUS_ADDRESS` fails silently.
3. Unless `-i`: `/proc/<ppid>/cmdline` must start with `-` (a login shell).
4. `is_active()` fails **visibly** (shown even without `-v`, but not with
   `-q`).
5. If VTs ≠ [0]: the foreground VT must be in the allowed list.
6. If VTs ≠ [0]: the session (`XDG_SESSION_ID`) VTNr must be non-zero and in
   the list. A missing XDG_SESSION_ID fails.
7. Unless `-r`: the session must not be Remote. A missing XDG_SESSION_ID
   fails.
8. If `-g > 0`: `wait_for_unit(graphical.target, system, {active,
   activating}, g, quiet=-q)`.

Exit 0 if every check passes. With `-v`, print "May start compositor." and
on failure the full list. Otherwise only the visible failures are printed.
Errors always print. Exit 1 on any failure.

> Upstream bug: error paths use `errors.update("msg", exc)`, which adds the
> message's individual characters to a set. rwsm should collect whole
> messages.

### 8.7 `check is-active [-v] [wm]`
An empty `DBUS_SESSION_BUS_ADDRESS` is an error and exits 1. Otherwise exit
0 or 1 depending on `is_active(wm, verbose)`.

## 9. Static unit graph (installed, not generated)
These files are copied as is, with `@BIN_PATH@`, `@BIN_NAME@` and
`@WAITPID_BIN@` substituted:

- `wayland-session-pre@.target`
- `wayland-session@.target`
- `wayland-session-xdg-autostart@.target`
- `wayland-session-envelope@.target`
- `wayland-session-shutdown.target`
- `wayland-wm-env@.service`, which runs `aux prepare-env -- "%I"` and on stop
  `aux cleanup-env`. It is a oneshot with RemainAfterExit and
  `EnvironmentFile=-%t/uwsm/env_session.conf`.
- `wayland-wm@.service`, which is `Type=notify`, `NotifyAccess=all` and runs
  `aux exec -- %I`, with TimeoutStartSec 30 and TimeoutStopSec 10.
- `wayland-session-waitenv.service`, which runs `aux waitenv`.
- `wayland-session-bindpid@.service`, which runs `waitpid -e %i` or falls
  back to `aux waitpid %i`.
- `wayland-wm-app-daemon.service`, which runs `aux app-daemon`.
- `app-graphical.slice`, `background-graphical.slice`,
  `session-graphical.slice`
- Optional: `fumon.service`, `ttyautolock@.service` and presets.

Dependency summary:
- **The envelope** BindsTo env@ and wm@, and PropagatesStopTo wm@.
- **env@** is part of pre@, which BindsTo `graphical-session-pre.target`.
- **wm@** BindsTo `session@`, which BindsTo `graphical-session.target` and
  Wants waitenv and xdg-autostart@.
- **Shutdown:** wm@, env@ and bindpid@ all have
  `OnSuccess=`/`OnFailure=wayland-session-shutdown.target` with
  `replace-irreversibly`. The shutdown target Conflicts with every session
  target.

The rwsm repo carries these templates and installs them unchanged.

## 10. Output and logging (P-OUTPUT)
- **Plain output** goes to stdout. With `UWSM_SILENT_START`, stdout is
  suppressed. When `LogFlag.log` is set, messages are also sent to syslog
  (`LOG_USER`, at the level of the message).
- **Fancy output** (ok in green to stdout, warning in yellow to stdout,
  error in red to stderr): colored when the file is a TTY. When
  `LogFlag.prefix` is set (finalize, app, aux) and the file is not a TTY,
  each line is prefixed with `<N>` (5 for ok, 4 for warning, 3 for error)
  for journald.
- **Notify:** `0` never notifies, `1` notifies when the output is not a TTY,
  and `2` always does. The default urgency is 0 for ok, 1 for warning and 2
  for error. Icons are `applications-system`, `dialog-warning` and
  `dialog-error`; summaries are `Message`, `Warning` and `Error`.
- **Debug:** grey, to stderr, with `DEBUG file:line func … END_DEBUG`. rwsm
  can use `module_path!()`/`line!()` and doesn't need to copy this format.

## 11. Known upstream quirks to decide on

| # | Quirk | Recommendation |
|---|---|---|
| Q1 | `extract_wm_id` returns the whole unit name | Fix (it is only used in messages and equality checks) |
| Q2 | `aux waitenv` references an undefined `bus_session` | Fix (read the property) |
| Q3 | `errors.update(str, exc)` in may-start | Fix |
| Q4 | Man page says the wait timeout default is 10, the code says 30 | Follow the code (30) |
| Q5 | Man page lists `__WM_*_LOWERCASE__` plugin vars that aren't set | Follow the code. Optionally add them, which is harmless |
| Q6 | `OLDWD` typo in the dropped shell vars | Keep for parity |
| Q7 | App daemon ignores `-S` | Keep for parity, or fix (low risk) |
| Q8 | `%%f` yields a literal `%f` | Keep |
| Q9 | `get_active_wm_unit` warning joins dbus structs | Fix the message |
| Q10 | `aux waitenv` declares its names with `type=list`, so any name passed on the command line becomes a list of characters and crashes the filter (the unit never passes names) | Fix: accept plain names |
| Q11 | `-U home` creates the unit dir with `os.mkdir`, so `start` crashes with "No such file or directory" when `~/.config/systemd` does not exist yet | Fix: create missing parents |
| Q12 | `app-daemon`: argparse exits on most bad args (unknown option, missing value), killing the daemon; the reply then comes from the restarted daemon via the `app_daemon_error` flag file | Fix: reply directly with the same `error 'Invalid arguments: <joined args>' 2` text and keep running |

## 12. Parity checklist

Every item is implemented. The table says what checks it. "Parity" means
`tests/parity/ci.sh`, run by the `parity` CI job against Python uwsm:
`cases.txt` lines through `cmp.sh` (output, exit code and generated units),
the generated desktop entries, the live session (`live.sh`, with
`/usr/bin/uwsm` pointing at each implementation in turn) and the app daemon
(`daemon.sh`). "Partial" names what is not compared automatically.

| Item | Status | Checked by |
|---|---|---|
| P-NAME: binary and path naming (§1) | Done | `arch-package` CI job (file list vs Arch's uwsm), `config` and `units` tests |
| P-CLI-*: flags, defaults, env presets (§2), P-CLI-STOP-R, P-CLI-APP-T | Done | 14 `cli` tests; parity `start`/`stop`/`app` lines. Argument errors are worded by clap (quirks note) |
| P-AUX-GUARD (§2.7) | Done | Parity `aux` lines |
| str2bool_plus, XDG paths (§3) | Done | `misc::bool_plus`, `xdg` tests |
| P-START-GST, P-START-ACTIVE, P-START-SIGHANDLER, start sequence (§4.1) | Partial | Parity `start -o`/`-n` lines and the live session; signal handling during start is not exercised |
| P-MAINARG, P-SELF-ENTRY, P-DESKTOP-NAMES, bin_id (§4.2) | Done | `comp` tests; parity `start` lines with `-D`, `-e`, `-N`, `-C`, entries and executables |
| P-UNIT-BYTES: drop-ins, timeouts, tweaks, rungs, marks (§4.3–4.5) | Done | `units::dropins_match_uwsm_bytes`; parity `start -o` lines diff the unit files (`-t`, `-F`, `-U home`) |
| select and default-id (§4.6) | Partial | `select::shorten_matches_textwrap`; the whiptail menu is not driven automatically |
| stop and `-r` (§4.7) | Partial | Parity `stop -n [-r]` and the live stop; `-r` removing files is not compared |
| finalize and notify (§4.8) | Done | Live session (the stand-in compositor calls `uwsm finalize`) |
| P-AUTOREADY (§4.11) | Done | Live session |
| waitpid, waitenv (§4.12–4.13, §8.5) | Done | Live session (`wayland-session-waitenv.service`, bindpid unit) |
| P-ENV-FILTER, Varnames sets and modifiers (§5.1–5.2) | Done | `varnames` tests; live activation environment |
| prepare_env, P-ENV-SESSION, aux vars file (§5.3) | Done | `env` tests; live activation environment and runtime files |
| Activation env set/unset, dbus-broker branch (§5.4) | Partial | Live session on dbus-daemon; the dbus-broker branch is not exercised in CI |
| cleanup_env, cleanup list (§5.5–5.6) | Done | Live `cleanup.list` and environment after stop |
| P-RUNTIME-FILES (§5.7) | Done | Live runtime file list |
| Desktop entry parser, P-ENTRY-VALIDATE, P-ENTRY-EXEC (§6.1) | Done | `desktop_entry` tests (expected errors from pyxdg 0.28); 45 generated entries in parity; all 92 entries of an Omarchy install compared by hand (docs/real-hardware-test.md) |
| P-APP, P-APP-UNITNAME, systemd-run argv (§6.2) | Done | 32 parity `app` lines with a fake `systemd-run`; `app::unit_name_trimming` |
| P-FIELDCODES (§6.3) | Done | `app::field_codes`; parity entries with field codes |
| P-TERMINAL, negative cache (§6.4) | Partial | `app::terminal_list_lines`; parity `app -T` lines; the negative cache file is not compared |
| P-APPDAEMON wire protocol (§6.5) | Done | Parity daemon run; `app::daemon_replies_match_uwsm` |
| P-PLUGINS: shipped scripts unchanged (§7) | Done | `tools/sync-upstream.sh` (byte comparison with the upstream tag) |
| simple_systemd_escape, wait_for_unit, is_active (§8.1–8.4) | Done | `escape` tests; live session |
| P-MAYSTART (§8.6), is-active (§8.7) | Done | Parity `check` lines; live `check is-active -v` |
| Static units installed (§9) | Done | `arch-package` CI job; `tools/sync-upstream.sh` |
| P-OUTPUT: colors, journal prefixes, syslog, notifications (§10) | Partial | Parity compares output with journal prefixes (stdout is not a TTY); colors, syslog and desktop notifications are not compared |

## 13. Suggested crate layout (input for step 2)
```
rwsm/
  Cargo.toml            (bin name "uwsm"; deps: clap, zbus (blocking), nix, shlex?, anyhow/thiserror)
  build.rs              (BIN_PATH, PKG_LIBEXEC_DIR, SH_BIN, WAITPID_BIN, WHIPTAIL_BIN)
  src/main.rs           (dispatch)
  src/cli.rs            (§2)
  src/xdg.rs            (§3.2, pyxdg semantics)
  src/output.rs         (§10)
  src/dbus.rs           (§8 wrappers)
  src/escape.rs         (§8.1, shlex-compatible join)
  src/units.rs          (§4.3–4.5)
  src/comp.rs           (§4.2)
  src/env.rs            (§5)
  src/desktop_entry.rs  (§6.1 parser, validator, tokenizer)
  src/app.rs            (§6.2–6.4)
  src/app_daemon.rs     (§6.5)
  src/check.rs          (§8.3–8.7)
  data/libexec/*.sh, data/plugins/*.sh, data/systemd/user/*   (vendored from uwsm, MIT)
```
uwsm is MIT-licensed (© 2022 Vladimir-csp). The vendored scripts and units
need its license notice kept.
