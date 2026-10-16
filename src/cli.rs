//! Command line, mirroring uwsm's argparse definitions (spec §2).
//!
//! Values that uwsm presets from environment variables (`-U`, `-t/-T`, app `-t`)
//! are parsed as `Option`s here and resolved by the `resolve_*` helpers, so the
//! env lookup and its warnings stay out of the parser.

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::misc::str2bool_plus;

#[derive(Debug, Parser)]
#[command(
    name = "uwsm",
    about = "Universal Wayland Session Manager.",
    long_about = "Universal Wayland Session Manager.\n\n\
        Launches arbitrary wayland compositor via a set of systemd user units \
        to provide graphical user session with environment management, \
        XDG autostart support, scoped application launch helpers, clean shutdown.",
    after_help = "See \"uwsm {subcommand} -h\" for further help on each subcommand.\n\
        See \"man uwsm\" for more detailed info on integration and operation.",
    subcommand_value_name = "subcommand",
    disable_version_flag = true,
    version = crate::config::PROJECT_VERSION
)]
pub struct Cli {
    /// print version and exit
    #[arg(short = 'v', long = "version", action = ArgAction::Version)]
    _version: Option<bool>,

    #[command(subcommand)]
    pub mode: Mode,
}

#[derive(Debug, Subcommand)]
pub enum Mode {
    /// Select default compositor entry
    #[command(
        long_about = "Invokes whiptail menu for selecting wayland-sessions Desktop Entries.",
        after_help = "Entries are selected from \"wayland-sessions\" XDG data hierarchy. \
            Default selection is read from first encountered \"uwsm/default-id\" file in \
            XDG Config hierarchy and system part of XDG Data hierarchy. When selected, choice \
            is saved to user part of XDG Config hierarchy (\"${XDG_CONFIG_HOME}/uwsm/default-id\"). \
            Nothing else is done."
    )]
    Select,

    /// Start compositor
    #[command(
        long_about = "Generates units for given compositor command line or Desktop Entry and starts compositor.",
        after_help = "Compositor should either put WAYLAND_DISPLAY var into systemd user service \
            environment itself, or finalize its startup by running this:\n\n  \
            uwsm finalize [VAR ...]\n\n\
            Otherwise, the compositor's unit will terminate due to startup timeout."
    )]
    Start(StartArgs),

    /// Stop compositor
    #[command(long_about = "Stops compositor and optionally removes generated units.")]
    Stop(StopArgs),

    /// Export variables from compositor, notify systemd of unit startup.
    #[command(
        long_about = "For use inside compositor to export essential variables and complete compositor unit startup.",
        after_help = "Exports variables to systemd user manager: WAYLAND_DISPLAY, DISPLAY, \
            and any optional variables mentioned by name as arguments, or listed \
            whitespace-separated in UWSM_FINALIZE_VARNAMES environment var.\n\n\
            Variables are also added to cleanup list to be unset during deactivation.\n\n\
            If all is well, sends startup notification to systemd user manager, \
            so compositor unit is considered started and graphical-session.target \
            can be declared reached."
    )]
    Finalize(FinalizeArgs),

    /// Application unit launcher
    #[command(
        long_about = "Launches application as a scope or service in specific slice.",
        after_help = "It is highly recommended to configure your compositor to launch apps \
            via this command to fully utilize user-level systemd unit management.\n\
            It would not be prudent to accumulate app processes in compositor's unit."
    )]
    App(AppArgs),

    /// Performs state checks
    #[command(
        long_about = "Performs a check, returns 0 if true, 1 if false.",
        after_help = "Use may-start checker to integrate startup into shell profile\n\
            See \"uwsm check may-start -h\"",
        subcommand_value_name = "checker"
    )]
    Check {
        #[command(subcommand)]
        checker: Checker,
    },

    /// Auxiliary functions
    #[command(
        long_about = "Can only be called by systemd user manager, used in units Exec*= keys",
        subcommand_value_name = "subcommand"
    )]
    Aux {
        #[command(subcommand)]
        action: AuxAction,
    },
}

/// `-D`, `-a`/`-e`, `-N`, `-C`, shared by `start` and `aux prepare-env`.
#[derive(Debug, Args, Clone, Default)]
pub struct WmMeta {
    /// Names to fill XDG_CURRENT_DESKTOP with (:-separated). Existing var content is a
    /// starting point if no active session is running.
    #[arg(short = 'D', value_name = "name[:name...]", default_value = "")]
    pub desktop_names: String,

