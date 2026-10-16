//! String escaping helpers that must match uwsm byte for byte.

/// Escapes a string the way uwsm's `simple_systemd_escape` does (spec §8.1).
///
/// This is deliberately not `systemd-escape`: `:` is kept, `/` becomes `-`,
/// and every byte outside `[._0-9:A-Za-z]` becomes a lowercase `\xNN`.
/// With `start` set, a leading `.` is escaped too.
pub fn simple_systemd_escape(s: &str, start: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    if start && let Some(stripped) = s.strip_prefix('.') {
        out.push_str("\\x2e");
        rest = stripped;
    }
    for ch in rest.chars() {
        if ch == '/' {
            out.push('-');
        } else if ch == '.' || ch == '_' || ch == ':' || ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for byte in ch.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("\\x{byte:02x}"));
            }
        }
    }
    out
}

/// Quotes a string like Python's `shlex.quote`.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// Joins arguments like Python's `shlex.join`.
pub fn shlex_join<S: AsRef<str>>(args: &[S]) -> String {
    args.iter()
        .map(|a| shlex_quote(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_keeps_allowed_chars() {
        assert_eq!(simple_systemd_escape("sway.desktop", false), "sway.desktop");
        assert_eq!(
            simple_systemd_escape("Hyprland.desktop:debug", false),
            "Hyprland.desktop:debug"
        );
        assert_eq!(simple_systemd_escape("a_b9", false), "a_b9");
    }

    #[test]
    fn escape_dash_space_slash() {
        assert_eq!(
            simple_systemd_escape("niri-session", false),
            "niri\\x2dsession"
        );
        assert_eq!(simple_systemd_escape("a b", false), "a\\x20b");
        assert_eq!(simple_systemd_escape("a/b", false), "a-b");
    }

    #[test]
    fn escape_leading_dot_only_at_start() {
        assert_eq!(simple_systemd_escape(".x", true), "\\x2ex");
        assert_eq!(simple_systemd_escape(".x", false), ".x");
    }

    #[test]
    fn escape_multibyte_utf8() {
        assert_eq!(simple_systemd_escape("é", false), "\\xc3\\xa9");
    }

    #[test]
    fn quote_matches_python() {
        assert_eq!(shlex_quote(""), "''");
        assert_eq!(shlex_quote("/usr/bin/uwsm"), "/usr/bin/uwsm");
        assert_eq!(shlex_quote("%I"), "%I");
        assert_eq!(shlex_quote("--unsupported-gpu"), "--unsupported-gpu");
        assert_eq!(shlex_quote("a b"), "'a b'");
        assert_eq!(shlex_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shlex_quote("é"), "'é'");
    }

    #[test]
    fn join_matches_uwsm_dropin_example() {
        let argv = [
            "/usr/bin/uwsm",
            "aux",
            "exec",
            "--",
            "%I",
            "",
            "--unsupported-gpu",
        ];
        assert_eq!(
            shlex_join(&argv),
            "/usr/bin/uwsm aux exec -- %I '' --unsupported-gpu"
        );
    }
}
