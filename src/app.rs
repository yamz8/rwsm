//! `uwsm app`: launching apps as systemd units, terminal lookup and the app
//! daemon (spec §6).

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;

use crate::cli::{self, AppArgs, AppUnitType, Cli, Mode, Silent, TerminalOpts};
use crate::comp::{MainArg, abspath, normpath};
use crate::config::BIN_NAME;
use crate::dbus::Bus;
use crate::desktop_entry::{
    DesktopEntry, Found, expand_str, find_entries, find_entry_by_id, mtime,
};
use crate::escape::{shlex_join, shlex_quote, simple_systemd_escape};
use crate::misc::sane_split;
use crate::output::{self, Notify};
use crate::varnames::SESSION_SPECIFIC;
use crate::{debug, env, xdg};

/// Variables describing the launching entry, dropped before systemd-run.
const DESKTOP_ENTRY_VARS: [&str; 13] = [
    "DESKTOP_ENTRY_ID",
    "DESKTOP_ENTRY_PATH",
    "DESKTOP_ENTRY_NAME",
    "DESKTOP_ENTRY_NAME_L",
    "DESKTOP_ENTRY_COMMENT",
    "DESKTOP_ENTRY_COMMENT_L",
    "DESKTOP_ENTRY_GENERICNAME",
    "DESKTOP_ENTRY_GENERICNAME_L",
    "DESKTOP_ENTRY_ICON",
    "DESKTOP_ENTRY_ACTION",
    "DESKTOP_ENTRY_ACTION_NAME",
    "DESKTOP_ENTRY_ACTION_NAME_L",
    "DESKTOP_ENTRY_ACTION_ICON",
];

// ---------------------------------------------------------------------------
// Field codes (spec §6.3)

/// `urllib.parse.urlparse(arg).scheme` is non-empty.
fn has_url_scheme(arg: &str) -> bool {
    let Some(i) = arg.find(':') else { return false };
    let scheme = &arg[..i];
    i > 0
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
}

/// `urllib.parse.quote(arg)` with the default safe `/`.
fn url_quote(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len());
    for b in arg.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn path2url(arg: &str) -> String {
    if has_url_scheme(arg) {
        arg.to_string()
    } else {
        format!("file://{}", url_quote(arg))
    }
}