    /// Append desktop names set by -D to other sources (default).
    #[arg(short = 'a', conflicts_with = "exclusive")]
    pub append: bool,

    /// Use desktop names set by -D exclusively, discard other sources.
    #[arg(short = 'e', id = "exclusive")]
    pub desktop_names_exclusive: bool,

    /// Fancy name for compositor (filled from Desktop Entry by default).
    #[arg(short = 'N', value_name = "Name", default_value = "")]
    pub wm_name: String,

    /// Fancy description for compositor (filled from Desktop Entry by default).
    #[arg(short = 'C', value_name = "Comment", default_value = "")]
    pub wm_comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UnitRung {
    Run,
    Home,
}

impl UnitRung {
    pub fn as_str(self) -> &'static str {
        match self {
            UnitRung::Run => "run",
            UnitRung::Home => "home",
        }
    }

    pub fn other(self) -> Self {
        match self {
            UnitRung::Run => UnitRung::Home,
            UnitRung::Home => UnitRung::Run,
        }
    }
}

#[derive(Debug, Args)]
pub struct StartArgs {
    #[command(flatten)]
    pub meta: WmMeta,

    /// Generated unit/drop-in files destination (default: run, can be preset by UWSM_UNIT_RUNG env var).
    #[arg(short = 'U', value_enum)]
    pub unit_rung: Option<UnitRung>,

    /// Do not generate tweak drop-ins (can be preset by UWSM_TWEAKS=false env var).
    #[arg(short = 't', conflicts_with = "tweaks_on")]
    pub tweaks_off: bool,

    /// Generate tweak drop-ins (default).
    #[arg(short = 'T', id = "tweaks_on")]
    pub tweaks_on: bool,

    /// Hardcode resulting command line (with path) to unit drop-ins.
    #[arg(short = 'F')]
    pub hardcode: bool,

    /// Seconds to wait for graphical.target in queue and warn if timed out (default: 60; negative to disable).
    #[arg(
        short = 'g',
        value_name = "S",
        allow_negative_numbers = true,
        conflicts_with = "gst_abort_seconds"
    )]
    pub gst_warn_seconds: Option<i64>,

    /// Seconds to wait for graphical.target in queue and abort if timed out (takes precedence over -g, default: -1 (disabled)).
    #[arg(short = 'G', value_name = "S", allow_negative_numbers = true)]
    pub gst_abort_seconds: Option<i64>,

    /// Only generate units, but do not start.
    #[arg(short = 'o')]
    pub only_generate: bool,

    /// Dry run, do not write or start anything.
    #[arg(short = 'n')]
    pub dry_run: bool,

    /// Compositor command line. The first argument acts as an ID and should be either one of:
    /// executable name; Desktop Entry ID (optionally with ":"-delimited action ID);
    /// special value "select" or "default". If given as path, hardcode mode is implied.
    #[arg(value_name = "args", required = true, num_args = 1..)]
    pub wm_cmdline: Vec<String>,
}

impl StartArgs {
    pub fn gst_warn_seconds(&self) -> i64 {
        self.gst_warn_seconds.unwrap_or(60)
    }

    pub fn gst_abort_seconds(&self) -> i64 {
        self.gst_abort_seconds.unwrap_or(-1)
    }
}

#[derive(Debug, Args)]
pub struct StopArgs {
    /// Also remove unit files, all or only with specific mark(s) (compositor ID, "tweaks",
    /// "generic" as comma-separated list).
    #[arg(short = 'r', value_name = "compositor", num_args = 0..=1)]
    pub remove_units: Option<Option<String>>,

    /// Generated unit/drop-in files destination (default: run, can be preset by UWSM_UNIT_RUNG env var).
    #[arg(short = 'U', value_enum)]
    pub unit_rung: Option<UnitRung>,

