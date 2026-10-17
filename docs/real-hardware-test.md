# Real hardware test

A checklist for replacing Python uwsm with rwsm on a real machine, written
for Arch-based systems (including Omarchy) that log in through a display
manager running `uwsm start`. Each step says how to undo it.

## Before switching

These run against the live session without changing anything, comparing the
installed Python uwsm with the freshly built rwsm binary (`$B`). Build the
package first (see `packaging/arch/PKGBUILD`) and extract it, or point `$B`
at `target/release/uwsm` built with the same `PREFIX`/`LIBEXECDIR`.

1. Find the login command, which is what has to work:
   `ps -eo args | grep 'uwsm start'`.
2. Compare a dry run of it (writes and starts nothing):
   `diff <(uwsm start -n ARGS 2>&1) <($B start -n ARGS 2>&1)`.
3. Compare `check is-active [-v]` and `check may-start [-v]`.
4. Compare `uwsm app` argument generation for the launches your compositor
   config uses, with a fake `systemd-run` first in `PATH` that prints its
   arguments (unit names end in a random hex suffix, mask it before diffing).
5. Run both app daemons in a private runtime dir and diff their replies:
   `XDG_RUNTIME_DIR=$tmp sh -c 'MANAGERPID=$$ "$0" aux app-daemon; :' $B`
   (the trailing `:` keeps `sh` from exec'ing, so the parent PID check
   passes), then send requests the way `tests/parity/daemon.sh` does. The
   only expected difference is quirk Q12: Python's daemon dies on a bad
   option, rwsm answers and keeps running.
6. If the installed uwsm is older than the one rwsm matches
   (`$B --version`), read upstream's changes in between. 0.26.7 to 0.27.0
   changed `prepare-env.sh`: env files that fail `sh -n` are skipped, as are
   files in `env.d`-style dirs named `*.bak`, `*~`, `*.disabled`,
   `*.example`, `*.sample` or `*.broken`. Check yours with
   `for f in ~/.config/uwsm/env*; do sh -n "$f" || echo "$f"; done`.
   0.27.1 made `uwsm stop` use job mode `replace` instead of `fail`
   (`UWSM_STOP_JOB_MODE` overrides it).
7. Compare `uwsm app <id>.desktop` for every installed desktop entry, which
   exercises the desktop entry validator on real files:
   `ls /usr/share/applications ~/.local/share/applications | grep '\.desktop$'`.

## Way back

Prepare this before switching, so a failed login can be fixed from a TTY
(Ctrl+Alt+F2, or F3 if the display manager holds F2):

- Python uwsm stays in the pacman cache: `ls /var/cache/pacman/pkg/uwsm-*`.
- Undo: `sudo pacman -U /var/cache/pacman/pkg/uwsm-<ver>-any.pkg.tar.zst`
  (pacman offers to remove `rwsm-git`), or `sudo pacman -S uwsm` with network.
- With display manager autologin, a broken `uwsm start` can loop back to the
  greeter. Know how to reach a TTY before you need it.

## Switch

```sh
cd packaging/arch
makepkg -si            # add -d if Rust comes from rustup outside pacman
```

pacman replaces `uwsm` with `rwsm-git`, which provides `uwsm=<upstream
version>`, so packages that depend on uwsm stay installed. `pacman -Syu` will
not switch back on its own, but a script that runs `pacman -S uwsm` will.

The running session keeps the Python daemon and units until you log out. Log
out and back in (or reboot) before testing.

## After switching

Record the result of each item. On failure, save
`journalctl --user -b -o short-monotonic` and the output of the failing
command before undoing anything.

Session lifecycle

- [ ] Login through the display manager reaches the compositor.
- [ ] `uwsm check is-active -v` names the session unit.
- [ ] `systemctl --user list-units 'wayland-*'` shows the same units as
      under Python uwsm, all active; nothing failed.
- [ ] `systemctl --user show-environment` has `WAYLAND_DISPLAY`,
      `XDG_CURRENT_DESKTOP`, compositor-specific variables (for Hyprland
      `HYPRLAND_INSTANCE_SIGNATURE`) and `UWSM_*` as before.
- [ ] XDG autostart entries and `graphical-session.target` units start
      (bar, notifications, clipboard, idle daemon).
- [ ] `ps -eo args | grep -c python.*uwsm` is 0.
- [ ] Logout from the compositor's menu ends the session cleanly, and
      `systemctl --user show-environment` no longer has the session vars.
- [ ] `uwsm stop` from a terminal does the same.
- [ ] Logging in again works (no stale units or runtime files left).

App launching

- [ ] Launcher, terminal, file manager and browser binds open apps.
- [ ] Apps land in `app-graphical.slice` with `app-<desktop>-<name>-<hex>`
      unit names: `systemctl --user list-units --type=scope 'app-*'`.
- [ ] `uwsm app -t service -- <app>` starts a service unit.
- [ ] `uwsm app -T -- htop` opens in the default terminal.
- [ ] Desktop entry launch with a file argument: `uwsm app <id>.desktop <file>`.
- [ ] `uwsm-app` (daemon client) launches are no slower than before.
- [ ] `uwsm-app -- does-not-exist` reports an error (notification or
      stderr) and the daemon stays up:
      `systemctl --user is-active wayland-wm-app-daemon`.

Hardware and seat

- [ ] Switching to a TTY and back keeps the session working.
- [ ] Suspend and resume.
- [ ] Lock screen and unlock.
- [ ] External monitor hotplug (compositor side, but env updates via
      `uwsm finalize` matter for apps started afterwards).
- [ ] Reboot from the session ends cleanly within the stop timeout.

Record the machine, compositor and its version, display manager, uwsm
version replaced and rwsm commit with the results.

## Results

### 2026-10-17: Omarchy, Hyprland, SDDM autologin, before switching

Replacing uwsm 0.26.7 (Arch `extra`) with rwsm 0.1.0 (matching 0.27.0).
Login command: `uwsm start -g -1 -e -D Hyprland hyprland.desktop`.

- Package builds; its file list matches `pacman -Ql uwsm` apart from the
  Python modules and the license directory. Units and plugins are byte
  identical; `prepare-env.sh` differs by the 0.27.0 change above (no
  `~/.config/uwsm` on this machine, so nothing affected).
- Dry run of the login command: identical output.
- `check is-active` (plain, `-v`, with an ID) and `check may-start` (`-v`,
  `-g 0`): identical.
- `uwsm app` for 17 launches taken from Omarchy's launch scripts (terminal,
  file manager, TUIs, web apps, desktop entries, `-t service`, `-T`, errors):
  identical `systemd-run` command lines.
- App daemon, 12 requests: identical apart from Q12.
- `uwsm app` for all 92 installed desktop entries: rwsm refused 7 that
  Python uwsm launches (all six LibreOffice entries, "Invalid key:
  InitialPreference") and worded one error differently. Fixed by porting
  pyxdg's validator (commit b57db37); afterwards all 92, plus 57 synthetic
  edge cases, match.
- Not yet done: everything under "After switching".
