//! Session lifecycle commands: start, stop, finalize, check is-active and the
//! aux actions run by units (spec §4, §8.3, §8.4, §8.7).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::cli::{self, AuxAction, StartArgs, StopArgs, WmMeta};
use crate::comp::{CompInput, CompMode, fill_comp_globals};
use crate::config::{PKG_LIBEXEC_DIR, SH_BIN};
use crate::dbus::{Bus, Level};
use crate::env::{self, Env};
use crate::escape::{shlex_join, simple_systemd_escape};
use crate::misc::str2bool_plus;
use crate::units::UnitFiles;
use crate::varnames::Varnames;
use crate::{debug, output, xdg};

/// Exit code for a finished command.
pub type Code = u8;

type NameDesc = (String, String);

/// Port of `wait_for_unit` (spec §8.3).
pub fn wait_for_unit(
    unit: &str,
    bus: &Bus,
    timeout: i64,
    states: &[&str],
    quiet: bool,
) -> Result<bool> {
    if !bus.list_units_by_patterns(states, &[unit])?.is_empty() {
        return Ok(true);
    }
    if timeout == 0 {
        return Ok(false);
    }
    let job_type = if states.iter().any(|s| *s == "active" || *s == "activating") {
        "start"
    } else {
        "stop"
    };
    let mut seen = false;
    let mut spacer = "";
    let mut attempt = timeout;
    while attempt >= 0 {
        let queued = bus
            .list_jobs()?
            .iter()
            .any(|j| j.unit == unit && j.job_type == job_type);
        if !queued {
            break;
        }
        seen = true;
        if !quiet && attempt == timeout {
            output::normal(&format!(
                "{unit} is queued for start, waiting for {timeout}s..."
            ));
        } else if !quiet && (attempt % 10 == 0 || attempt == 15 || attempt < 10) {
            output::normal_inline(&format!("{spacer}{attempt}"));
            spacer = " ";
        }
        if attempt != 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        attempt -= 1;
    }
    if !quiet && seen {
        output::normal("");
    }
    if bus.list_units_by_patterns(states, &[unit])?.is_empty() {
        if !quiet && seen {
            output::warning("Timed out.");
        }
        return Ok(false);
    }
    Ok(true)
}

const GENERIC_UNITS: [&str; 5] = [
    "graphical-session-pre.target",
    "wayland-session-pre@*.target",
    "graphical-session.target",
    "wayland-session@*.target",
    "wayland-wm@*.service",
];

/// What [`is_active`] checks.
pub enum ActiveCheck<'a> {
    Generic,
    CompositorOnly,
    Id(&'a str),
}

/// Port of `is_active` (spec §8.4).
pub fn is_active(check: ActiveCheck, verbose: bool, verbose_active: bool) -> Result<bool> {
    let bus = Bus::session()?;
    let specific;
    let patterns: Vec<&str> = match check {
        ActiveCheck::Generic => GENERIC_UNITS.to_vec(),
        ActiveCheck::CompositorOnly => vec!["wayland-wm@*.service"],
        ActiveCheck::Id(id) => {
            specific = format!("wayland-wm@{}.service", simple_systemd_escape(id, false));
            vec![specific.as_str()]
        }
    };
    let split = |patterns: &[&str]| -> Result<(Vec<NameDesc>, Vec<NameDesc>)> {
        let units = bus.list_units_by_patterns(&[], patterns)?;
        Ok(units
            .into_iter()
            .fold((vec![], vec![]), |(mut a, mut i), u| {
                if u.active_state == "active" || u.active_state == "activating" {
                    a.push((u.name, u.description));
                } else {
                    i.push((u.name, u.description));
                }
                (a, i)
            }))
    };
    let (active, inactive) = split(&patterns)?;
    if !active.is_empty() {
        if verbose || verbose_active {
            output::normal(&format!(
                "Matched {} units, {} active:",
                active.len() + inactive.len(),
                active.len()
            ));
            for (n, d) in &active {
                output::normal(&format!("  {n} ({d})"));
            }
            if verbose {
                if !inactive.is_empty() {
                    output::normal(&format!("{} inactive:", inactive.len()));
                }
                for (n, d) in &inactive {
                    output::normal(&format!("  {n} ({d})"));
                }
            }
        }
        return Ok(true);
    }
    if !verbose {
        return Ok(false);
    }
    let (active, inactive) = split(&GENERIC_UNITS)?;
    output::normal("No units matched. Listing other relevant units:");
    if !active.is_empty() {
        output::normal(&format!("  {} active:", active.len()));
        for (n, d) in &active {
            output::normal(&format!("    {n} ({d})"));
        }
    }
    if !inactive.is_empty() {
        output::normal(&format!("  {} inactive:", inactive.len()));
        for (n, d) in &inactive {
            output::normal(&format!("    {n} ({d})"));
        }
    }
    Ok(false)
}