/// Counts `(?!^|[^%])%[a-zA-Z]` matches: `%X` fields not at the very start.
fn count_fields(arg: &str) -> usize {
    let b = arg.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i + 1 < b.len() {
        if i > 0 && b[i] == b'%' && b[i + 1].is_ascii_alphabetic() {
            n += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    n
}

fn has_deprecated_field(arg: &str) -> bool {
    arg.as_bytes()
        .windows(2)
        .any(|w| w[0] == b'%' && b"DdNnvm".contains(&w[1]))
}

/// Rendered entry command: one argv tail, or one per instance.
#[derive(Debug, PartialEq, Eq)]
pub enum EntryArgs {
    Single(Vec<String>),
    Iterated(Vec<Vec<String>>),
}

/// Port of `gen_entry_args`.
pub fn gen_entry_args(
    entry: &DesktopEntry,
    args: &[String],
    action: Option<&str>,
) -> Result<(String, EntryArgs)> {
    let keys = entry.action_keys(action)?;
    let Some((cmd, pre)) = keys.exec.split_first() else {
        bail!("list index out of range");
    };
    debug!("entry_cmd, entry_args pre: {cmd} {pre:?}");

    let mut out: Vec<String> = Vec::new();
    let mut encountered = String::new();
    let mut fu_idx = 0;

    for (idx, arg) in pre.iter().enumerate() {
        debug!("parsing argument {}: \"{arg}\"", idx + 1);
        if count_fields(arg) > 1 {
            bail!("More than one % field in argument: \"{arg}\"");
        }
        let conflict = |encountered: &str| -> Result<()> {
            if encountered.is_empty() {
                Ok(())
            } else {
                Err(anyhow!(
                    "Desktop entry has conflicting args: \"{encountered}\", \"{arg}\""
                ))
            }
        };
        if arg.contains("%%") {
            out.push(arg.replace("%%", "%"));
        } else if has_deprecated_field(arg) {
            debug!("dropped as deprecated");
        } else if arg.contains("%f") {
            conflict(&encountered)?;
            encountered = arg.clone();
            fu_idx = out.len();
            match args.len() {
                0 => {}
                1 => out.push(arg.replace("%f", &args[0])),
                _ => out.push(arg.clone()),
            }
        } else if arg == "%F" {
            conflict(&encountered)?;
            encountered = arg.clone();
            out.extend(args.iter().cloned());
        } else if arg.contains("%F") {
            bail!("Encountered \"%F\" inside argument: \"{arg}\"");
        } else if arg.contains("%u") {
            conflict(&encountered)?;
            encountered = arg.clone();
            fu_idx = out.len();
            match args.len() {
                0 => {}
                1 => out.push(arg.replace("%u", &path2url(&args[0]))),
                _ => out.push(arg.clone()),
            }
        } else if arg == "%U" {
            conflict(&encountered)?;
            encountered = arg.clone();
            out.extend(args.iter().map(|a| path2url(a)));
        } else if arg.contains("%U") {
            bail!("Encountered \"%U\" inside argument: \"{arg}\"");
        } else if arg == "%c" {
            out.push(expand_str(entry.name()));
        } else if arg == "%k" {
            out.push(entry.path.to_string_lossy().into_owned());
        } else if arg == "%i" {
            if !keys.icon.is_empty() {
                out.push("--icon".into());
                out.push(keys.icon.clone());
            }
        } else {
            out.push(arg.clone());
        }
    }
    debug!("entry_cmd, entry_args post: {cmd} {out:?}");

    if !args.is_empty() && encountered.is_empty() {
        let base = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match action {
            Some(a) => bail!("Entry \"{base}\" action \"{a}\" does not support arguments"),
            None => bail!("Entry \"{base}\" does not support arguments"),
        }
    }

    // `encountered` holds the whole arg; upstream compares it to the bare
    // field, so only args that are exactly %f/%u iterate.
    if args.len() > 1 && (encountered == "%f" || encountered == "%u") {
        let instances = args
            .iter()
            .map(|a| {
                let a = if encountered == "%u" {
                    path2url(a)
                } else {
                    a.clone()
                };
                let mut cur = out.clone();
                cur[fu_idx] = cur[fu_idx].replace(&encountered, &a);
                cur
            })
            .collect();
        return Ok((cmd.clone(), EntryArgs::Iterated(instances)));
    }
    Ok((cmd.clone(), EntryArgs::Single(out)))
}

// ---------------------------------------------------------------------------
// Terminal lookup (spec §6.4)

pub struct TerminalEntry {
    pub entry: DesktopEntry,
    pub id: String,
    pub action: Option<String>,
}

#[derive(Default)]
struct TerminalConfig {
    explicit: Vec<(String, Option<String>)>,
    excluded: Vec<String>,
    unexcluded: Vec<String>,
    default_execargs: HashMap<String, String>,
}

fn terminal_config_dirs() -> Vec<PathBuf> {
    let mut dirs = xdg::config_dirs();
    let data_home = normpath(&xdg::data_home().to_string_lossy());
    let home = normpath(&crate::comp::expanduser("~"));
    for path in xdg::data_dirs() {
        let path = normpath(&path.to_string_lossy());
        if path == data_home || path.starts_with(&home) {
            continue;
        }
        dirs.push(Path::new(&path).join("xdg-terminal-exec"));
    }
    dirs
}

fn read_terminal_config() -> TerminalConfig {
    let mut cfg = TerminalConfig::default();
    let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let mut names: Vec<String> = sane_split(&desktops, ":")
        .into_iter()
        .filter(|d| !d.is_empty())
        .map(|d| format!("{d}-xdg-terminals.list"))
        .collect();
    names.push("xdg-terminals.list".into());

    for dir in terminal_config_dirs() {
        for name in &names {
            let file = dir.join(name);
            let text = match fs::read_to_string(&file) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    output::warning_notify(&e.to_string(), Notify::IfNotTty);
                    continue;
                }
            };
            debug!("reading {}", file.display());
            for line in text.lines().map(str::trim) {
                parse_terminal_line(&mut cfg, line);
            }
        }
    }
    debug!("explicit terminal_entries {:?}", cfg.explicit);
    cfg
}

fn parse_terminal_line(cfg: &mut TerminalConfig, line: &str) {
    if line.is_empty() || line.starts_with('#') {
        return;
    }
    let first = line.as_bytes()[0];
    let (control, line) = if line.starts_with('-') && line.contains(".desktop") {
        (-1, &line[1..])
    } else if line.starts_with('+') && line.contains(".desktop") {
        (1, &line[1..])
    } else if (first.is_ascii_alphabetic() || first == b'_') && line.contains(".desktop") {
        (0, line)
    } else if let Some(rest) = line.strip_prefix("/execarg_default:")
        && rest.contains(".desktop:")
    {
        let (entry, arg) = rest.split_once(".desktop:").unwrap_or_default();
        cfg.default_execargs
            .entry(format!("{entry}.desktop"))
            .or_insert_with(|| arg.to_string());
        return;
    } else {
        debug!("discarded config line {line}");
        return;
    };
    let Ok(arg) = MainArg::new(Some(line)) else {
        return;
    };
    let Some(id) = arg.entry_id.filter(|_| arg.path.is_none()) else {
        return;
    };
    let action = arg.entry_action;
    let listed = cfg.excluded.contains(&id) || cfg.unexcluded.contains(&id);
    match control {
        0 if !cfg.explicit.contains(&(id.clone(), action.clone())) => {
            cfg.explicit.push((id, action))
        }
        -1 if action.is_none() && !listed => cfg.excluded.push(id),
        1 if action.is_none() && !listed => cfg.unexcluded.push(id),
        _ => debug!("ignored line {line}"),
    }
}

fn neg_cache_path(name: &str) -> PathBuf {
    xdg::cache_home().join(format!("{BIN_NAME}-{name}"))
}