    /// Dry run, do not stop or remove anything.
    #[arg(short = 'n')]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct FinalizeArgs {
    /// Additional vars to export by name. Direct assignment "VAR_NAME=value" is also possible,
    /// but recommended only for creating flags for "UWSM_WAIT_VARNAMES" mechanism.
    #[arg(value_name = "VAR_NAME")]
    pub env_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AppUnitType {
    Scope,
    Service,
}

impl AppUnitType {
    pub fn as_str(self) -> &'static str {
        match self {
            AppUnitType::Scope => "scope",
            AppUnitType::Service => "service",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Silent {
    Out,
    Err,
    Both,
}

#[derive(Debug, Args)]
pub struct AppArgs {
    /// Slice selector: a (app-graphical.slice), b (background-graphical.slice),
    /// s (session-graphical.slice), or custom by full name.
    #[arg(short = 's', value_name = "{a,b,s,custom.slice}", default_value = "a")]
    pub slice_name: String,

    /// Type of unit to launch (default: scope, can be preset by UWSM_APP_UNIT_TYPE env var).
    #[arg(short = 't', value_enum)]
    pub app_unit_type: Option<AppUnitType>,

    /// Override app name (a substring in unit name).
    #[arg(short = 'a', value_name = "app_name", default_value = "")]
    pub app_name: String,

    /// Override the whole autogenerated unit name.
    #[arg(short = 'u', value_name = "unit_name", default_value = "")]
    pub unit_name: String,

    /// Unit Description.
    #[arg(short = 'd', value_name = "unit_description", default_value = "")]
    pub unit_description: String,

    /// Pass Unit property assignment.
    #[arg(short = 'p', value_name = "Property=value")]
    pub unit_properties: Vec<String>,

    /// Silence stdout, stderr, or both.
    #[arg(short = 'S', value_enum)]
    pub silent: Option<Silent>,

    /// Launch app in a terminal, or just a terminal if command is empty.
    #[arg(short = 'T')]
    pub terminal: bool,

    /// Application command line. The first argument can be either one of: executable name or
    /// path; Desktop Entry ID (with optional ":"-delimited action ID); path to Desktop Entry
    /// file (with optional ":"-delimited action ID).
    #[arg(value_name = "args", required_unless_present = "terminal")]
    pub cmdline: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Checker {
    /// Checks for active compositor
    #[command(
        long_about = "Checks for specific compositor or graphical-session*.target in general in active or activating state"
    )]
    IsActive {
        /// Specify compositor by executable or Desktop Entry (without arguments).
        wm: Option<String>,

        /// Show additional info.
        #[arg(short = 'v')]
        verbose: bool,
    },

    /// Checks for start conditions
    #[command(
        long_about = "Checks whether it is OK to launch a wayland session.",
        after_help = "Conditions:\n  \
            - User's dbus is available\n  \
            - Running from login shell\n  \
            - System is at graphical.target\n  \
            - User graphical-session*.target are not yet active\n  \
            - Foreground and session's VT is among allowed (default: 1)\n\n\
            This command is essential for integrating startup into shell profile."
    )]
    MayStart(MayStartArgs),
}

#[derive(Debug, Args)]
pub struct MayStartArgs {
    /// VT numbers allowed for startup (default: 1; disable check: 0).
    #[arg(value_name = "N")]
    pub vtnr: Vec<u32>,

    /// Show all failed tests.
    #[arg(short = 'v', conflicts_with = "quiet")]
    pub verbose: bool,

    /// Do not show anything.
    #[arg(short = 'q')]
    pub quiet: bool,

    /// Do not check for login shell.
    #[arg(short = 'i')]
    pub nologin: bool,

    /// Do not check for local session (allow remote).
    #[arg(short = 'r')]
    pub allow_remote: bool,

    /// Seconds to wait for graphical.target in queue (default: 60; 0 or less disables check).
    #[arg(
        short = 'g',
        value_name = "S",
        default_value_t = 60,
        allow_negative_numbers = true
    )]
    pub gst_seconds: i64,
}

