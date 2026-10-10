//! Environment management: runtime env files, activation environment export
//! and cleanup, the environment preloader, finalize and waitenv (spec §4.8,
//! §4.11–4.13, §5).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::comp::Comp;
use crate::config::{BIN_NAME, PKG_LIBEXEC_DIR, SH_BIN};
use crate::dbus::Bus;
use crate::escape::shlex_quote;
use crate::misc::sane_split;
use crate::varnames::{SESSION_SPECIFIC, Varnames, filter_env, filter_names};
use crate::{debug, output, xdg};

pub type Env = BTreeMap<String, String>;

fn warn_illegal(names: &[String]) {
    for name in names {
        output::warning(&format!("Encountered illegal var \"{name}\"."));
    }
}

/// Filters an environment map, warning about illegal names.
pub fn filter(env: Env) -> Env {
    let (kept, illegal) = filter_env(env);
    warn_illegal(&illegal);
    kept
}

/// Filters a list of names, warning about illegal ones.
pub fn filter_list<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let (kept, illegal) = filter_names(names);
    warn_illegal(&illegal);
    kept
}

/// Varname sets with `UWSM_*_VARNAMES` from the process environment applied.
pub fn varnames_from_process_env() -> Varnames {
    let mut v = Varnames::default();
    update_varnames(&mut v, |n| std::env::var(n).ok());
    v
}

/// Port of `update_varnames_sets`, printing what changed.
pub fn update_varnames(v: &mut Varnames, lookup: impl Fn(&str) -> Option<String>) {
    for change in v.update_from_env(lookup) {
        if !change.added.is_empty() {
            output::normal(&format!(
                "Adding to {} set: {}",
                change.set_name,
                change.added.join(", ")
            ));
        }
        if !change.removed.is_empty() {
            output::normal(&format!(
                "Removing from {} set: {}",
                change.set_name,
                change.removed.join(", ")
            ));
        }
    }
}

fn runtime_file(name: &str) -> Result<PathBuf> {
    Ok(xdg::uwsm_runtime_dir()?.join(name))
}

/// Port of `save_env`: `K=V` joined by `separator` (plus a trailing newline
/// when the separator is a newline). Values are not escaped.
pub fn save_env<'a, I>(name: &str, env: I, separator: &str) -> Result<()>
where
    I: IntoIterator<Item = (&'a String, &'a String)>,
{
    let pairs: Vec<String> = env
        .into_iter()
        .filter(|(k, _)| match crate::varnames::check_varname(k) {
            crate::varnames::VarnameCheck::Keep => true,
            crate::varnames::VarnameCheck::Drop => false,
            crate::varnames::VarnameCheck::Illegal => {
                output::warning(&format!("Encountered illegal var \"{k}\"."));
                false
            }
        })
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if pairs.is_empty() {
        debug!("nothing to write");
        return Ok(());
    }
    let path = runtime_file(name)?;
    fs::create_dir_all(path.parent().expect("runtime file has a parent"))?;
    let mut data = pairs.join(separator);
    if separator == "\n" {
        data.push('\n');
    }
    fs::write(&path, data).with_context(|| format!("Could not write {}", path.display()))?;
    debug!("written {}", path.display());
    Ok(())
}

/// Port of `load_env`: reads a NUL-separated runtime env file. Parse errors
/// only warn.
pub fn load_env(name: &str, delete: bool) -> Result<Env> {
    let path = runtime_file(name)?;
    if !path.is_file() {
        return Ok(Env::new());
    }
    let mut env = Env::new();
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let mut bad = None;
            for item in sane_split(&raw, "\0") {
                match item.split_once('=') {
                    Some((k, v)) => {
                        env.insert(k.to_string(), v.to_string());
                    }
                    None => bad = Some(item.to_string()),
                }
            }
            if let Some(item) = bad {
                output::warning(&format!(
                    "Could not read or parse \"{}\": bad item \"{item}\"",
                    path.display()
                ));
                env.clear();
            } else {
                env = filter(env);
            }
        }
        Err(e) => output::warning(&format!(
            "Could not read or parse \"{}\": {e}",
            path.display()
        )),
    }
    if delete {
        fs::remove_file(&path)?;
        debug!("removed {}", path.display());
    }
    Ok(env)
}