/// Port of `read_neg_cache`: `path;mtime` lines. A bad line removes the file
/// and keeps what was read before it.
fn read_neg_cache(name: &str) -> HashMap<PathBuf, f64> {
    let path = neg_cache_path(name);
    let mut out = HashMap::new();
    let Ok(text) = fs::read_to_string(&path) else {
        debug!("no cache {}", path.display());
        return out;
    };
    for line in text.lines() {
        let parts: Vec<&str> = line.trim().split(';').collect();
        let parsed = match parts.as_slice() {
            [p, m] => m.trim().parse::<f64>().ok().map(|m| (PathBuf::from(p), m)),
            _ => None,
        };
        match parsed {
            Some((p, m)) => {
                out.insert(p, m);
            }
            None => {
                debug!("Removing cache file {} due to a bad line", path.display());
                let _ = fs::remove_file(&path);
                break;
            }
        }
    }
    out
}

/// Python's `repr(float)` for the values mtimes take.
fn py_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn write_neg_cache(name: &str, data: &HashMap<PathBuf, f64>) {
    let path = neg_cache_path(name);
    debug!("writing cache {} ({} items)", path.display(), data.len());
    let body: String = data
        .iter()
        .map(|(p, m)| format!("{};{}\n", p.display(), py_float(*m)))
        .collect();
    if fs::create_dir_all(xdg::cache_home())
        .and_then(|_| fs::write(&path, body))
        .is_err()
        && path.is_file()
    {
        let _ = fs::remove_file(&path);
    }
}

fn is_terminal_emulator(entry: &DesktopEntry) -> bool {
    entry.categories().iter().any(|c| c == "TerminalEmulator")
}

/// Port of `find_terminal_entry`. Also returns the config for execarg defaults.
fn find_terminal_entry() -> Result<(TerminalEntry, HashMap<String, String>)> {
    let cfg = read_terminal_config();

    if !cfg.explicit.is_empty() {
        let explicit = &cfg.explicit;
        let found: Vec<TerminalEntry> = find_entries("applications", None, &[], |id, path| {
            if !explicit.iter().any(|(e, _)| e == id) {
                return Found::Drop;
            }
            let Ok(entry) = DesktopEntry::load(path) else {
                return Found::Drop;
            };
            if !is_terminal_emulator(&entry) {
                return Found::Drop;
            }
            let mut results = Vec::new();
            let mut seen = Vec::new();
            for (_, action) in explicit.iter().filter(|(e, _)| e == id) {
                if seen.contains(action) {
                    continue;
                }
                seen.push(action.clone());
                if entry.check_basic(action.as_deref()).is_err() {
                    debug!("action {action:?} failed basic checks");
                    continue;
                }
                let te = TerminalEntry {
                    entry: entry.clone(),
                    id: id.to_string(),
                    action: action.clone(),
                };
                if (id, action) == (explicit[0].0.as_str(), &explicit[0].1) {
                    return Found::Return(te);
                }
                results.push(te);
            }
            if results.is_empty() {
                Found::Drop
            } else {
                Found::Extend(results)
            }
        });
        debug!("found {} entries", found.len());
        let mut found: Vec<Option<TerminalEntry>> = found.into_iter().map(Some).collect();
        for (id, action) in &cfg.explicit {
            if let Some(slot) = found.iter_mut().find(|t| {
                t.as_ref()
                    .is_some_and(|t| &t.id == id && &t.action == action)
            }) {
                return Ok((slot.take().unwrap(), cfg.default_execargs));
            }
        }
    }

    debug!("no explicit terminals matched, starting terminal entry search");
    let initial = read_neg_cache("not-terminals");
    let mut cache = initial.clone();
    let found = find_entries("applications", Some(&initial), &cfg.excluded, |id, path| {
        let mut reject = |path: &Path| {
            if let Some(m) = mtime(path) {
                cache.insert(path.to_path_buf(), m);
            }
            Found::Drop
        };
        let Ok(entry) = DesktopEntry::load(path) else {
            return reject(path);
        };
        if !is_terminal_emulator(&entry) || entry.check_basic(None).is_err() {
            return reject(path);
        }
        if entry.check_showin().is_err() {
            return Found::Drop;
        }
        Found::Return(TerminalEntry {
            entry,
            id: id.to_string(),
            action: None,
        })
    });
    if let Some(t) = found.into_iter().next() {
        debug!("found terminal {}", t.id);
        if cache != initial {
            write_neg_cache("not-terminals", &cache);
        }
        return Ok((t, cfg.default_execargs));
    }
    bail!("Could not find a Terminal Emulator application")
}

/// The resolved terminal, looked up once per invocation.
pub struct Terminal {
    pub found: TerminalEntry,
    default_execargs: HashMap<String, String>,
}

impl Terminal {
    fn key(&self, names: &[&str]) -> Option<String> {
        names
            .iter()
            .find(|k| self.found.entry.has_key(k))
            .map(|k| self.found.entry.get(k).to_string())
    }