impl MayStartArgs {
    pub fn allowed_vtnr(&self) -> Vec<u32> {
        if self.vtnr.is_empty() {
            vec![1]
        } else {
            self.vtnr.clone()
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum AuxAction {
    /// Prepares environment (for wayland-wm-env@.service, wayland-session-pre@.target).
    #[command(
        long_about = "Prepares environment variables, loads into systemd user activation environment.",
        after_help = "Used in ExecStart in wayland-wm-env@.service for wayland-session-pre@.target, graphical-session-pre.target."
    )]
    PrepareEnv {
        #[command(flatten)]
        meta: WmMeta,

        /// Compositor ID.
        #[arg(value_name = "ID")]
        wm_id: String,

        /// Full Compositor command line.
        #[arg(value_name = "args")]
        wm_cmdline: Vec<String>,
    },

    /// Cleans up environment (for wayland-wm-env@.service, wayland-session-pre@.target).
    #[command(
        long_about = "Cleans up systemd user activation environment from previously loaded session-related variables.",
        after_help = "Used in ExecStop in wayland-wm-env@.service for wayland-session-pre@.target, graphical-session-pre.target."
    )]
    CleanupEnv,

    /// Executes binary with arguments or Desktop Entry (for wayland-wm@.service, wayland-session@.target).
    #[command(
        long_about = "Executes compositor command line or Desktop Entry.",
        after_help = "Used in ExecStart of wayland-wm@.service for wayland-session@.target, graphical-session.target."
    )]
    Exec {
        /// Compositor ID.
        #[arg(value_name = "ID")]
        wm_id: String,

        /// Full Compositor command line.
        #[arg(value_name = "args")]
        wm_cmdline: Vec<String>,
    },

    /// Waits for a PID to exit (for wayland-session-bindpid@.service).
    #[command(
        long_about = "Exits successfully when a process by given PID ends, or if it does not exist.",
        after_help = "Used in wayland-session-bindpid@.service as a shim for waitpid if it is unavailable."
    )]
    Waitpid {
        /// PID to wait for
        #[arg(value_name = "PID")]
        pid: i32,
    },

    /// Waits for WAYLAND_DISPLAY and optionally other vars (for wayland-session-waitenv.service, wayland-session@.target).
    #[command(
        long_about = "Exits successfully when WAYLAND_DISPLAY (and optionally other vars) appear in systemd \
            user manager activation environment. Optional vars are read from command line and \
            whitespace-separated UWSM_WAIT_VARNAMES var.",
        after_help = "Used in wayland-session-waitenv.service to delay activation of wayland-session@.target, graphical-session.target."
    )]
    Waitenv {
        /// Names of additional variables to wait for
        #[arg(value_name = "VAR_NAME")]
        env_names: Vec<String>,
    },

    /// Daemon for fast app argument generation.
    #[command(
        long_about = "Receives app arguments from a named pipe, returns shell code",
        after_help = "Receives app arguments via \"${XDG_RUNTIME_DIR}/uwsm-app-daemon-in\" pipe.\n\
            Returns shell code to \"${XDG_RUNTIME_DIR}/uwsm-app-daemon-out\" pipe.\n\n\
            Arguments are expected to be \"\\0\"-delimited, leading \"\\0\" are stripped. \
            One command is received per write+close.\n\n\
            The first argument determines the behavior:\n\n  \
            app\tthe rest is processed the same as in \"uwsm app\"\n  \
            ping\tjust \"pong\" is returned\n  \
            stop\tdaemon is stopped"
    )]
    AppDaemon,

    /// Internal: waits for the compositor to export its environment and
    /// notifies readiness (forked off by `aux exec`, not part of uwsm's CLI).
    #[command(hide = true)]
    Autoready {
        #[arg(long)]
        main_pid: i32,

        #[arg(value_name = "ID")]
        wm_id: String,

        #[arg(value_name = "ID_UNIT_STRING")]
        id_unit_string: String,
    },
}

/// Terminal options pulled out of `app -T ... --` before parsing (spec §2.5, P-CLI-APP-T).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TerminalOpts {
    /// `--app-id=`, `--title=`, `--dir=`, `--hold` and other unknown dashed options.
    pub opts: Vec<String>,
    /// `--print-*` options (collected, unused upstream).
    pub print_opts: Vec<String>,
}

/// Options `app` knows itself; everything else dashed between `-T` and `--` is
/// a terminal option.
const APP_OPTS: [&str; 7] = ["-s", "-t", "-a", "-u", "-d", "-S", "-T"];

/// Removes xdg-terminal-exec style options from `args` (without argv0), like
/// uwsm's pre-parse filter. Only applies to `app` when `-T` comes before `--`.
pub fn extract_terminal_opts(args: &mut Vec<String>) -> TerminalOpts {
    let mut result = TerminalOpts::default();
    if args.first().map(String::as_str) != Some("app") {
        return result;
    }
    let first_marker = args[1..].iter().find(|a| *a == "-T" || *a == "--");
    if first_marker.map(String::as_str) != Some("-T") {
        return result;
    }

    let mut capture = false;
    let mut kept = Vec::with_capacity(args.len());
    let mut iter = std::mem::take(args).into_iter();
    for arg in iter.by_ref() {
        if arg == "--" {
            kept.push(arg);
            break;
        }
        if !capture && arg == "-T" {
            capture = true;
            kept.push(arg);
            continue;
        }
        if capture && arg.starts_with('-') && !APP_OPTS.contains(&arg.as_str()) {
            if arg.starts_with("--print-") {
                result.print_opts.push(arg);
            } else {
                result.opts.push(arg);
            }
            continue;
        }
        kept.push(arg);
    }
    kept.extend(iter);
    *args = kept;
    result
}

