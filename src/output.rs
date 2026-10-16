//! Message output, ported from uwsm's `print_*` helpers (spec §10).
//!
//! Plain messages go to stdout. Ok and warning messages go to stdout, errors to
//! stderr; all three are colored on a TTY, prefixed with `<N>` journal levels
//! when [`set_prefix`] is on and the stream is not a TTY, and can be mirrored to
//! syslog and desktop notifications.

use std::ffi::CString;
use std::io::{IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::misc::str2bool_plus;

static LOG: AtomicBool = AtomicBool::new(false);
static PREFIX: AtomicBool = AtomicBool::new(false);
static NO_STDOUT: AtomicBool = AtomicBool::new(false);
static NO_WARN: AtomicBool = AtomicBool::new(false);
static DEBUG: OnceLock<(bool, Option<String>)> = OnceLock::new();

/// Also send messages to syslog.
pub fn set_log(on: bool) {
    LOG.store(on, Ordering::Relaxed);
}

/// Prefix lines with `<N>` loglevels when not writing to a TTY.
pub fn set_prefix(on: bool) {
    PREFIX.store(on, Ordering::Relaxed);
}

/// `UWSM_SILENT_START` handling: suppress stdout, and optionally warnings too.
pub fn set_silent(no_stdout: bool, no_warn: bool) {
    NO_STDOUT.store(no_stdout, Ordering::Relaxed);
    NO_WARN.store(no_warn, Ordering::Relaxed);
}

pub fn no_stdout() -> bool {
    NO_STDOUT.load(Ordering::Relaxed)
}

pub fn no_warn() -> bool {
    NO_WARN.load(Ordering::Relaxed)
}

fn debug_state() -> &'static (bool, Option<String>) {
    DEBUG.get_or_init(|| {
        let raw = std::env::var("UWSM_DEBUG")
            .or_else(|_| std::env::var("DEBUG"))
            .unwrap_or_else(|_| "0".into());
        match str2bool_plus(&raw) {
            Ok(v) => (v.as_bool(), None),
            Err(_) => (
                false,
                Some(format!(
                    "Expected boolean or numeric or empty value for DEBUG, got \"{raw}\", assuming False"
                )),
            ),
        }
    })
}

/// Whether `UWSM_DEBUG`/`DEBUG` asked for debug output.
pub fn debug_enabled() -> bool {
    debug_state().0
}

/// Warning about an invalid `DEBUG` value, to print once at startup.
pub fn debug_warning() -> Option<&'static str> {
    debug_state().1.as_deref()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn is_tty(self) -> bool {
        match self {
            Stream::Stdout => std::io::stdout().is_terminal(),
            Stream::Stderr => std::io::stderr().is_terminal(),
        }
    }

    fn write(self, text: &str) {
        // Flush each message so output from forked helpers stays ordered.
        let _ = match self {
            Stream::Stdout => {
                let mut out = std::io::stdout().lock();
                out.write_all(text.as_bytes()).and_then(|_| out.flush())
            }
            Stream::Stderr => {
                let mut err = std::io::stderr().lock();
                err.write_all(text.as_bytes()).and_then(|_| err.flush())
            }
        };
    }
}

/// When to also send a desktop notification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Notify {
    Never,
    /// Only when the output stream is not a TTY (e.g. launched from a compositor).
    IfNotTty,
    Always,
}

struct Style {
    stream: Stream,
    color: &'static str,
    loglevel: u8,
    urgency: u8,
    summary: &'static str,
    icon: &'static str,
}

const RESET: &str = "\x1b[0m";

