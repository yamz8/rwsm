//! Environment variable name validation and the export/cleanup name sets
//! (spec §5.1, §5.2).

use std::collections::{BTreeMap, BTreeSet};

/// Shell variables dropped silently instead of with a warning.
/// `OLDWD` (sic) matches upstream, which never drops `OLDPWD`.
const DROP_SH_VARS: [&str; 4] = ["_", "SHELL", "PWD", "OLDWD"];

/// `^([a-zA-Z_][a-zA-Z0-9_]+|[a-zA-Z][a-zA-Z0-9_]*)$`: a lone `_` is invalid.
pub fn is_valid_varname(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    let tail_ok = rest.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_');
    if first.is_ascii_alphabetic() {
        tail_ok
    } else {
        first == b'_' && !rest.is_empty() && tail_ok
    }
}

/// Result of checking one name with [`check_varname`].
#[derive(Debug, PartialEq, Eq)]
pub enum VarnameCheck {
    Keep,
    /// Dropped silently (shell bookkeeping variable).
    Drop,
    /// Dropped with an `Encountered illegal var` warning.
    Illegal,
}

pub fn check_varname(name: &str) -> VarnameCheck {
    if DROP_SH_VARS.contains(&name) {
        VarnameCheck::Drop
    } else if is_valid_varname(name) {
        VarnameCheck::Keep
    } else {
        VarnameCheck::Illegal
    }
}

/// Port of `filter_varnames` for a list of names. Returns the kept names and
/// the illegal ones, so callers can warn about them.
pub fn filter_names<I, S>(names: I) -> (Vec<String>, Vec<String>)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut kept = Vec::new();
    let mut illegal = Vec::new();
    for name in names {
        let name = name.into();
        match check_varname(&name) {
            VarnameCheck::Keep => kept.push(name),
            VarnameCheck::Drop => {}
            VarnameCheck::Illegal => illegal.push(name),
        }
    }
    (kept, illegal)
}

/// Port of `filter_varnames` for an environment map.
pub fn filter_env(env: BTreeMap<String, String>) -> (BTreeMap<String, String>, Vec<String>) {
    let mut illegal = Vec::new();
    let kept = env
        .into_iter()
        .filter(|(name, _)| match check_varname(name) {
            VarnameCheck::Keep => true,
            VarnameCheck::Drop => false,
            VarnameCheck::Illegal => {
                illegal.push(name.clone());
                false
            }
        })
        .collect();
    (kept, illegal)
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

pub const SESSION_SPECIFIC: [&str; 5] = [
    "XDG_SEAT",
    "XDG_SEAT_PATH",
    "XDG_SESSION_ID",
    "XDG_SESSION_PATH",
    "XDG_VTNR",
];

/// The four modifiable name sets plus the fixed ones, as in uwsm's `Varnames`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Varnames {
    pub always_unset: BTreeSet<String>,
    pub always_export: BTreeSet<String>,
    pub never_export: BTreeSet<String>,
    pub always_cleanup: BTreeSet<String>,
    pub never_cleanup: BTreeSet<String>,
}

/// One change made by [`Varnames::update_from_env`], for printing.
#[derive(Debug, PartialEq, Eq)]
pub struct SetChange {
    pub set_name: &'static str,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl Default for Varnames {
    fn default() -> Self {
        let session = set(&SESSION_SPECIFIC);
        let mut always_unset = set(&["DISPLAY", "WAYLAND_DISPLAY"]);
        always_unset.extend(session.iter().cloned());
        let mut never_export = set(&[
            "PWD",
            "LS_COLORS",
            "INVOCATION_ID",
            "SHLVL",
            "SHELL",
            "TERM",
            "COLORTERM",
            "TERM_SESSION_TYPE",
            "NOTIFY_SOCKET",
        ]);
        never_export.extend(session.iter().cloned());
        let mut always_cleanup = set(&[
            "DISPLAY",
            "LANG",
            "PATH",
            "WAYLAND_DISPLAY",
            "XCURSOR_SIZE",
            "XCURSOR_THEME",
            "XDG_CURRENT_DESKTOP",
            "XDG_MENU_PREFIX",
            "XDG_SESSION_DESKTOP",
            "XDG_SESSION_CLASS",
            "XDG_SESSION_TYPE",
            "NOTIFY_SOCKET",
        ]);
        always_cleanup.extend(session);
        Self {
            always_unset,
            always_export: set(&[
                "PATH",
                "XDG_CURRENT_DESKTOP",
                "XDG_MENU_PREFIX",
                "XDG_SESSION_DESKTOP",
                "XDG_SESSION_CLASS",
                "XDG_SESSION_TYPE",
            ]),
            never_export,
            always_cleanup,
            never_cleanup: set(&["SSH_AGENT_LAUNCHER", "SSH_AUTH_SOCK", "SSH_AGENT_PID"]),
        }
    }
}