/// A non-fatal message produced while resolving env presets.
pub type Warning = String;

/// `-U`, falling back to `UWSM_UNIT_RUNG`, then `run`.
pub fn resolve_unit_rung(cli: Option<UnitRung>, env: Option<&str>) -> (UnitRung, Option<Warning>) {
    if let Some(rung) = cli {
        return (rung, None);
    }
    match env {
        None => (UnitRung::Run, None),
        Some("run") => (UnitRung::Run, None),
        Some("home") => (UnitRung::Home, None),
        Some(other) => (
            UnitRung::Run,
            Some(format!(
                "invalid UWSM_UNIT_RUNG value \"{other}\" ignored, set to \"run\"."
            )),
        ),
    }
}

/// `-t`/`-T`, falling back to `UWSM_TWEAKS`, then true.
pub fn resolve_tweaks(args: &StartArgs, env: Option<&str>) -> (bool, Option<Warning>) {
    if args.tweaks_off {
        return (false, None);
    }
    if args.tweaks_on {
        return (true, None);
    }
    match env.map(str2bool_plus) {
        None => (true, None),
        Some(Ok(value)) => (value.as_bool(), None),
        Some(Err(_)) => (
            true,
            Some(format!(
                "Expected boolean value from UWSM_TWEAKS, got \"{}\" ignored, set to True.",
                env.unwrap_or_default()
            )),
        ),
    }
}