/// Saves the set session-specific vars for units' `EnvironmentFile=`.
pub fn save_session_env(lookup: impl Fn(&str) -> Option<String>) -> Result<()> {
    let vars: Vec<(String, String)> = SESSION_SPECIFIC
        .iter()
        .filter_map(|n| {
            lookup(n)
                .filter(|v| !v.is_empty())
                .map(|v| (n.to_string(), v))
        })
        .collect();
    save_env("env_session.conf", vars.iter().map(|(k, v)| (k, v)), "\n")
}

/// Port of `set_systemd_vars` (spec §5.4).
pub fn set_systemd_vars(bus: &Bus, vars: &Env, dbus_only: bool, verbose: bool) -> Result<()> {
    let existing = bus.get_systemd_vars()?;
    let mut msg = Vec::new();
    for (k, v) in vars {
        match existing.get(k) {
            Some(old) if old == v => {
                if verbose {
                    msg.push(format!("{k} (already set)"));
                }
            }
            Some(_) => msg.push(format!("{k} (updating)")),
            None => msg.push(k.clone()),
        }
    }
    if !verbose && msg.is_empty() {
        msg.push("(All already set)".into());
    }
    output::normal(&format!(
        "Exporting variables to systemd user manager:\n  {}",
        msg.join("\n  ")
    ));
    if !dbus_only {
        bus.set_systemd_vars(vars)?;
    } else {
        debug!("skipped managing systemd activation environment");
    }
    if !bus.is_dbus_broker()? {
        output::normal(if dbus_only {
            "Exporting vars to dbus environment (consider switching to dbus-broker)"
        } else {
            "Also exporting vars to dbus environment (consider switching to dbus-broker)"
        });
        bus.set_dbus_vars(vars)?;
    }
    Ok(())
}

/// Port of `unset_systemd_vars` (spec §5.4).
pub fn unset_systemd_vars(bus: &Bus, names: &BTreeSet<String>) -> Result<()> {
    output::normal(&format!(
        "Removing variables from systemd user manager:\n  {}",
        names.iter().cloned().collect::<Vec<_>>().join("\n  ")
    ));
    if !bus.is_dbus_broker()? {
        let empty: Env = names.iter().map(|n| (n.clone(), String::new())).collect();
        output::normal(
            "Also writing empty values to dbus environment (consider switching to dbus-broker)",
        );
        bus.set_dbus_vars(&empty)?;
    }
    let list: Vec<String> = names.iter().cloned().collect();
    bus.unset_systemd_vars(&list)
}

/// Port of `append_to_cleanup_file` (spec §5.6).
pub fn append_to_cleanup_file<I, S>(
    varnames: &mut Varnames,
    names: I,
    skip_always_cleanup: bool,
    create: bool,
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let path = runtime_file("env_cleanup.list")?;
    update_varnames(varnames, |n| std::env::var(n).ok());

    let mut names: BTreeSet<String> = filter_list(names)
        .into_iter()
        .filter(|n| !varnames.never_cleanup.contains(n))
        .collect();
    if skip_always_cleanup {
        names.retain(|n| !varnames.always_cleanup.contains(n));
    }
    if names.is_empty() {
        debug!("no varnames to write to cleanup file");
        return Ok(());
    }

    if !path.exists() {
        if !create {
            bail!("\"{}\" does not exist!", path.display());
        }
        fs::create_dir_all(path.parent().expect("has parent"))?;
        let mut data = names.iter().cloned().collect::<Vec<_>>().join("\n");
        data.push('\n');
        fs::write(&path, data)?;
    } else if !path.is_file() {
        bail!("\"{}\" is not a file!", path.display());
    } else {
        let current: BTreeSet<String> = filter_list(
            fs::read_to_string(&path)?
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from),
        )
        .into_iter()
        .collect();
        names = names.difference(&current).cloned().collect();
        if names.is_empty() {
            debug!("no new varnames to append to cleanup file");
            return Ok(());
        }
        let mut data = names.iter().cloned().collect::<Vec<_>>().join("\n");
        data.push('\n');
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(data.as_bytes())?;
    }
    output::normal(&format!(
        "Marking variables for later cleanup from systemd user manager on stop:\n  {}",
        names.into_iter().collect::<Vec<_>>().join("\n  ")
    ));
    Ok(())
}