    fn execarg(&self) -> Vec<String> {
        let arg = self
            .key(&[
                "TerminalArgExec",
                "X-TerminalArgExec",
                "ExecArg",
                "X-ExecArg",
            ])
            .or_else(|| self.default_execargs.get(&self.found.id).cloned())
            .unwrap_or_else(|| "-e".into());
        if arg.is_empty() {
            vec![]
        } else {
            vec![expand_str(&arg)]
        }
    }

    /// Terminal (action) Exec plus mapped `-T` options.
    fn cmdline(&self, opts: &TerminalOpts) -> Result<Vec<String>> {
        let mut cmd = self
            .found
            .entry
            .action_keys(self.found.action.as_deref())?
            .exec;
        for opt in &opts.opts {
            let mapped = [
                ("--app-id=", "TerminalArgAppId"),
                ("--title=", "TerminalArgTitle"),
                ("--dir=", "TerminalArgDir"),
            ]
            .into_iter()
            .find(|(prefix, _)| opt.starts_with(prefix));
            if let Some((_, key)) = mapped {
                let value = opt.split_once('=').map(|(_, v)| v).unwrap_or_default();
                let arg = self.key(&[key, &format!("X-{key}")]).unwrap_or_default();
                if arg.ends_with('=') {
                    cmd.push(format!("{arg}{value}"));
                } else if !arg.is_empty() {
                    cmd.push(arg);
                    cmd.push(value.to_string());
                }
            } else if opt == "--hold"
                && let Some(hold) = self.key(&["TerminalArgHold", "X-TerminalArgHold"])
            {
                cmd.push(hold);
            }
        }
        Ok(cmd)
    }
}

// ---------------------------------------------------------------------------
// app() (spec §6.2)

pub struct AppRequest {
    pub cmdline: Vec<String>,
    pub terminal: bool,
    pub slice_name: String,
    pub unit_type: AppUnitType,
    pub app_name: String,
    pub unit_name: String,
    pub unit_description: String,
    pub silent: Option<Silent>,
    pub workdir: Option<String>,
}

/// Final systemd-run argv: one, or one per instance.
#[derive(Debug)]
pub enum Launch {
    Single(Vec<String>),
    Iterated(Vec<Vec<String>>),
}

pub struct AppCtx {
    terminal_opts: TerminalOpts,
    terminal: Option<Terminal>,
    /// Set once `DESKTOP_ENTRY_*` are "popped" (after the first forked instance).
    entry_env_dropped: bool,
    /// Daemon mode: only build argv, never touch the environment.
    return_cmdline: bool,
}

impl AppCtx {
    pub fn new(terminal_opts: TerminalOpts, return_cmdline: bool) -> Self {
        Self {
            terminal_opts,
            terminal: None,
            entry_env_dropped: false,
            return_cmdline,
        }
    }

    fn entry_var(&self, name: &str) -> String {
        if self.entry_env_dropped {
            String::new()
        } else {
            std::env::var(name).unwrap_or_default()
        }
    }