/// App `-t`, falling back to `UWSM_APP_UNIT_TYPE`, then scope.
pub fn resolve_app_unit_type(
    cli: Option<AppUnitType>,
    env: Option<&str>,
) -> (AppUnitType, Option<Warning>) {
    if let Some(t) = cli {
        return (t, None);
    }
    match env {
        None => (AppUnitType::Scope, None),
        Some("scope") => (AppUnitType::Scope, None),
        Some("service") => (AppUnitType::Service, None),
        Some(other) => (
            AppUnitType::Scope,
            Some(format!(
                "invalid UWSM_APP_UNIT_TYPE value \"{other}\" ignored, set to \"scope\"."
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("uwsm").chain(args.iter().copied()))
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn command_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn start_with_desktop_entry_and_args() {
        let cli = parse(&[
            "start",
            "-D",
            "sway:wlroots",
            "-e",
            "--",
            "sway.desktop",
            "--unsupported-gpu",
        ])
        .unwrap();
        let Mode::Start(s) = cli.mode else { panic!() };
        assert_eq!(s.meta.desktop_names, "sway:wlroots");
        assert!(s.meta.desktop_names_exclusive);
        assert_eq!(
            s.wm_cmdline,
            strings(&["sway.desktop", "--unsupported-gpu"])
        );
        assert_eq!(s.gst_warn_seconds(), 60);
        assert_eq!(s.gst_abort_seconds(), -1);
    }

    #[test]
    fn start_requires_command() {
        assert!(parse(&["start"]).is_err());
    }

    #[test]
    fn start_mutually_exclusive_flags() {
        assert!(parse(&["start", "-a", "-e", "sway"]).is_err());
        assert!(parse(&["start", "-t", "-T", "sway"]).is_err());
        assert!(parse(&["start", "-g", "5", "-G", "5", "sway"]).is_err());
    }

    #[test]
    fn start_negative_timeouts() {
        let cli = parse(&["start", "-g", "-1", "sway"]).unwrap();
        let Mode::Start(s) = cli.mode else { panic!() };
        assert_eq!(s.gst_warn_seconds(), -1);
    }

    #[test]
    fn stop_remove_units_three_states() {
        let Mode::Stop(s) = parse(&["stop"]).unwrap().mode else {
            panic!()
        };
        assert_eq!(s.remove_units, None);
        let Mode::Stop(s) = parse(&["stop", "-r"]).unwrap().mode else {
            panic!()
        };
        assert_eq!(s.remove_units, Some(None));
        let Mode::Stop(s) = parse(&["stop", "-r", "sway,tweaks"]).unwrap().mode else {
            panic!()
        };
        assert_eq!(s.remove_units, Some(Some("sway,tweaks".into())));
    }

    #[test]
    fn app_needs_cmdline_unless_terminal() {
        assert!(parse(&["app"]).is_err());
        let Mode::App(a) = parse(&["app", "-T"]).unwrap().mode else {
            panic!()
        };
        assert!(a.terminal);
        assert!(a.cmdline.is_empty());
    }

    #[test]
    fn app_properties_repeat() {
        let Mode::App(a) = parse(&["app", "-p", "A=1", "-p", "B=2", "--", "foot"])
            .unwrap()
            .mode
        else {
            panic!()
        };
        assert_eq!(a.unit_properties, strings(&["A=1", "B=2"]));
        assert_eq!(a.slice_name, "a");
    }

    #[test]
    fn terminal_opts_are_extracted() {
        let mut args = strings(&[
            "app",
            "-T",
            "--app-id=foo",
            "-s",
            "b",
            "--hold",
            "--print-cmd",
            "--",
            "htop",
            "--dir=x",
        ]);
        let opts = extract_terminal_opts(&mut args);
        assert_eq!(opts.opts, strings(&["--app-id=foo", "--hold"]));
        assert_eq!(opts.print_opts, strings(&["--print-cmd"]));
        assert_eq!(
            args,
            strings(&["app", "-T", "-s", "b", "--", "htop", "--dir=x"])
        );
    }

    #[test]
    fn terminal_opts_untouched_without_t_before_double_dash() {
        let mut args = strings(&["app", "--", "foot", "-T", "--hold"]);
        let before = args.clone();
        assert_eq!(extract_terminal_opts(&mut args), TerminalOpts::default());
        assert_eq!(args, before);
    }

    #[test]
    fn may_start_defaults() {
        let Mode::Check {
            checker: Checker::MayStart(m),
        } = parse(&["check", "may-start"]).unwrap().mode
        else {
            panic!()
        };
        assert_eq!(m.allowed_vtnr(), vec![1]);
        assert_eq!(m.gst_seconds, 60);
        assert!(parse(&["check", "may-start", "-v", "-q"]).is_err());
    }

    #[test]
    fn aux_exec_keeps_empty_and_dashed_args() {
        let cli = parse(&["aux", "exec", "--", "sway.desktop", "", "--unsupported-gpu"]).unwrap();
        let Mode::Aux {
            action: AuxAction::Exec { wm_id, wm_cmdline },
        } = cli.mode
        else {
            panic!()
        };
        assert_eq!(wm_id, "sway.desktop");
        assert_eq!(wm_cmdline, strings(&["", "--unsupported-gpu"]));
    }

    #[test]
    fn aux_prepare_env_like_dropin() {
        let cli = parse(&[
            "aux",
            "prepare-env",
            "-eD",
            "sway:wlroots",
            "--",
            "sway",
            "/usr/bin/sway",
        ])
        .unwrap();
        let Mode::Aux {
            action:
                AuxAction::PrepareEnv {
                    meta,
                    wm_id,
                    wm_cmdline,
                },
        } = cli.mode
        else {
            panic!()
        };
        assert!(meta.desktop_names_exclusive);
        assert_eq!(meta.desktop_names, "sway:wlroots");
        assert_eq!(wm_id, "sway");
        assert_eq!(wm_cmdline, strings(&["/usr/bin/sway"]));
    }

    #[test]
    fn env_presets() {
        assert_eq!(resolve_unit_rung(None, Some("home")).0, UnitRung::Home);
        assert_eq!(
            resolve_unit_rung(Some(UnitRung::Run), Some("home")).0,
            UnitRung::Run
        );
        let (rung, warning) = resolve_unit_rung(None, Some("bogus"));
        assert_eq!(rung, UnitRung::Run);
        assert!(warning.is_some());

        assert_eq!(
            resolve_app_unit_type(None, Some("service")).0,
            AppUnitType::Service
        );
        assert_eq!(resolve_app_unit_type(None, None).0, AppUnitType::Scope);

        let Mode::Start(s) = parse(&["start", "sway"]).unwrap().mode else {
            panic!()
        };
        assert_eq!(resolve_tweaks(&s, Some("false")), (false, None));
        assert_eq!(resolve_tweaks(&s, None), (true, None));
        assert!(resolve_tweaks(&s, Some("bogus")).1.is_some());
        let Mode::Start(s) = parse(&["start", "-T", "sway"]).unwrap().mode else {
            panic!()
        };
        assert_eq!(resolve_tweaks(&s, Some("false")), (true, None));
    }
}
