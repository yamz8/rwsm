//! Small helpers ported from uwsm's `misc.py`.

use anyhow::{Result, bail};

/// Parsed value of a boolean-or-numeric string (spec §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoolPlus(pub u64);

impl BoolPlus {
    pub fn as_bool(self) -> bool {
        self.0 > 0
    }

    pub fn as_int(self) -> u64 {
        self.0
    }
}

/// Port of `str2bool_plus`: digits give their number, `""`/`no`/`false`/`n`
/// give 0, `yes`/`true`/`y` give 1, anything else is an error.
pub fn str2bool_plus(s: &str) -> Result<BoolPlus> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(BoolPlus(s.parse()?));
    }
    match s.to_lowercase().as_str() {
        "" | "no" | "false" | "n" => Ok(BoolPlus(0)),
        "yes" | "true" | "y" => Ok(BoolPlus(1)),
        _ => bail!("Expected boolean or numeric or empty value, got \"{s}\""),
    }
}

/// Port of `sane_split`: like `str::split`, but an empty string gives no items.
pub fn sane_split<'a>(s: &'a str, delimiter: &str) -> Vec<&'a str> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(delimiter).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_plus() {
        assert_eq!(str2bool_plus("2").unwrap(), BoolPlus(2));
        assert!(!str2bool_plus("0").unwrap().as_bool());
        assert!(!str2bool_plus("").unwrap().as_bool());
        assert!(!str2bool_plus("No").unwrap().as_bool());
        assert!(str2bool_plus("TRUE").unwrap().as_bool());
        assert!(str2bool_plus("y").unwrap().as_bool());
        assert!(str2bool_plus("-1").is_err());
        assert!(str2bool_plus("maybe").is_err());
    }

    #[test]
    fn split() {
        assert!(sane_split("", ":").is_empty());
        assert_eq!(sane_split("a::b", ":"), vec!["a", "", "b"]);
    }
}