/// Unescaped compositor ID from a `wayland-wm@<id>.service` unit name.
pub fn extract_wm_id(unit: &str) -> String {
    let Some(inst) = unit
        .split_once('@')
        .and_then(|(_, rest)| rest.strip_suffix(".service"))
    else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let raw = inst.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\'
            && raw.get(i + 1) == Some(&b'x')
            && let Some(Ok(b)) = raw
                .get(i + 2..i + 4)
                .map(|h| u8::from_str_radix(std::str::from_utf8(h).unwrap_or("zz"), 16))
        {
            bytes.push(b);
            i += 4;
            continue;
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Port of `get_active_wm_unit`.
pub fn get_active_wm_unit(bus: &Bus, active: bool, activating: bool) -> Result<String> {
    let mut states = Vec::new();
    if active {
        states.push("active");
    }
    if activating {
        states.push("activating");
    }
    let units = bus.list_units_by_patterns(&states, &["wayland-wm@*.service"])?;
    if units.len() > 1 {
        output::warning(&format!(
            "Got more than 1 active wayland-wm@*.service: {}",
            units
                .iter()
                .map(|u| u.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(units.into_iter().next().map(|u| u.name).unwrap_or_default())
}

fn select_default(cmdline0: &str, dry_run: bool) -> Result<String> {
    crate::select::select_or_default(cmdline0 == "default", dry_run)
}

/// `uwsm start` (spec §4.1).
pub fn start(mut args: StartArgs) -> Result<Code> {
    output::set_log(!args.dry_run && !args.only_generate);

    let silent_raw = std::env::var("UWSM_SILENT_START").unwrap_or_else(|_| "0".into());
    let silent = match str2bool_plus(&silent_raw) {
        Ok(v) => v.as_int(),
        Err(_) => {
            output::warning(&format!(
                "Expected boolean or numeric or empty value for UWSM_SILENT_START, got \"{silent_raw}\", assuming False"
            ));
            0
        }
    };
    output::set_silent(silent > 0, silent > 1);

    let (rung, warn) = cli::resolve_unit_rung(
        args.unit_rung,
        std::env::var("UWSM_UNIT_RUNG").ok().as_deref(),
    );
    if let Some(w) = warn {
        output::warning(&w);
    }
    let (tweaks, warn) = cli::resolve_tweaks(&args, std::env::var("UWSM_TWEAKS").ok().as_deref());
    if let Some(w) = warn {
        output::warning(&w);
    }

    let (warn_s, abort_s) = (args.gst_warn_seconds(), args.gst_abort_seconds());
    if (warn_s >= 0 || abort_s >= 0) && !args.dry_run && !args.only_generate {
        let timeout = if abort_s >= 0 { abort_s } else { warn_s };
        let reached = Bus::get(Level::System).and_then(|bus| {
            wait_for_unit(
                "graphical.target",
                bus,
                timeout,
                &["active", "activating"],
                false,
            )
        });
        match reached {
            Ok(true) => {}
            Ok(false) if abort_s >= 0 => {
                output::error("System has not reached graphical.target. Aborting.");
                return Ok(1);
            }
            Ok(false) => {
                if !output::no_warn() {
                    output::warning(
                        "System has not reached graphical.target.\n\
                         It might be a good idea to check default system target or screen for this\n\
                         with a condition, i.e. via \"uwsm check may-start\".\n\
                         Will continue in 5 seconds...\n",
                    );
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
            Err(e) => {
                output::error("Could not check if graphical.target is reached!");
                output::error(&format!("{e:#}"));
                return Ok(1);
            }
        }
    }

    if args.wm_cmdline[0] == "select" || args.wm_cmdline[0] == "default" {
        match select_default(&args.wm_cmdline[0], args.dry_run) {
            Ok(id) if !id.is_empty() => args.wm_cmdline = vec![id],
            Ok(_) => {
                output::error("No compositor was selected!");
                return Ok(1);
            }
            Err(e) => {
                output::error(&format!("{e:#}"));
                return Ok(1);
            }
        }
    }

    let comp = fill_comp_globals(CompInput {
        mode: CompMode::Start,
        wm_cmdline: &args.wm_cmdline,
        wm_id: None,
        meta: &args.meta,
        hardcode: args.hardcode,
    })?;

    // uwsm's dedent() trims the end of the block but keeps one newline.
    let summary = format!(
        "Selected compositor ID: {}\n          Command Line: {}\n      Plugin/binary ID: {}\n Initial Desktop Names: {}\n                  Name: {}\n           Description: {}",
        comp.id,
        shlex_join(&comp.cmdline),
        comp.bin_id,
        comp.desktop_names.join(":"),
        comp.name,
        comp.description.as_deref().unwrap_or("None")
    );
    output::normal(&format!("{}\n", summary.trim_end()));

    if is_active(ActiveCheck::Generic, false, true)? {
        output::error("A compositor or graphical-session* target is already active!");
        if !args.dry_run {
            return Ok(1);
        }
        output::ok("...but this is dry run, so the dream continues.");
    }

    let mut units = UnitFiles::new(args.dry_run);
    units.generate_dropins(&comp, env::get_waitenv_timeout(), rung)?;
    if tweaks {
        units.generate_tweaks(rung)?;
    } else {
        units.remove_marked(&["tweaks".into()], rung)?;
    }
    units.remove_marked(&[], rung.other())?;
    if units.changed {
        units.reload_systemd()?;
    } else {
        output::normal("Units unchanged.");
    }

    if args.only_generate {
        output::warning("Only unit creation was requested. Will not go further.");
        return Ok(0);
    }
    if args.dry_run {
        output::normal(&format!("Will start {}...", comp.id));
        output::warning("Dry Run Mode. Will not go further.");
        return Ok(0);
    }

    output::normal(&format!(
        "Starting {} and waiting while it is running...",
        comp.id
    ));
    let status = Command::new("systemctl")
        .args([
            "--user",
            "start",
            &format!("wayland-session-bindpid@{}.service", std::process::id()),
        ])
        .status()?;
    if !status.success() {
        bail!("Starting bindpid unit failed: {status}");
    }

    let login: Vec<(String, String)> = std::env::vars().collect();
    env::save_env("env_login", login.iter().map(|(k, v)| (k, v)), "\0")?;
    env::save_session_env(|n| std::env::var(n).ok())?;

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

    // Keep the terminal reachable past systemd-cat for signal-handler.sh.
    unsafe {
        libc::dup2(1, 3);
        libc::dup2(2, 4);
    }
    let err = Command::new("systemd-cat")
        .args(["--identifier=uwsm", "--stderr-priority=err", "--"])
        .arg(sh)
        .arg(format!("{PKG_LIBEXEC_DIR}/signal-handler.sh"))
        .arg(format!(
            "wayland-session-envelope@{}.target",
            comp.id_unit_string
        ))
        .env(
            "UWSM_SH_NO_STDOUT",
            if output::no_stdout() { "1" } else { "" },
        )
        .env("UWSM_SH_NO_WARN", if output::no_warn() { "1" } else { "" })
        .exec();
    Err(err).context("Could not exec systemd-cat")
}

/// `uwsm stop` (spec §4.7).
pub fn stop(args: StopArgs) -> Result<Code> {
    output::set_log(!args.dry_run);
    let (rung, warn) = cli::resolve_unit_rung(
        args.unit_rung,
        std::env::var("UWSM_UNIT_RUNG").ok().as_deref(),
    );
    if let Some(w) = warn {
        output::warning(&w);
    }

    let code = match stop_wm(args.dry_run) {
        Ok(_) => 0,
        Err(e) => {
            output::error(&format!("{e:#}"));
            1
        }
    };

    if let Some(marks) = args.remove_units {
        let mut units = UnitFiles::new(args.dry_run);
        let marks: Vec<String> = marks
            .map(|m| m.split(',').map(String::from).collect())
            .unwrap_or_default();
        units.remove_marked(&marks, rung)?;
        if units.changed {
            if let Err(e) = units.reload_systemd() {
                output::warning(&format!("{e:#}"));
            }
        } else {
            output::normal("Units unchanged.");
        }
    }
    Ok(code)
}

fn stop_wm(dry_run: bool) -> Result<bool> {
    output::normal("Stopping compositor...");
    let bus = Bus::session()?;
    let units: Vec<String> = bus
        .list_units_by_patterns(&["active", "activating"], &["wayland-wm@*.service"])?
        .into_iter()
        .map(|u| u.name)
        .collect();
    let Some(unit) = units.first() else {
        output::ok("Compositor is not running.");
        return Ok(false);
    };
    if units.len() > 1 {
        output::warning(&format!(
            "Multiple compositor units found: {}!",
            units.join(", ")
        ));
    }
    if dry_run {
        output::normal(&format!("Will stop compositor {unit}."));
        return Ok(true);
    }
    output::normal(&format!("Found running compositor {unit}."));
    let (mode, warn) =
        cli::resolve_stop_job_mode(std::env::var("UWSM_STOP_JOB_MODE").ok().as_deref());
    if let Some(w) = warn {
        output::warning(&w);
    }
    let job = bus.stop_unit(unit, mode)?;
    bus.wait_for_job(&job)?;
    output::ok("Sent stop job.");
    Ok(true)
}

fn exec_systemd_notify(args: &[&str]) -> anyhow::Error {
    let err = Command::new("systemd-notify").args(args).exec();
    anyhow::Error::new(err).context("Could not exec systemd-notify")
}

/// Declares the compositor unit ready, or only restricts notification access
/// if it is already active. Shared by finalize and autoready.
fn notify_ready(
    bus: &Bus,
    wm_unit: &str,
    wm_id: &str,
    activating: bool,
    prefix: &str,
) -> Result<Code> {
    if activating {
        output::normal(&format!(
            "{prefix}Declaring unit for {wm_id} ready and restricting notification access."
        ));
        return Err(exec_systemd_notify(&["READY=1", "NOTIFYACCESS=exec"]));
    }
    if bus.get_unit_property_string(wm_unit, "NotifyAccess", true)? == "all" {
        output::normal(&format!(
            "{prefix}Unit for {wm_id} is already active, restricting notification access."
        ));
        return Err(exec_systemd_notify(&["NOTIFYACCESS=exec"]));
    }
    output::normal(&format!(
        "{prefix}Unit for {wm_id} is already active, notification access is restricted."
    ));
    Ok(0)
}

/// `uwsm finalize` (spec §4.8).
pub fn finalize(env_names: Vec<String>) -> Result<Code> {
    output::set_prefix(true);
    let result = (|| -> Result<Code> {
        let mut names = env_names;
        names.extend(
            std::env::var("UWSM_FINALIZE_VARNAMES")
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from),
        );
        if std::env::var("WAYLAND_DISPLAY")
            .unwrap_or_default()
            .is_empty()
        {
            bail!(
                "WAYLAND_DISPLAY is not defined or empty. Are we being run by a wayland compositor or not?"
            );
        }
        let extra: BTreeSet<String> = names.into_iter().collect();
        let mut export = Env::new();
        for item in ["WAYLAND_DISPLAY".to_string(), "DISPLAY".to_string()]
            .into_iter()
            .chain(extra)
        {
            let (name, value) = match item.split_once('=') {
                Some((n, v)) => {
                    if env::filter_list([n]).is_empty() {
                        continue;
                    }
                    if n == "WAYLAND_DISPLAY" || n == "DISPLAY" {
                        output::warning(&format!(
                            "Refusing to override {n}, will take from environment"
                        ));
                        continue;
                    }
                    (n.to_string(), Some(v.to_string()))
                }
                None => {
                    if env::filter_list([item.as_str()]).is_empty() {
                        continue;
                    }
                    let value = std::env::var(&item).ok();
                    (item, value)
                }
            };
            if let Some(v) = value {
                export.entry(name).or_insert(v);
            }
        }

        let bus = Bus::session()?;
        let wm_unit = get_active_wm_unit(bus, true, true)?;
        let wm_id = extract_wm_id(&wm_unit);
        let activating_unit = get_active_wm_unit(bus, false, true)?;
        let activating_id = extract_wm_id(&activating_unit);
        if wm_id.is_empty() {
            output::error(
                "Finalization: Could not get ID of active or activating Wayland session. If it is in activating state, it will time out in 30 seconds (by default).",
            );
            return Ok(1);
        }
        if !activating_id.is_empty() && wm_id != activating_id {
            output::error(&format!(
                "Finalization: Unit conflict, active: \"{wm_id}\", but another is activating: \"{activating_id}\"!"
            ));
            return Ok(1);
        }

        let mut varnames = Varnames::default();
        if let Err(e) =
            env::append_to_cleanup_file(&mut varnames, export.keys().cloned(), false, false)
        {
            output::error(&format!("{e:#} Assuming env preloader failed."));
            return Ok(1);
        }
        env::set_systemd_vars(bus, &export, false, true)?;
        notify_ready(bus, &wm_unit, &wm_id, !activating_id.is_empty(), "")
    })();
    match result {
        Ok(code) => Ok(code),
        Err(e) => {
            output::error_notify(&format!("{e:#}"), output::Notify::IfNotTty);
            Ok(0)
        }
    }
}

/// `uwsm check is-active` (spec §8.7).
pub fn check_is_active(wm: Option<String>, verbose: bool) -> Result<Code> {
    if std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .unwrap_or_default()
        .is_empty()
    {
        output::error("DBUS_SESSION_BUS_ADDRESS is not available");
        return Ok(1);
    }
    let check = match wm.as_deref() {
        Some(id) if !id.is_empty() => ActiveCheck::Id(id),
        _ => ActiveCheck::Generic,
    };
    match is_active(check, verbose, false) {
        Ok(true) => Ok(0),
        Ok(false) => Ok(1),
        Err(e) => {
            output::error(&format!("{e:#}"));
            Ok(1)
        }
    }
}

/// Dispatches `uwsm aux ...` (spec §4.9–4.13).
pub fn aux(action: AuxAction) -> Result<Code> {
    output::set_prefix(true);
    match action {
        AuxAction::PrepareEnv {
            meta,
            wm_id,
            wm_cmdline,
        } => {
            let comp = fill_comp_globals(CompInput {
                mode: CompMode::PrepareEnv,
                wm_cmdline: &wm_cmdline,
                wm_id: Some(&wm_id),
                meta: &meta,
                hardcode: false,
            })?;
            if let Err(e) = env::prepare_env(&comp) {
                output::error(&format!("{e:#}"));
                if let Err(e2) = env::cleanup_env() {
                    output::error(&format!("{e2:#}"));
                }
                return Ok(1);
            }
            Ok(0)
        }
        AuxAction::CleanupEnv => {
            if is_active(ActiveCheck::CompositorOnly, false, true)? {
                output::error("A compositor is running, will not cleanup environment!");
                return Ok(1);
            }
            match env::cleanup_env() {
                Ok(()) => Ok(0),
                Err(e) => {
                    output::error(&format!("{e:#}"));
                    Ok(1)
                }
            }
        }
        AuxAction::Exec { wm_id, wm_cmdline } => aux_exec(&wm_id, &wm_cmdline),
        AuxAction::Waitpid { pid } => {
            waitpid(pid)?;
            Ok(0)
        }
        AuxAction::Waitenv { env_names } => {
            let timeout = Bus::session()
                .and_then(|b| {
                    b.get_unit_property_u64(
                        "wayland-session-waitenv.service",
                        "TimeoutStartUSec",
                        true,
                    )
                })
                .map(|us| us / 1_000_000)
                .ok();
            debug!("waitenv timeout from unit: {timeout:?}");
            let names = wait_names(env_names);
            env::waitenv(&names, timeout)?;
            std::thread::sleep(env::settle_time(""));
            Ok(0)
        }
        AuxAction::AppDaemon => crate::app::app_daemon(),
        AuxAction::Autoready { .. } => unreachable!("handled before the aux guard"),
    }
}

fn wait_names(extra: Vec<String>) -> Vec<String> {
    std::iter::once("WAYLAND_DISPLAY".to_string())
        .chain(extra)
        .chain(
            std::env::var("UWSM_WAIT_VARNAMES")
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from),
        )
        .collect()
}

fn waitpid(pid: i32) -> Result<()> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            output::normal(&format!("Process with PID {pid} not found."));
            return Ok(());
        }
        return Err(err).context("pidfd_open failed");
    }
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let r = unsafe { libc::poll(&mut pfd, 1, -1) };
        if r >= 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err).context("poll failed");
        }
    }
    unsafe { libc::close(fd) };
    output::normal(&format!("Process with PID {pid} has ended."));
    Ok(())
}

