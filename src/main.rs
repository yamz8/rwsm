//! rwsm: a Rust rewrite of uwsm, installed as `uwsm`.
//!
//! See `docs/uwsm-parity-spec.md` for the behavior this binary must match.

mod app;
mod cli;
mod comp;
mod config;
mod dbus;
mod desktop_entry;
mod env;
mod escape;
mod misc;
mod nonprintable;
mod output;
mod select;
mod session;
mod units;
mod varnames;
mod xdg;

use std::process::ExitCode;

use clap::Parser;

use cli::{AuxAction, Checker, Cli, Mode};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    let argv0 = args.remove(0);
    let terminal_opts = cli::extract_terminal_opts(&mut args);
    let cli = Cli::parse_from(std::iter::once(argv0).chain(args));

    // The readiness helper is forked off by `aux exec`, not by the manager.
    if let Mode::Aux {
        action:
            AuxAction::Autoready {
                main_pid,
                wm_id,
                id_unit_string,
            },
    } = &cli.mode
    {
        return ExitCode::from(session::autoready(*main_pid, wm_id, id_unit_string));
    }

    if let Some(w) = output::debug_warning() {
        output::warning(w);
    }

    if let Mode::Aux { .. } = cli.mode {
        output::set_prefix(true);
    }
    if let Mode::Aux { .. } = cli.mode
        && !called_by_systemd_manager()
    {
        output::error("Aux actions can only be run by systemd user manager!");
        return ExitCode::from(1);
    }

    let result = match cli.mode {
        Mode::Select => Ok(select::select_main()),
        Mode::Start(a) => session::start(a),
        Mode::Stop(a) => session::stop(a),
        Mode::Finalize(a) => session::finalize(a.env_names),
        Mode::Check {
            checker: Checker::IsActive { wm, verbose },
        } => session::check_is_active(wm, verbose),
        Mode::Aux { action } => session::aux(action),
        Mode::Check {
            checker: Checker::MayStart(a),
        } => session::check_may_start(a),
        Mode::App(a) => Ok(app::app_main(a, terminal_opts)),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            output::error(&format!("{e:#}"));
            ExitCode::from(1)
        }
    }
}

/// Aux actions are only valid when the parent is the systemd user manager
/// (spec §2.7, P-AUX-GUARD).
fn called_by_systemd_manager() -> bool {
    let manager_pid: u32 = std::env::var("MANAGERPID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    manager_pid != 0 && manager_pid == std::os::unix::process::parent_id()
}