    fn terminal(&mut self) -> Result<&Terminal> {
        if self.terminal.is_none() {
            let (found, default_execargs) = find_terminal_entry()?;
            self.terminal = Some(Terminal {
                found,
                default_execargs,
            });
        }
        Ok(self.terminal.as_ref().unwrap())
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Trims an escaped unit name part so that `fixed + part` stays under
/// `limit`, never splitting a `\xNN` sequence.
fn trim_escaped(part: &str, fixed: usize, limit: usize) -> String {
    let mut fragments = Vec::new();
    let mut rest = part;
    while let Some(pos) = rest.find("\\x") {
        if rest.len() < pos + 4 {
            break;
        }
        fragments.push(&rest[..pos]);
        fragments.push(&rest[pos..pos + 4]);
        rest = &rest[pos + 4..];
    }
    fragments.push(rest);

    let mut out = String::new();
    let mut used = fixed;
    for fragment in fragments {
        if fragment.len() + used < limit {
            out.push_str(fragment);
            used += fragment.len();
        } else {
            if !fragment.starts_with("\\x") {
                out.push_str(&fragment[..limit.saturating_sub(used).min(fragment.len())]);
            }
            break;
        }
    }
    out
}

pub fn gen_unit_name(app_name: &str, cmd0: &str, unit_type: AppUnitType) -> String {
    let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "uwsm".into());
    let first = sane_split(&desktops, ":")
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut desk = simple_systemd_escape(first, false);
    let name = if app_name.is_empty() {
        basename(cmd0)
    } else {
        app_name
    };
    let mut cmd = simple_systemd_escape(name, false);

    let l_static = "app---DEADBEEF.".len() + unit_type.as_str().len();
    if l_static + desk.len() > 127 {
        desk = trim_escaped(&desk, l_static, 127);
    }
    if l_static + desk.len() + cmd.len() > 255 {
        cmd = trim_escaped(&cmd, l_static + desk.len(), 255);
    }
    match unit_type {
        AppUnitType::Scope => format!("app-{desk}-{cmd}-{}.scope", env::random_hex(8)),
        AppUnitType::Service => format!("app-{desk}-{cmd}@{}.service", env::random_hex(8)),
    }
}

/// Port of `app()` up to the point of launching: returns the systemd-run argv.
pub fn build(ctx: &mut AppCtx, mut req: AppRequest, props: &mut Vec<String>) -> Result<Launch> {
    for prop in props.iter() {
        if !prop.contains('=') {
            bail!("Expected systemd unit property assignment, got: \"{prop}\"");
        }
    }

    let main_arg = MainArg::new(req.cmdline.first().map(String::as_str))?;
    debug!("main_arg {main_arg:?}");
    if main_arg.path.is_some() {
        main_arg.check_path()?;
    }

    if let Some(entry_id) = &main_arg.entry_id {
        let entry = match &main_arg.path {
            Some(path) => {
                let entry = DesktopEntry::load(Path::new(path)).with_context(|| {
                    format!("Failed to parse entry \"{entry_id}\" from \"{path}\"!")
                })?;
                entry.check_basic(main_arg.entry_action.as_deref())?;
                entry
            }
            None => find_entry_by_id("applications", entry_id, main_arg.entry_action.as_deref())?
                .ok_or_else(|| anyhow!("Desktop entry not found: \"{}\"", req.cmdline[0]))?,
        };
        props.push(format!(
            "SourcePath={}",
            abspath(&entry.path.to_string_lossy())
        ));

        if entry.terminal() {
            debug!("entry requested a terminal");
            req.terminal = true;
        }
        let path_key = entry.path_key();
        req.workdir = (!path_key.is_empty()).then(|| path_key.to_string());
        if let Some(dir) = &req.workdir
            && !Path::new(dir).is_dir()
        {
            bail!("\"Path\" requested by entry \"{entry_id}\" is not a directory: \"{dir}\"");
        }
        if req.app_name.is_empty() {
            req.app_name = match entry_id.rfind('.') {
                Some(i) if i > 0 => entry_id[..i].to_string(),
                _ => entry_id.clone(),
            };
        }
        if req.unit_description.is_empty() {
            let name = expand_str(entry.name());
            let generic = expand_str(entry.generic_name());
            req.unit_description = if !generic.is_empty() && generic != name {
                format!("{name} - {generic}")
            } else {
                name
            };
        }

        let (cmd, args) =
            gen_entry_args(&entry, &req.cmdline[1..], main_arg.entry_action.as_deref())?;
        match args {
            EntryArgs::Iterated(instances) => {
                if !req.unit_name.is_empty() {
                    output::warning(&format!(
                        "Dropping unit name \"{}\" because entry \"{}\" requires multiple instances for given arguments.",
                        req.unit_name,
                        entry
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default()
                    ));
                }
                let mut out = Vec::new();
                for instance in instances {
                    let sub = AppRequest {
                        cmdline: std::iter::once(cmd.clone()).chain(instance).collect(),
                        terminal: req.terminal,
                        slice_name: req.slice_name.clone(),
                        unit_type: req.unit_type,
                        app_name: req.app_name.clone(),
                        unit_name: String::new(),
                        unit_description: req.unit_description.clone(),
                        silent: req.silent,
                        workdir: req.workdir.clone(),
                    };
                    match build(ctx, sub, props)? {
                        Launch::Single(argv) => out.push(argv),
                        Launch::Iterated(_) => unreachable!("instances are plain commands"),
                    }
                    // upstream pops DESKTOP_ENTRY_* after each forked instance
                    if !ctx.return_cmdline {
                        ctx.entry_env_dropped = true;
                    }
                }
                return Ok(Launch::Iterated(out));
            }
            EntryArgs::Single(args) => {
                req.cmdline = std::iter::once(cmd).chain(args).collect();
            }
        }
    } else {
        let id = ctx.entry_var("DESKTOP_ENTRY_ID");
        if req.app_name.is_empty() && !id.is_empty() {
            req.app_name = id.strip_suffix(".desktop").unwrap_or(&id).to_string();
        }
        let path = ctx.entry_var("DESKTOP_ENTRY_PATH");
        if !path.is_empty() {
            props.push(format!("SourcePath={}", abspath(&path)));
        }
        if req.unit_description.is_empty() {
            let first = |a: &str, b: &str| {
                let v = ctx.entry_var(a);
                if v.is_empty() { ctx.entry_var(b) } else { v }
            };
            let parts = [
                first("DESKTOP_ENTRY_NAME_L", "DESKTOP_ENTRY_NAME"),
                first("DESKTOP_ENTRY_GENERICNAME_L", "DESKTOP_ENTRY_GENERICNAME"),
            ];
            req.unit_description = parts
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" - ");
        }
    }