/// `uwsm aux exec` (spec §4.11): starts the autoready helper, then execs the
/// compositor in place.
fn aux_exec(wm_id: &str, wm_cmdline: &[String]) -> Result<Code> {
    let meta = WmMeta::default();
    let comp = match fill_comp_globals(CompInput {
        mode: CompMode::Exec,
        wm_cmdline,
        wm_id: Some(wm_id),
        meta: &meta,
        hardcode: false,
    }) {
        Ok(c) => c,
        Err(e) => {
            output::error(&format!("{e:#}"));
            return Ok(1);
        }
    };
    output::normal(&format!("Starting: {}...", shlex_join(&comp.cmdline)));

    let env_pre = env::filter(Bus::session()?.get_systemd_vars()?);
    let payload: String = env_pre
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\0");

    // zbus has started a thread, so forking here is unsafe. Instead run a
    // helper that forks itself before doing anything else; it reports the
    // pre-start environment over stdin and its intermediate process exits at
    // once, so the leaf is reparented away from the compositor.
    let mut helper = Command::new("/proc/self/exe")
        .args(["aux", "autoready", "--main-pid"])
        .arg(std::process::id().to_string())
        .arg("--")
        .arg(&comp.id)
        .arg(&comp.id_unit_string)
        .stdin(Stdio::piped())
        .spawn()
        .context("Could not start autoready helper")?;
    if let Some(mut stdin) = helper.stdin.take() {
        stdin.write_all(payload.as_bytes())?;
    }
    helper.wait()?;

    let err = Command::new(&comp.cmdline[0])
        .args(&comp.cmdline[1..])
        .exec();
    output::error(&format!("{err}"));
    Ok(1)
}

