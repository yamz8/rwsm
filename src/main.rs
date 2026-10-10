//! rwsm: a Rust rewrite of uwsm, installed as `uwsm`.
//!
//! See `docs/uwsm-parity-spec.md` for the behavior this binary must match.

// Helpers are ported ahead of the subcommands that use them; drop this once
// every subcommand is wired up.
#![allow(dead_code)]

mod cli;
mod config;
mod escape;
mod misc;
mod varnames;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Mode};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    let argv0 = args.remove(0);
    let _terminal_opts = cli::extract_terminal_opts(&mut args);
    let cli = Cli::parse_from(std::iter::once(argv0).chain(args));

    if let Mode::Aux { .. } = cli.mode
        && !called_by_systemd_manager()
    {
        eprintln!("Aux actions can only be run by systemd user manager!");
        return ExitCode::from(1);
    }

    let name = match &cli.mode {
        Mode::Select => "select",
        Mode::Start(_) => "start",
        Mode::Stop(_) => "stop",
        Mode::Finalize(_) => "finalize",
        Mode::App(_) => "app",
        Mode::Check { .. } => "check",
        Mode::Aux { .. } => "aux",
    };
    eprintln!("rwsm: \"{name}\" is not implemented yet");
    ExitCode::from(1)
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