    let mut cmdline = req.cmdline;
    let (terminal_cmdline, execarg) = if req.terminal {
        let opts = ctx.terminal_opts.clone();
        let term = ctx.terminal()?;
        let terminal_cmdline = term.cmdline(&opts)?;
        let mut execarg = term.execarg();
        if cmdline.len() > 1 && (cmdline[..1] == execarg[..] || cmdline[0] == "-e") {
            debug!("discarded explicit terminal exec arg {}", cmdline[0]);
            cmdline.remove(0);
        }
        if cmdline.is_empty() || cmdline == ["--"] {
            if req.app_name.is_empty() {
                let id = &term.found.id;
                req.app_name = match id.rfind('.') {
                    Some(i) if i > 0 => id[..i].to_string(),
                    _ => id.clone(),
                };
            }
            if req.unit_description.is_empty() {
                req.unit_description = [
                    expand_str(term.found.entry.name()),
                    expand_str(term.found.entry.generic_name()),
                ]
                .into_iter()
                .filter(|n| !n.is_empty())
                .collect::<Vec<_>>()
                .join(" - ");
            }
            cmdline.clear();
            execarg.clear();
        }
        (terminal_cmdline, execarg)
    } else {
        (vec![], vec![])
    };

    if req.unit_description.is_empty() {
        // upstream: `(app_name or basename(cmd)) if cmdline else "App launched by uwsm"`
        req.unit_description = match cmdline.first() {
            Some(c) if req.app_name.is_empty() => basename(c).to_string(),
            Some(_) => req.app_name.clone(),
            None => format!("App launched by {BIN_NAME}"),
        };
    }

    if let Some(c) = cmdline.first()
        && xdg::which(c).is_none()
    {
        bail!("Command not found: \"{c}\"");
    }

    let slice = match req.slice_name.as_str() {
        "a" => "app-graphical.slice".to_string(),
        "b" => "background-graphical.slice".to_string(),
        "s" => "session-graphical.slice".to_string(),
        s if s.ends_with(".slice") => s.to_string(),
        s => bail!("Invalid slice name: {s}!"),
    };

    let type_str = req.unit_type.as_str();
    let unit_name = if req.unit_name.is_empty() {
        gen_unit_name(
            &req.app_name,
            cmdline.first().map(String::as_str).unwrap_or_default(),
            req.unit_type,
        )
    } else {
        if !req.unit_name.ends_with(&format!(".{type_str}")) {
            bail!("Only \".{type_str}\" is supported as unit suffix for {type_str} unit type");
        }
        if req.unit_name.chars().count() > 255 {
            bail!(
                "Unit name is too long ({} > 255): {}",
                req.unit_name.chars().count(),
                req.unit_name
            );
        }
        req.unit_name
    };

    let mut argv: Vec<String> = vec!["systemd-run".into(), "--user".into()];
    match req.unit_type {
        AppUnitType::Scope => argv.push("--scope".into()),
        AppUnitType::Service => {
            argv.push("--property=Type=exec".into());
            argv.push("--property=ExitType=cgroup".into());
            let mut names = SESSION_SPECIFIC.to_vec();
            names.sort();
            for name in names {
                let value = std::env::var(name).unwrap_or_default();
                if !value.is_empty() {
                    argv.push(format!("--setenv={name}={value}"));
                }
            }
            match req.silent {
                Some(Silent::Out) => {
                    argv.push("--property=StandardOutput=null".into());
                    let bus = Bus::session()?;
                    if bus.get_manager_property_string("DefaultStandardError")? == "inherit" {
                        let out = bus.get_manager_property_string("DefaultStandardOutput")?;
                        argv.push(format!("--property=StandardError={out}"));
                    }
                }
                Some(Silent::Err) => argv.push("--property=StandardError=null".into()),
                Some(Silent::Both) => {
                    argv.push("--property=StandardOutput=null".into());
                    argv.push("--property=StandardError=null".into());
                }
                None => {}
            }
        }
    }
    argv.extend(props.iter().map(|p| format!("--property={p}")));
    argv.push(format!("--slice={slice}"));
    argv.push(format!("--unit={unit_name}"));
    argv.push(format!("--description={}", req.unit_description));
    argv.push("--quiet".into());
    argv.push("--collect".into());
    argv.push(match &req.workdir {
        Some(dir) => format!("--working-directory={dir}"),
        None => "--same-dir".into(),
    });
    argv.push("--".into());
    argv.extend(terminal_cmdline);
    argv.extend(execarg);
    argv.extend(cmdline);
    debug!("final_args {argv:?}");
    Ok(Launch::Single(argv))
}

fn systemd_run(argv: &[String]) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    for var in DESKTOP_ENTRY_VARS {
        cmd.env_remove(var);
    }
    cmd
}

fn request_from_args(args: AppArgs) -> (AppRequest, Vec<String>) {
    let (unit_type, warn) = cli::resolve_app_unit_type(
        args.app_unit_type,
        std::env::var("UWSM_APP_UNIT_TYPE").ok().as_deref(),
    );
    if let Some(w) = warn {
        output::warning(&w);
    }
    (
        AppRequest {
            cmdline: args.cmdline,
            terminal: args.terminal,
            slice_name: args.slice_name,
            unit_type,
            app_name: args.app_name,
            unit_name: args.unit_name,
            unit_description: args.unit_description,
            silent: args.silent,
            workdir: None,
        },
        args.unit_properties,
    )
}