impl Varnames {
    /// Port of `update_varnames_sets`: applies `UWSM_{ALWAYS,NEVER}_{EXPORT,CLEANUP}_VARNAMES`
    /// (bare or `+` names are added, `-` names removed). `lookup` reads a variable
    /// from whichever environment the caller is working with.
    pub fn update_from_env(&mut self, lookup: impl Fn(&str) -> Option<String>) -> Vec<SetChange> {
        let mut changes = Vec::new();
        for (set_name, target) in [
            ("always_export", &mut self.always_export),
            ("never_export", &mut self.never_export),
            ("always_cleanup", &mut self.always_cleanup),
            ("never_cleanup", &mut self.never_cleanup),
        ] {
            let raw =
                lookup(&format!("UWSM_{}_VARNAMES", set_name.to_uppercase())).unwrap_or_default();
            let words: Vec<&str> = raw.split_whitespace().collect();
            let (add, _) = filter_names(
                words
                    .iter()
                    .filter(|w| !w.starts_with('-'))
                    .map(|w| w.strip_prefix('+').unwrap_or(w)),
            );
            let (remove, _) = filter_names(words.iter().filter_map(|w| w.strip_prefix('-')));
            let added: Vec<String> = add
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|n| target.insert(n.clone()))
                .collect();
            let removed: Vec<String> = remove
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|n| target.remove(n))
                .collect();
            if !added.is_empty() || !removed.is_empty() {
                changes.push(SetChange {
                    set_name,
                    added,
                    removed,
                });
            }
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(is_valid_varname("A"));
        assert!(is_valid_varname("_A"));
        assert!(is_valid_varname("WAYLAND_DISPLAY"));
        assert!(is_valid_varname("a1"));
        assert!(!is_valid_varname("_"));
        assert!(!is_valid_varname(""));
        assert!(!is_valid_varname("1A"));
        assert!(!is_valid_varname("A-B"));
        assert!(!is_valid_varname("É"));
    }

    #[test]
    fn filtering() {
        let (kept, illegal) = filter_names(["PATH", "_", "SHELL", "OLDPWD", "OLDWD", "bad-name"]);
        assert_eq!(kept, vec!["PATH", "OLDPWD"]);
        assert_eq!(illegal, vec!["bad-name"]);
    }

    #[test]
    fn default_sets() {
        let v = Varnames::default();
        assert!(v.always_unset.contains("XDG_VTNR"));
        assert!(v.never_export.contains("XDG_SEAT"));
        assert!(v.always_cleanup.contains("LANG"));
        assert!(!v.always_export.contains("LANG"));
    }

    #[test]
    fn update_sets_from_env() {
        let mut v = Varnames::default();
        let changes = v.update_from_env(|name| match name {
            "UWSM_ALWAYS_EXPORT_VARNAMES" => Some("FOO +BAR -PATH PATH".into()),
            "UWSM_NEVER_CLEANUP_VARNAMES" => Some("-SSH_AUTH_SOCK bad-x".into()),
            _ => None,
        });
        // added first, then removed: PATH is already present so only removal applies
        assert!(v.always_export.contains("FOO"));
        assert!(v.always_export.contains("BAR"));
        assert!(!v.always_export.contains("PATH"));
        assert!(!v.never_cleanup.contains("SSH_AUTH_SOCK"));
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].added, vec!["BAR", "FOO"]);
        assert_eq!(changes[0].removed, vec!["PATH"]);
    }
}