/// Hidden `aux autoready` helper. Runs before any thread exists: reads the
/// pre-start environment, double-forks, and the leaf waits for the compositor
/// to export its variables and declares the unit ready.
pub fn autoready(main_pid: i32, wm_id: &str, id_unit_string: &str) -> Code {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return 1;
    }
    match unsafe { libc::fork() } {
        -1 => return 1,
        0 => {}
        _ => unsafe { libc::_exit(0) },
    }
    output::set_prefix(true);
    let env_pre: Env = crate::misc::sane_split(&raw, "\0")
        .into_iter()
        .filter_map(|i| {
            i.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    match autoready_leaf(main_pid, wm_id, id_unit_string, &env_pre) {
        Ok(code) => code,
        Err(e) => {
            output::warning(&format!("Autoready failed: {e:#}"));
            1
        }
    }
}

fn autoready_leaf(main_pid: i32, wm_id: &str, id_unit_string: &str, env_pre: &Env) -> Result<Code> {
    let bus = Bus::session()?;
    let unit = format!("wayland-wm@{id_unit_string}.service");
    let timeout = bus
        .get_unit_property_u64(&unit, "TimeoutStartUSec", true)
        .map(|us| us / 1_000_000)
        .ok();
    env::waitenv(&wait_names(Vec::new()), timeout)?;
    std::thread::sleep(env::settle_time("Autoready: "));

    let env_post = env::filter(bus.get_systemd_vars()?);
    let delta: Env = env_post
        .into_iter()
        .filter(|(k, v)| env_pre.get(k) != Some(v))
        .collect();
    if !delta.is_empty() {
        env::set_systemd_vars(bus, &delta, true, true)?;
        let mut varnames = Varnames::default();
        if let Err(e) =
            env::append_to_cleanup_file(&mut varnames, delta.keys().cloned(), true, true)
        {
            output::error(&format!("Autoready: Assuming env preloader failed: {e:#}"));
            unsafe { libc::kill(main_pid, libc::SIGTERM) };
            return Ok(1);
        }
    }
    let activating = !get_active_wm_unit(bus, false, true)?.is_empty();
    notify_ready(bus, &unit, wm_id, activating, "Autoready: ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wm_id_from_unit() {
        assert_eq!(
            extract_wm_id("wayland-wm@sway.desktop.service"),
            "sway.desktop"
        );
        assert_eq!(
            extract_wm_id("wayland-wm@niri\\x2dsession.service"),
            "niri-session"
        );
        assert_eq!(extract_wm_id(""), "");
    }
}

/// Outcome of the may-start checks: errors, visible and silent dealbreakers.
#[derive(Default)]
struct MayStart {
    errors: Vec<String>,
    visible: Vec<String>,
    silent: Vec<String>,
}

impl MayStart {
    fn add(list: &mut Vec<String>, msg: String) {
        if !list.contains(&msg) {
            list.push(msg);
        }
    }

    fn error(&mut self, msg: &str, e: impl std::fmt::Display) {
        // uwsm adds these with set.update(), scattering the characters (spec §8.6).
        Self::add(&mut self.errors, msg.to_string());
        Self::add(&mut self.errors, e.to_string());
    }

    fn silent(&mut self, msg: String) {
        Self::add(&mut self.silent, msg);
    }
}

fn check_may_start_inner(args: &cli::MayStartArgs) -> MayStart {
    let mut r = MayStart::default();
    let verbose = args.verbose;

    if unsafe { libc::geteuid() } == 0 {
        r.silent("Running as root".into());
        return r;
    }
    if std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .unwrap_or_default()
        .is_empty()
    {
        r.silent("DBUS_SESSION_BUS_ADDRESS is not available".into());
        return r;
    }

    if !args.nologin {
        let ppid = std::os::unix::process::parent_id();
        match std::fs::read(format!("/proc/{ppid}/cmdline")) {
            Ok(raw) => {
                let cmdline = String::from_utf8_lossy(&raw);
                debug!("parent_pid: {ppid}");
                debug!("parent_cmdline: {}", cmdline.trim());
                if !cmdline.trim().starts_with('-') {
                    r.silent("Not in login shell".into());
                    if !verbose {
                        return r;
                    }
                }
            }
            Err(e) => {
                r.error("Could not determine parent process command!", e);
                return r;
            }
        }
    }

    match is_active(ActiveCheck::Generic, false, false) {
        Ok(true) => {
            MayStart::add(
                &mut r.visible,
                "A compositor and/or graphical-session* targets are already active".into(),
            );
            if !verbose {
                return r;
            }
        }
        Ok(false) => {}
        Err(e) => {
            r.error("Could not check for active compositor!", format!("{e:#}"));
            return r;
        }
    }

    let allowed = args.allowed_vtnr();
    let allowed_str = allowed
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join("|");
    debug!("allowed_vtnr {allowed:?}");
    let check_vt = allowed != [0];
    if check_vt {
        match env::get_fg_vt() {
            None => {
                r.silent("Could not determine foreground VT".into());
                if !verbose {
                    return r;
                }
            }
            Some(fgvt) if !allowed.contains(&fgvt) => {
                r.silent(format!(
                    "Foreground VT ({fgvt}) is not among allowed VTs ({allowed_str})"
                ));
                if !verbose {
                    return r;
                }
            }
            Some(_) => {}
        }
    }

    let session_id = std::env::var("XDG_SESSION_ID").unwrap_or_default();
    if check_vt {
        if session_id.is_empty() {
            r.silent("XDG_SESSION_ID is not available".into());
            if !verbose {
                return r;
            }
        } else {
            match Bus::system().and_then(|bus| bus.get_session_property_u32(&session_id, "VTNr")) {
                Ok(0) => {
                    r.silent(format!("Session {session_id} is not associated with a VT"));
                    if !verbose {
                        return r;
                    }
                }
                Ok(vtnr) => {
                    debug!("session_vtnr {vtnr}");
                    if !allowed.contains(&vtnr) {
                        r.silent(format!(
                            "Session VT ({vtnr}) is not among allowed VTs ({allowed_str})"
                        ));
                        if !verbose {
                            return r;
                        }
                    }
                }
                Err(e) => {
                    r.error("Could not get session VTNr!", format!("{e:#}"));
                    return r;
                }
            }
        }
    }

    if !args.allow_remote {
        if session_id.is_empty() {
            r.silent("XDG_SESSION_ID is not available".into());
            if !verbose {
                return r;
            }
        } else {
            match Bus::system().and_then(|bus| bus.get_session_property_bool(&session_id, "Remote"))
            {
                Ok(true) => {
                    r.silent(format!("Session {session_id} is not local"));
                    if !verbose {
                        return r;
                    }
                }
                Ok(false) => {}
                Err(e) => {
                    r.error("Could not get session Remote attr!", format!("{e:#}"));
                    return r;
                }
            }
        }
    }

    if args.gst_seconds > 0 {
        let reached = Bus::system().and_then(|bus| {
            wait_for_unit(
                "graphical.target",
                bus,
                args.gst_seconds,
                &["active", "activating"],
                args.quiet,
            )
        });
        match reached {
            Ok(true) => {}
            Ok(false) => {
                r.silent("System has not reached graphical.target".into());
                if !verbose {
                    return r;
                }
            }
            Err(e) => {
                r.error(
                    "Could not check if graphical.target is reached!",
                    format!("{e:#}"),
                );
                return r;
            }
        }
    }
    r
}

/// `uwsm check may-start` (spec §8.6).
pub fn check_may_start(args: cli::MayStartArgs) -> Result<Code> {
    let r = check_may_start_inner(&args);
    if r.errors.is_empty() && r.visible.is_empty() && r.silent.is_empty() {
        if args.verbose {
            output::ok("May start compositor.");
        }
        return Ok(0);
    }
    if !r.errors.is_empty() {
        let mut lines = vec!["Got errors while checking if may start compositor:".to_string()];
        lines.extend(r.errors);
        output::error(&lines.join("\n"));
    }
    if r.visible.is_empty() && r.silent.is_empty() {
        return Ok(1);
    }
    if args.verbose {
        let mut lines = vec!["May not start compositor:".to_string()];
        lines.extend(r.visible);
        lines.extend(r.silent);
        output::warning(&lines.join("\n  "));
    } else if !args.quiet && !r.visible.is_empty() {
        output::warning(&r.visible.join("\n"));
    }
    Ok(1)
}