const OK: Style = Style {
    stream: Stream::Stdout,
    color: "\x1b[32m",
    loglevel: 5,
    urgency: 0,
    summary: "Message",
    icon: "applications-system",
};
const WARNING: Style = Style {
    stream: Stream::Stdout,
    color: "\x1b[33m",
    loglevel: 4,
    urgency: 1,
    summary: "Warning",
    icon: "dialog-warning",
};
const ERROR: Style = Style {
    stream: Stream::Stderr,
    color: "\x1b[31m",
    loglevel: 3,
    urgency: 2,
    summary: "Error",
    icon: "dialog-error",
};
const DEBUG_STYLE: Style = Style {
    stream: Stream::Stderr,
    color: "\x1b[90m",
    loglevel: 7,
    urgency: 0,
    summary: "",
    icon: "",
};

/// Port of `print_fancy`. Like upstream, `nostdout` only applies on a TTY.
fn fancy(style: &Style, msg: &str, nostdout: bool, notify: Notify) {
    let tty = style.stream.is_tty();
    if tty {
        if !(nostdout && style.stream == Stream::Stdout) {
            style
                .stream
                .write(&format!("{}{msg}\n{RESET}", style.color));
        }
    } else if PREFIX.load(Ordering::Relaxed) {
        let prefixed: Vec<String> = msg
            .lines()
            .map(|l| format!("<{}>{l}", style.loglevel))
            .collect();
        style.stream.write(&format!("{}\n", prefixed.join("\n")));
    } else {
        style.stream.write(&format!("{msg}\n"));
    }

    if LOG.load(Ordering::Relaxed) {
        syslog(style.loglevel, msg.trim());
    }

    if (notify == Notify::Always || (notify == Notify::IfNotTty && !tty))
        && let Err(e) = crate::dbus::notify(style.summary, msg, style.icon, style.urgency)
    {
        fancy(
            &WARNING,
            &e.to_string(),
            no_stdout() && no_warn(),
            Notify::Never,
        );
    }
}

/// Port of `print_normal`.
pub fn normal(msg: &str) {
    if !no_stdout() {
        Stream::Stdout.write(&format!("{msg}\n"));
    }
    if LOG.load(Ordering::Relaxed) {
        syslog(6, msg.trim());
    }
}

/// Like [`normal`] but without the trailing newline (progress counters).
pub fn normal_inline(msg: &str) {
    if !no_stdout() {
        Stream::Stdout.write(msg);
    }
}

pub fn ok(msg: &str) {
    fancy(&OK, msg, no_stdout(), Notify::Never);
}

pub fn warning(msg: &str) {
    fancy(&WARNING, msg, no_stdout() && no_warn(), Notify::Never);
}

pub fn warning_notify(msg: &str, notify: Notify) {
    fancy(&WARNING, msg, no_stdout() && no_warn(), notify);
}

pub fn error(msg: &str) {
    fancy(&ERROR, msg, no_stdout(), Notify::Never);
}

pub fn error_notify(msg: &str, notify: Notify) {
    fancy(&ERROR, msg, no_stdout(), notify);
}

/// Debug output, only when `UWSM_DEBUG`/`DEBUG` is truthy.
pub fn debug_at(location: &str, msg: &str) {
    if debug_enabled() {
        fancy(
            &DEBUG_STYLE,
            &format!("DEBUG {location}\n{msg}\nEND_DEBUG"),
            false,
            Notify::Never,
        );
    }
}

/// `debug!("fmt", args..)` prints with the call site when debugging is on.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::output::debug_enabled() {
            $crate::output::debug_at(
                concat!(module_path!(), ":", line!()),
                &format!($($arg)*),
            );
        }
    };
}

fn syslog(level: u8, msg: &str) {
    static OPENED: OnceLock<()> = OnceLock::new();
    OPENED.get_or_init(|| {
        // Python's syslog module opens with the program name as ident.
        static IDENT: &std::ffi::CStr = c"uwsm";
        unsafe { libc::openlog(IDENT.as_ptr(), 0, libc::LOG_USER) };
    });
    let Ok(text) = CString::new(msg.replace('\0', " ")) else {
        return;
    };
    unsafe {
        libc::syslog(
            libc::LOG_USER | i32::from(level),
            c"%s".as_ptr(),
            text.as_ptr(),
        )
    };
}
