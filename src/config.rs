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

/// `whiptail` binary used by `uwsm select`.
pub const WHIPTAIL_BIN: &str = build_env!("UWSM_WHIPTAIL_BIN", "whiptail");

/// uwsm release whose behavior this build matches; `data/` is vendored from it.
macro_rules! upstream_version {
    () => {
        "0.27.0"
    };
}

/// Version reported by `uwsm --version`: the matched uwsm release first, so
/// scripts that parse it keep working, then rwsm's own version.
pub const PROJECT_VERSION: &str = concat!(
    upstream_version!(),
    " (rwsm ",
    env!("CARGO_PKG_VERSION"),
    ")"
);

#[cfg(test)]
mod tests {
    #[test]
    fn upstream_version_matches_vendored_data() {
        let readme = include_str!("../data/README.md");
        assert!(readme.contains(concat!("v", upstream_version!(), " ")));
    }
}