fn run(args: AppArgs, terminal_opts: TerminalOpts) -> Result<u8> {
    // uwsm warns about the env preset while parsing, before the prefix is on.
    let (req, mut props) = request_from_args(args);
    output::set_prefix(true);
    let silent = req.silent;
    let unit_type = req.unit_type;
    let mut ctx = AppCtx::new(terminal_opts, false);
    match build(&mut ctx, req, &mut props)? {
        Launch::Single(argv) => {
            if unit_type == AppUnitType::Scope
                && let Some(silent) = silent
            {
                let null = fs::OpenOptions::new().write(true).open("/dev/null")?;
                use std::os::fd::AsRawFd;
                if matches!(silent, Silent::Out | Silent::Both) {
                    unsafe { libc::dup2(null.as_raw_fd(), 1) };
                }
                if matches!(silent, Silent::Err | Silent::Both) {
                    unsafe { libc::dup2(null.as_raw_fd(), 2) };
                }
            }
            let err = systemd_run(&argv).exec();
            Err(anyhow!("{err}"))
        }
        Launch::Iterated(instances) => {
            let mut children: Vec<(Child, String, Option<i32>)> = Vec::new();
            for argv in instances {
                let tail = argv
                    .iter()
                    .position(|a| a == "--")
                    .map(|i| shlex_join(&argv[i + 1..]))
                    .unwrap_or_default();
                children.push((systemd_run(&argv).spawn()?, tail, None));
            }
            while children.iter().any(|c| c.2.is_none()) {
                for (child, tail, rc) in children.iter_mut().filter(|c| c.2.is_none()) {
                    if let Some(status) = child.try_wait()? {
                        let code = status
                            .code()
                            .unwrap_or_else(|| -status.signal().unwrap_or(0));
                        *rc = Some(code);
                        let msg = format!("systemd-run for \"{tail}\" returned {code}.");
                        if code == 0 {
                            output::normal(&msg);
                        } else {
                            output::error_notify(&msg, Notify::IfNotTty);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(u8::from(children.iter().any(|c| c.2 != Some(0))))
        }
    }
}

use std::os::unix::process::ExitStatusExt;

/// `uwsm app`.
pub fn app_main(args: AppArgs, terminal_opts: TerminalOpts) -> u8 {
    match run(args, terminal_opts) {
        Ok(code) => code,
        Err(e) => {
            output::error_notify(&format!("{e:#}"), Notify::IfNotTty);
            1
        }
    }
}

// ---------------------------------------------------------------------------
// App daemon (spec §6.5)

fn create_fifo(name: &str) -> Result<PathBuf> {
    let path = xdg::runtime_dir()?.join(name);
    if let Ok(meta) = fs::metadata(&path) {
        if meta.file_type().is_fifo() {
            return Ok(path);
        }
        debug!("not a fifo: {}, removing.", path.display());
        fs::remove_file(&path)?;
    }
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    if unsafe { libc::mkfifo(c.as_ptr(), 0o666) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(path)
}

/// Opens a FIFO for writing, waiting up to `timeout` for a reader.
fn open_fifo_write(path: &Path, timeout: Duration) -> std::io::Result<Option<fs::File>> {
    let deadline = Instant::now() + timeout;
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(f) => {
                use std::os::fd::AsRawFd;
                let fd = f.as_raw_fd();
                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
                }
                return Ok(Some(f));
            }
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

fn send_reply(args_in: &[String], reply: &str) -> Result<()> {
    output::normal(&format!("received: {}\nsent: {reply}", shlex_join(args_in)));
    let path = create_fifo("uwsm-app-daemon-out")?;
    let Some(mut f) = open_fifo_write(&path, Duration::from_secs(10))? else {
        output::warning(&format!(
            "Timed out waiting for client to read {}, dropping reply",
            path.display()
        ));
        return Ok(());
    };
    if let Err(e) = f.write_all(format!("{reply}\n").as_bytes()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            output::warning(&format!(
                "Client closed {} before reply was written",
                path.display()
            ));
        } else {
            return Err(e.into());
        }
    }
    Ok(())
}

extern "C" fn stop_on_signal(sig: libc::c_int) {
    // Only async-signal-safe calls here.
    let msg = format!("Received signal {sig}, stopping app daemon...\n");
    unsafe {
        libc::write(1, msg.as_ptr().cast(), msg.len());
        libc::_exit(0);
    }
}

fn invalid_args(args: &[String]) -> String {
    format!(
        "error {} 2",
        shlex_quote(&format!("Invalid arguments: {}", shlex_join(args)))
    )
}

/// Builds the daemon's shell reply for one `app ...` request.
pub fn daemon_reply(mut args_in: Vec<String>) -> String {
    let terminal_opts = cli::extract_terminal_opts(&mut args_in);
    let parsed =
        Cli::try_parse_from(std::iter::once(BIN_NAME.to_string()).chain(args_in.iter().cloned()));
    let args = match parsed {
        Ok(Cli {
            mode: Mode::App(a), ..
        }) => a,
        // argparse exits on most bad args; uwsm's restart workaround then
        // replies with the joined args, which is what this matches.
        _ => return invalid_args(&args_in),
    };
    let (req, mut props) = request_from_args(args);
    let req = AppRequest {
        silent: None,
        ..req
    };
    let mut ctx = AppCtx::new(terminal_opts, true);
    match build(&mut ctx, req, &mut props) {
        Ok(Launch::Single(argv)) => format!("exec {}", shlex_join(&argv)),
        Ok(Launch::Iterated(all)) => {
            let mut parts: Vec<String> =
                all.iter().map(|a| format!("{} &", shlex_join(a))).collect();
            parts.push("wait".into());
            parts.join(" ")
        }
        Err(e) => format!("error {} 1", shlex_quote(&format!("Error: {e:#}"))),
    }
}

/// `uwsm aux app-daemon`.
pub fn app_daemon() -> Result<u8> {
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(sig, stop_on_signal as *const () as libc::sighandler_t);
        }
    }
    let error_flag = xdg::runtime_dir()?.join("uwsm").join("app_daemon_error");

    loop {
        let fifo_in = create_fifo("uwsm-app-daemon-in")?;
        create_fifo("uwsm-app-daemon-out")?;

        // Left behind by a Python daemon that argparse killed.
        if error_flag.is_file() {
            output::normal(&format!("error flag {} exists", error_flag.display()));
            let old = fs::read_to_string(&error_flag).unwrap_or_default();
            fs::remove_file(&error_flag)?;
            let old_args: Vec<String> = sane_split(old.trim_start_matches('\0'), "\0")
                .into_iter()
                .map(str::to_string)
                .collect();
            send_reply(&old_args, &invalid_args(&old_args))?;
            continue;
        }

        debug!("reading command...");
        let mut raw = Vec::new();
        fs::File::open(&fifo_in)?.read_to_end(&mut raw)?;
        let line = String::from_utf8_lossy(&raw);
        let args_in: Vec<String> = sane_split(line.trim_start_matches('\0'), "\0")
            .into_iter()
            .map(str::to_string)
            .collect();
        debug!("args_in {args_in:?}");

        match args_in.first().map(String::as_str) {
            None => send_reply(&args_in, "error 'No args given!' 2")?,
            Some("stop") => {
                send_reply(&args_in, "message 'Stopping app daemon.'")?;
                output::normal("Exiting.");
                return Ok(0);
            }
            Some("ping") => send_reply(&args_in, "pong")?,
            Some("app") => {
                let mut shown = args_in.clone();
                cli::extract_terminal_opts(&mut shown);
                let reply = daemon_reply(args_in);
                send_reply(&shown, &reply)?;
            }
            Some(_) => send_reply(&args_in, &invalid_args(&args_in))?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(path2url("/tmp/a b"), "file:///tmp/a%20b");
        assert_eq!(path2url("https://x.org/?a"), "https://x.org/?a");
        assert_eq!(path2url("rel/ü"), "file://rel/%C3%BC");
        assert_eq!(path2url("1x:y"), "file://1x%3Ay");
    }

    #[test]
    fn field_counting() {
        assert_eq!(count_fields("%f"), 0);
        assert_eq!(count_fields("%f%u"), 1);
        assert_eq!(count_fields("--x=%f%u"), 2);
        assert_eq!(count_fields("%%f"), 1);
    }

    #[test]
    fn escaped_trim_keeps_sequences_whole() {
        assert_eq!(trim_escaped("ab\\x2dcd", 0, 4), "ab");
        assert_eq!(trim_escaped("ab\\x2dcd", 0, 7), "ab\\x2dc");
        assert_eq!(trim_escaped("ab\\x2dcd", 0, 5), "ab");
        assert_eq!(trim_escaped("abcdef", 0, 4), "abcd");
    }

    fn entry(exec: &str) -> DesktopEntry {
        DesktopEntry::parse(
            Path::new("/x/t.desktop"),
            &format!("[Desktop Entry]\nType=Application\nName=T\nIcon=ic\nExec={exec}\n"),
        )
        .unwrap()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn field_codes() {
        let e = entry("sh %f --x %i %c %k %%d %d");
        let (cmd, args) = gen_entry_args(&e, &s(&["a"]), None).unwrap();
        assert_eq!(cmd, "sh");
        assert_eq!(
            args,
            EntryArgs::Single(s(&["a", "--x", "--icon", "ic", "T", "/x/t.desktop", "%d"]))
        );
        let (_, args) = gen_entry_args(&e, &s(&["a", "b"]), None).unwrap();
        assert_eq!(
            args,
            EntryArgs::Iterated(vec![
                s(&["a", "--x", "--icon", "ic", "T", "/x/t.desktop", "%d"]),
                s(&["b", "--x", "--icon", "ic", "T", "/x/t.desktop", "%d"]),
            ])
        );
        let (_, args) = gen_entry_args(&entry("sh %U"), &s(&["/a", "b:c"]), None).unwrap();
        assert_eq!(args, EntryArgs::Single(s(&["file:///a", "b:c"])));
        assert!(gen_entry_args(&entry("sh"), &s(&["a"]), None).is_err());
        assert!(gen_entry_args(&entry("sh %f %u"), &[], None).is_err());
        assert!(gen_entry_args(&entry("sh x%F"), &[], None).is_err());
    }
}