/// Random lowercase hex string.
pub fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len.div_ceil(2)];
    if fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
            ^ u128::from(std::process::id());
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (seed >> ((i % 16) * 8)) as u8;
        }
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    hex[..len].to_string()
}

/// Foreground VT number from `/sys/class/tty/tty0/active`.
pub fn get_fg_vt() -> Option<u32> {
    let raw = match fs::read_to_string("/sys/class/tty/tty0/active") {
        Ok(r) => r,
        Err(e) => {
            output::error(&e.to_string());
            return None;
        }
    };
    let raw = raw.trim();
    let Some(num) = raw.strip_prefix("tty") else {
        output::error(&format!(
            "Reading \"/sys/class/tty/tty0/active\" returned \"{raw}\", expected \"tty[0-9]\"!"
        ));
        return None;
    };
    match num.parse() {
        Ok(n) => Some(n),
        Err(_) => {
            output::error(&format!(
                "Reading \"/sys/class/tty/tty0/active\" returned \"{raw}\", could not extract number!"
            ));
            None
        }
    }
}

fn login_name() -> Option<String> {
    let ptr = unsafe { libc::getlogin() };
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// The current user's login session on a VT, as `(session_id, seat_id)`.
pub fn get_session_by_vt(vt: u32, system: &Bus) -> Option<(String, String)> {
    let Some(login) = login_name() else {
        output::error("Could not determine login name");
        return None;
    };
    let sessions = match system.list_login_sessions_ex() {
        Ok(s) => s,
        Err(e) => {
            output::error(&format!("{e:#}"));
            return None;
        }
    };
    sessions
        .into_iter()
        .find(|s| {
            s.class == "user"
                && s.user_name == login
                && system.get_session_property_u32(&s.id, "VTNr").ok() == Some(vt)
        })
        .map(|s| (s.id, s.seat))
}

/// Port of `prepare_env` (spec §5.3).
pub fn prepare_env(comp: &Comp) -> Result<()> {
    let mut varnames = Varnames::default();
    output::normal(&format!(
        "Preparing environment for {}...",
        if comp.name.is_empty() {
            &comp.bin_name
        } else {
            &comp.name
        }
    ));
    let bus = Bus::session()?;

    let mut env_login = load_env("env_login", true)?;
    let have_env_login = !env_login.is_empty();
    if have_env_login {
        output::normal("Got saved login session variables.");
    }

    let missing = |k: &str, e: &Env| e.get(k).is_none_or(|v| v.is_empty());
    if missing("XDG_SEAT", &env_login) || missing("XDG_SESSION_ID", &env_login) {
        let vt = match env_login
            .get("XDG_VTNR")
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        {
            Some(v) => v.parse()?,
            None => {
                let vt = get_fg_vt().context("Could not determine foreground VT")?;
                output::ok(&format!("Deduced XDG_VTNR={vt} from foreground VT"));
                env_login.insert("XDG_VTNR".into(), vt.to_string());
                vt
            }
        };
        let system = Bus::system()?;
        let (session_id, seat) = get_session_by_vt(vt, system)
            .with_context(|| format!("Could not determine session on foreground VT {vt}"))?;
        output::ok(&format!(
            "Deduced XDG_SESSION_ID={session_id}, XDG_SEAT={seat}"
        ));
        env_login.insert("XDG_SESSION_ID".into(), session_id.clone());
        env_login.insert("XDG_SEAT".into(), seat);

        if bus
            .list_units_by_patterns(
                &["active", "activating"],
                &["wayland-session-bindpid@*.service"],
            )?
            .is_empty()
        {
            output::warning(&format!(
                "No running wayland-session-bindpid@*.service unit found, starting one on session {session_id} leader PID..."
            ));
            let leader = system.get_session_property_u32(&session_id, "Leader")?;
            let status = Command::new("systemctl")
                .args([
                    "--user",
                    "start",
                    &format!("wayland-session-bindpid@{leader}.service"),
                ])
                .status()?;
            if !status.success() {
                bail!("systemctl start of bindpid unit failed: {status}");
            }
        }
        let snapshot = env_login.clone();
        save_session_env(|n| snapshot.get(n).cloned())?;
    }

    let env_pre = filter(bus.get_systemd_vars()?);
    let systemd_varnames: BTreeSet<String> = env_pre.keys().cloned().collect();
    save_env("env_pre", &env_pre, "\0")?;

    let mut env_merged = env_pre.clone();
    env_merged.extend(env_login);

    let mark = random_hex(16);
    let first_dn = comp.desktop_names.first().cloned().unwrap_or_default();
    let aux_vars = format!(
        "# vars for plugins\n\
         __SELF_NAME__={}\n\
         __WM_ID__={}\n\
         __WM_ID_UNIT_STRING__={}\n\
         __WM_BIN_ID__={}\n\
         __WM_DESKTOP_NAMES__={}\n\
         __WM_FIRST_DESKTOP_NAME__={}\n\
         __WM_DESKTOP_NAMES_EXCLUSIVE__={}\n\
         __LOAD_PROFILE__={}\n\
         __OIFS__=\" \\t\\n\"\n\
         # random mark for environment dump at the end\n\
         __RANDOM_MARK__={mark}\n\
         # context marker for profile scripting\n\
         IN_UWSM_ENV_PRELOADER=true\n",
        shlex_quote(BIN_NAME),
        shlex_quote(&comp.id),
        shlex_quote(&comp.id_unit_string),
        shlex_quote(&comp.bin_id),
        shlex_quote(&comp.desktop_names.join(":")),
        shlex_quote(&first_dn),
        if comp.cli_desktop_names_exclusive {
            "true"
        } else {
            "false"
        },
        if have_env_login { "true" } else { "false" },
    );
    let aux_vars_file = xdg::uwsm_runtime_dir()?.join(format!("vars_{mark}"));
    fs::create_dir_all(aux_vars_file.parent().expect("has parent"))?;
    fs::write(&aux_vars_file, aux_vars)?;

    let plugins = xdg::load_data_paths(&format!("uwsm/plugins/{}.sh", comp.bin_id));
    let sh = xdg::which(SH_BIN).with_context(|| {
        format!(
            "\"{SH_BIN}\" is not {}!",
            if SH_BIN.contains('/') {
                "found"
            } else {
                "in PATH"
            }
        )
    })?;
    let mut cmd = Command::new(sh);
    if output::debug_enabled() {
        cmd.arg("-x");
    }
    let out = cmd
        .arg(format!("{PKG_LIBEXEC_DIR}/prepare-env.sh"))
        .arg(&aux_vars_file)
        .args(&plugins)
        .env_clear()
        .envs(&env_merged)
        .output()
        .context("Could not run environment loader")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let Some(pos) = stdout.find(&mark) else {
        if !stdout.trim().is_empty() {
            output::normal(stdout.trim());
        }
        if !stderr.trim().is_empty() {
            output::error(stderr.trim());
        }
        bail!("Env output mark \"{mark}\" not found in shell output!");
    };
    let messages = &stdout[..pos];
    let dump = stdout[pos + mark.len()..].trim_end_matches('\0');
    if !messages.trim().is_empty() {
        output::normal(messages.trim());
    }
    if !stderr.trim().is_empty() {
        output::error(stderr.trim());
    }
    if !out.status.success() {
        bail!("Shell returned {}!", out.status.code().unwrap_or(-1));
    }

    let mut env_post = Env::new();
    for item in dump.split('\0') {
        match item.split_once('=') {
            Some((k, v)) if k != "_" => {
                env_post.insert(k.to_string(), v.to_string());
            }
            _ => output::error(&format!("No value: {item:?}!")),
        }
    }
    let env_post = filter(env_post);

    let mut set_env: Env = env_post
        .iter()
        .filter(|(k, v)| env_pre.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    debug!("env_pre {env_pre:?}\nenv_post {env_post:?}\nset_env {set_env:?}");

    update_varnames(&mut varnames, |n| env_post.get(n).cloned());

    for var in &varnames.always_export {
        if varnames.never_export.contains(var) || varnames.always_unset.contains(var) {
            continue;
        }
        if let Some(v) = env_post.get(var) {
            set_env.insert(var.clone(), v.clone());
        }
    }
    for var in varnames.never_export.iter().chain(&varnames.always_unset) {
        set_env.remove(var);
    }

    let unset: BTreeSet<String> = env_pre
        .keys()
        .filter(|k| !env_post.contains_key(*k))
        .cloned()
        .chain(varnames.always_unset.iter().cloned())
        .filter(|k| systemd_varnames.contains(k))
        .collect();

    let cleanup: Vec<String> = set_env
        .keys()
        .filter(|k| !varnames.never_cleanup.contains(*k))
        .cloned()
        .collect();
    append_to_cleanup_file(&mut varnames, cleanup, false, true)?;

    set_systemd_vars(bus, &set_env, false, true)?;
    if !unset.is_empty() {
        unset_systemd_vars(bus, &unset)?;
    }
    Ok(())
}

/// Port of `cleanup_env` (spec §5.5).
pub fn cleanup_env() -> Result<()> {
    let varnames = varnames_from_process_env();
    output::normal("Cleaning up...");
    let bus = Bus::session()?;

    let cleanup_file = runtime_file("env_cleanup.list")?;
    let mut listed = BTreeSet::new();
    if cleanup_file.is_file() {
        output::normal("Found environment cleanup file \"env_cleanup.list\".");
        listed.extend(
            fs::read_to_string(&cleanup_file)?
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from),
        );
    } else {
        output::warning(&format!(
            "Environment cleanup file \"{}\" not found.",
            cleanup_file.display()
        ));
    }

    let systemd_names: BTreeSet<String> = bus.get_systemd_vars()?.into_keys().collect();
    let env_pre_file = runtime_file("env_pre")?;
    let env_pre = if env_pre_file.is_file() {
        output::normal("Found initial systemd state file \"env_pre\".");
        load_env("env_pre", false)?
    } else {
        Env::new()
    };

    let cleanup: BTreeSet<String> = listed
        .union(&varnames.always_cleanup)
        .filter(|n| !varnames.never_cleanup.contains(*n))
        .filter(|n| systemd_names.contains(*n))
        .filter(|n| !env_pre.contains_key(*n))
        .cloned()
        .collect();
    if !cleanup.is_empty() {
        unset_systemd_vars(bus, &cleanup)?;
    }
    if !env_pre.is_empty() {
        output::normal("Restoring initial systemd vars.");
        set_systemd_vars(bus, &env_pre, false, false)?;
    }

    for file in [
        cleanup_file,
        env_pre_file,
        runtime_file("env_session.conf")?,
    ] {
        if file.exists() {
            fs::remove_file(&file)?;
            output::ok(&format!(
                "Removed \"{}\".",
                file.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    Ok(())
}

/// `UWSM_WAIT_VARNAMES_TIMEOUT`, a positive integer, default 30.
pub fn get_waitenv_timeout() -> u64 {
    const DEFAULT: u64 = 30;
    let raw = std::env::var("UWSM_WAIT_VARNAMES_TIMEOUT").unwrap_or_else(|_| DEFAULT.to_string());
    if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
        match raw.parse::<u64>() {
            Ok(0) => {
                output::warning(&format!(
                    "Expected positive value from UWSM_WAIT_VARNAMES_TIMEOUT var, got: {raw}"
                ));
                DEFAULT
            }
            Ok(t) => t,
            Err(_) => DEFAULT,
        }
    } else {
        output::warning(&format!(
            "Expected integer value from UWSM_WAIT_VARNAMES_TIMEOUT var, got: {raw}"
        ));
        DEFAULT
    }
}

/// `UWSM_WAIT_VARNAMES_SETTLETIME`, default 0.2 seconds.
pub fn settle_time(prefix: &str) -> Duration {
    let raw = std::env::var("UWSM_WAIT_VARNAMES_SETTLETIME").unwrap_or_else(|_| "0.2".into());
    match raw.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && v >= 0.0 => Duration::from_secs_f64(v),
        _ => {
            output::warning(&format!(
                "{prefix}\"UWSM_WAIT_VARNAMES_SETTLETIME\" contains invalid value \"{raw}\", using \"0.2\""
            ));
            Duration::from_millis(200)
        }
    }
}

/// Port of `waitenv` (spec §8.5).
pub fn waitenv(names: &[String], timeout: Option<u64>) -> Result<()> {
    let timeout = timeout.unwrap_or_else(get_waitenv_timeout);
    let names = filter_list(names.iter().cloned());
    let wanted: BTreeSet<String> = names.iter().cloned().collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let bus = Bus::session()?;
    let step = Duration::from_millis(500);
    let start = Instant::now();
    let attempts = (timeout as f64 / 0.5) as u64;
    let mut warned = false;
    let join = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(", ");

    for attempt in 1..=attempts {
        let present: BTreeSet<String> = bus.get_systemd_vars()?.into_keys().collect();
        if wanted.is_subset(&present) {
            if attempt == 1 {
                output::ok(&format!(
                    "All expected variables appeared in activation environment:\n  {}",
                    names.join(", ")
                ));
            } else {
                output::ok("All expected variables appeared in activation environment.");
            }
            return Ok(());
        }
        let appeared: BTreeSet<String> = wanted
            .intersection(&present)
            .filter(|n| !seen.contains(*n))
            .cloned()
            .collect();
        if !appeared.is_empty() {
            seen.extend(appeared.iter().cloned());
            let still: BTreeSet<String> = wanted.difference(&seen).cloned().collect();
            output::normal(&format!(
                "Expected variables appeared in activation environment:\n  {}\nStill expecting:\n  {}",
                join(&appeared),
                join(&still)
            ));
        } else if attempt == 1 {
            output::normal(&format!(
                "Expecting variables to appear in activation environment:\n  {}",
                join(&wanted)
            ));
        } else if (attempt as f64 * 0.5) / timeout as f64 >= 0.8 && !warned {
            warned = true;
            output::warning(&format!(
                "Still waiting for variables in activation environment, nearing timeout:\n  {}",
                join(&wanted)
            ));
        }
        if start.elapsed() > Duration::from_secs(timeout) {
            break;
        }
        std::thread::sleep(step);
    }
    // Let systemd's own start timeout fire first.
    std::thread::sleep(Duration::from_secs(3));
    let missing: BTreeSet<String> = wanted.difference(&seen).cloned().collect();
    bail!(
        "Timed out waiting for variables in activation environment:\n  {}",
        join(&missing)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex() {
        let h = random_hex(16);
        assert_eq!(h.len(), 16);
        assert!(
            h.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(random_hex(7).len(), 7);
    }

    #[test]
    fn env_files_roundtrip() {
        let _g = crate::xdg::test_env::lock();
        let dir = std::env::temp_dir().join(format!("rwsm-test-{}", random_hex(8)));
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
        let mut env = Env::new();
        env.insert("A".into(), "1".into());
        env.insert("B".into(), "x=y".into());
        env.insert("_".into(), "dropped".into());
        save_env("env_login", &env, "\0").unwrap();
        let raw = fs::read_to_string(dir.join("uwsm/env_login")).unwrap();
        assert_eq!(raw, "A=1\0B=x=y");
        let loaded = load_env("env_login", true).unwrap();
        assert_eq!(loaded.get("B").map(String::as_str), Some("x=y"));
        assert!(!dir.join("uwsm/env_login").exists());

        save_session_env(|n| (n == "XDG_VTNR").then(|| "1".to_string())).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("uwsm/env_session.conf")).unwrap(),
            "XDG_VTNR=1\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
