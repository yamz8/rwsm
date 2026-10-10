//! Build-time constants, the equivalent of uwsm's `params.py.in`.
//!
//! Each value can be overridden by setting the environment variable of the same
//! name when running `cargo build`, e.g. `UWSM_BIN_PATH=/usr/local/bin/uwsm`.

macro_rules! build_env {
    ($name:literal, $default:expr) => {
        match option_env!($name) {
            Some(value) => value,
            None => $default,
        }
    };
}

/// Name used in runtime/config paths, unit marks and log identifiers.
/// Must stay `uwsm` for drop-in compatibility.
pub const BIN_NAME: &str = "uwsm";

/// Absolute path of the installed binary, written into generated unit drop-ins.
pub const BIN_PATH: &str = build_env!("UWSM_BIN_PATH", "/usr/bin/uwsm");

/// Directory holding `prepare-env.sh` and `signal-handler.sh`.
pub const PKG_LIBEXEC_DIR: &str = build_env!("UWSM_LIBEXEC_DIR", "/usr/libexec/uwsm");

/// POSIX shell used to run the libexec scripts.
pub const SH_BIN: &str = build_env!("UWSM_SH_BIN", "sh");

/// `waitpid` binary used by `wayland-session-bindpid@.service`.
pub const WAITPID_BIN: &str = build_env!("UWSM_WAITPID_BIN", "waitpid");

/// `whiptail` binary used by `uwsm select`.
pub const WHIPTAIL_BIN: &str = build_env!("UWSM_WHIPTAIL_BIN", "whiptail");

/// Version reported by `uwsm --version`.
pub const PROJECT_VERSION: &str = env!("CARGO_PKG_VERSION");
