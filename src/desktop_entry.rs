//! Desktop entry parsing and checks (spec §6.1), covering what uwsm uses from
//! pyxdg's `DesktopEntry` plus uwsm's own value expansion and Exec tokenizer.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::escape::py_repr;
use crate::misc::sane_split;
use crate::{debug, xdg};

/// Main group headers pyxdg accepts, in order of preference.
const MAIN_GROUPS: [&str; 2] = ["Desktop Entry", "KDE Desktop Entry"];

/// How pyxdg's `checkValue` checks a key's value.
#[derive(Clone, Copy, PartialEq)]
enum Check {
    /// Not checked (`localestring`, or no `checkValue` call).
    None,
    /// Must be ASCII.
    Str,
    /// `true` or `false` (`1`/`0` are only deprecated).
    Bool,
    /// Parses as a float.
    Numeric,
}

/// One row of pyxdg's `DesktopEntry.checkKey` table.
struct KeySpec {
    name: &'static str,
    /// Accepts a `[locale]` suffix.
    localized: bool,
    check: Check,
    /// The value is a list; each item is checked.
    list: bool,
    /// The `Type` the key is restricted to.
    only_type: Option<&'static str>,
}

const fn key(name: &'static str, check: Check) -> KeySpec {
    KeySpec {
        name,
        localized: false,
        check,
        list: false,
        only_type: None,
    }
}

impl KeySpec {
    const fn localized(mut self) -> Self {
        self.localized = true;
        self
    }

    const fn list(mut self) -> Self {
        self.list = true;
        self
    }

    const fn only(mut self, entry_type: &'static str) -> Self {
        self.only_type = Some(entry_type);
        self
    }
}

/// pyxdg's `DesktopEntry.checkKey` table. Keys not listed (and not `X-*`)
/// are invalid. `URL` really is restricted to `Type=URL` in pyxdg.
const KEYS: &[KeySpec] = &[
    key("Version", Check::Str),
    key("Name", Check::None).localized(),
    key("GenericName", Check::None).localized(),
    key("NoDisplay", Check::Bool),
    key("Comment", Check::None).localized(),
    key("Icon", Check::Str).localized(),
    key("Hidden", Check::Bool),
    key("OnlyShowIn", Check::Str).list(),
    key("NotShowIn", Check::Str).list(),
    key("TryExec", Check::Str).only("Application"),
    key("Exec", Check::Str).only("Application"),
    key("Path", Check::Str).only("Application"),
    key("Terminal", Check::Bool).only("Application"),
    key("Actions", Check::Str).list().only("Application"),
    key("MimeType", Check::Str).list().only("Application"),
    key("Categories", Check::Str).only("Application"),
    key("Keywords", Check::None)
        .localized()
        .list()
        .only("Application"),
    key("StartupNotify", Check::Bool).only("Application"),
    key("StartupWMClass", Check::None).only("Application"),
    key("URL", Check::Str).only("URL"),
    key("ServiceTypes", Check::Str).list(),
    key("DocPath", Check::Str),
    key("InitialPreference", Check::Numeric),
    key("Dev", Check::Str).only("FSDevice"),
    key("FSType", Check::Str).only("FSDevice"),
    key("MountPoint", Check::Str).only("FSDevice"),
    key("ReadOnly", Check::Bool).only("FSDevice"),
    key("UnmountIcon", Check::Str).localized().only("FSDevice"),
    // deprecated keys pyxdg only warns about
    key("Encoding", Check::Str),
    key("MiniIcon", Check::Str).localized(),
    key("TerminalOptions", Check::Str),
    key("DefaultApp", Check::Str),
    key("Protocols", Check::Str).list(),
    key("Extensions", Check::Str).list(),
    key("BinaryPattern", Check::Str),
    key("MapNotify", Check::Str),
    key("SwallowTitle", Check::None).localized(),
    key("SwallowExec", Check::Str),
    // checked as a regex by pyxdg; any value is accepted here
    key("FilePattern", Check::None).list(),
    key("SortOrder", Check::Str).list(),
];

/// Validation errors uwsm tolerates (spec §6.1).
const TOLERATED_ERRORS: &[&str] = &[
    "Invalid key: ExecArg",
    "Missing main category",
    "Invalid key: DesktopNames",
    "Invalid key: DBusActivatable",
    "Invalid key: SingleMainWindow",
    "Invalid key: PrefersNonDefaultGPU",
    "Invalid key: TargetEnvironment",
];

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub path: PathBuf,
    /// Group name to key/value pairs, in file order.
    groups: Vec<(String, Vec<(String, String)>)>,
    /// The main group (pyxdg's `defaultGroup`): `Desktop Entry`, or
    /// `KDE Desktop Entry` in old files.
    main: String,
}

impl DesktopEntry {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            fs::read(path).with_context(|| format!("Could not read \"{}\"", path.display()))?;
        // pyxdg decodes with errors='replace', so legacy encodings still load.
        Self::parse(path, &String::from_utf8_lossy(&bytes))
    }

    /// Loads an entry; on failure only `message` is reported, as uwsm does
    /// (the cause is shown with debug output).
    pub fn load_or(path: &Path, message: impl FnOnce() -> String) -> Result<Self> {
        Self::load(path).map_err(|e| {
            debug!("{e:#}");
            anyhow!(message())
        })
    }

    /// Parses like pyxdg's `IniFile.parse` with debug off: a repeated key
    /// overwrites the earlier value and a repeated group header empties the
    /// group. Lines end at `\n`, `\r\n` or a lone `\r` (Python's universal
    /// newlines).
    pub fn parse(path: &Path, text: &str) -> Result<Self> {
        let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut current: Option<usize> = None;
        for (lineno, raw) in text
            .split("\r\n")
            .flat_map(|l| l.split(['\r', '\n']))
            .enumerate()
        {
            let line = py_strip(raw);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                let name = line.trim_start_matches('[').trim_end_matches(']');
                current = Some(match groups.iter().position(|(g, _)| g == name) {
                    Some(i) => {
                        groups[i].1.clear();
                        i
                    }
                    None => {
                        groups.push((name.to_string(), Vec::new()));
                        groups.len() - 1
                    }
                });
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bail!(
                    "Invalid line {} in \"{}\": {raw}",
                    lineno + 1,
                    path.display()
                );
            };
            let Some(i) = current else {
                bail!("\"{}\" does not start with a group header", path.display());
            };
            let key = py_strip(key).to_string();
            let value = py_strip(value).to_string();
            let entries = &mut groups[i].1;
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key, value)),
            }
        }
        let Some(main) = MAIN_GROUPS
            .into_iter()
            .find(|m| groups.iter().any(|(g, _)| g == m))
        else {
            bail!("\"{}\" has no [Desktop Entry] group", path.display());
        };
        Ok(Self {
            path: path.to_path_buf(),
            groups,
            main: main.to_string(),
        })
    }

    fn group(&self, group: &str) -> Option<&[(String, String)]> {
        self.groups
            .iter()
            .find(|(g, _)| g == group)
            .map(|(_, e)| e.as_slice())
    }

    pub fn has_group(&self, group: &str) -> bool {
        self.group(group).is_some()
    }

    /// Raw value of `key` in `group`, without locale lookup.
    pub fn get_raw_in(&self, group: &str, key: &str) -> Option<&str> {
        self.group(group)?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn get_raw(&self, key: &str) -> Option<&str> {
        self.get_raw_in(&self.main, key)
    }

    pub fn has_key(&self, key: &str) -> bool {
        self.get_raw(key).is_some()
    }

    /// Localized value: `key[locale]` for the first matching locale, else
    /// `key`. As in pyxdg, localized variants count only when `key` itself
    /// is present.
    pub fn get_localized_in(&self, group: &str, key: &str) -> Option<&str> {
        self.get_raw_in(group, key)?;
        for lang in locale_variants() {
            if let Some(v) = self.get_raw_in(group, &format!("{key}[{lang}]")) {
                return Some(v);
            }
        }
        self.get_raw_in(group, key)
    }

    /// Value of a string key in the main group, empty when missing.
    pub fn get(&self, key: &str) -> &str {
        self.get_raw(key).unwrap_or_default()
    }

    pub fn name(&self) -> &str {
        self.get_localized_in(&self.main, "Name")
            .unwrap_or_default()
    }

    pub fn generic_name(&self) -> &str {
        self.get_localized_in(&self.main, "GenericName")
            .unwrap_or_default()
    }

    pub fn comment(&self) -> &str {
        self.get_localized_in(&self.main, "Comment")
            .unwrap_or_default()
    }

    pub fn icon(&self) -> &str {
        self.get_localized_in(&self.main, "Icon")
            .unwrap_or_default()
    }

    pub fn exec(&self) -> &str {
        self.get("Exec")
    }

    pub fn path_key(&self) -> &str {
        self.get("Path")
    }

    pub fn terminal(&self) -> bool {
        self.get("Terminal") == "true"
    }

    pub fn hidden(&self) -> bool {
        self.get("Hidden") == "true"
    }

    /// A list value, split like pyxdg's `getList`.
    pub fn get_list(&self, key: &str) -> Vec<String> {
        pyxdg_list(self.get(key))
    }

    pub fn actions(&self) -> Vec<String> {
        self.get_list("Actions")
    }

    pub fn categories(&self) -> Vec<String> {
        self.get_list("Categories")
    }

    /// `TryExec` passes when it is absent or resolves to an executable.
    pub fn try_exec_ok(&self) -> bool {
        match self.get_raw("TryExec") {
            None => true,
            Some(t) => xdg::which(t).is_some(),
        }
    }

    /// Errors as pyxdg's `validate()` reports them, in file order and without
    /// duplicates (uwsm collects them in a set). pyxdg's warnings, such as a
    /// missing `Exec`, are not reported: uwsm ignores them and checks what it
    /// needs itself.
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors: Vec<String> = Vec::new();
        let mut push = |e: String| {
            if !errors.contains(&e) {
                errors.push(e);
            }
        };
        let main = self.group(&self.main).unwrap_or_default();
        let has = |k: &str| main.iter().any(|(key, _)| key == k);
        if !has("Type") {
            push("Key 'Type' is missing".into());
        }
        if !has("Name") {
            push("Key 'Name' is missing".into());
        }
        let entry_type = self.get_raw("Type").unwrap_or("");
        for (group, entries) in &self.groups {
            let action_group = group.strip_prefix("Desktop Action ").is_some_and(|a| {
                !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            });
            if !(*group == self.main
                || action_group
                || (group.starts_with("X-") && group.is_ascii()))
            {
                push(format!("Invalid Group name: {group}"));
            } else if entries.iter().any(|(k, _)| k == "OnlyShowIn")
                && entries.iter().any(|(k, _)| k == "NotShowIn")
            {
                push("Group may either have OnlyShowIn or NotShowIn, but not both".into());
            }
            for (key, value) in entries {
                check_key(key, value, entry_type, &mut push);
            }
        }
        errors
    }
}

/// pyxdg's `DesktopEntry.checkKey`, errors only.
fn check_key(key: &str, value: &str, entry_type: &str, push: &mut impl FnMut(String)) {
    if key == "Type" {
        if !matches!(
            value,
            "Application"
                | "Link"
                | "Directory"
                | "ServiceType"
                | "Service"
                | "FSDevice"
                | "MimeType"
        ) {
            push(format!(
                "Value of key 'Type' must be Application, Link or Directory, but is '{value}'"
            ));
        }
        return;
    }
    let known = KEYS.iter().find(|spec| {
        key == spec.name
            || (spec.localized && key.strip_prefix(spec.name).is_some_and(is_locale_suffix))
    });
    let Some(&KeySpec {
        check,
        list,
        only_type,
        ..
    }) = known
    else {
        if !is_x_key(key) {
            push(format!("Invalid key: {key}"));
        }
        return;
    };
    let values = if list {
        pyxdg_list(value)
    } else {
        vec![value.to_string()]
    };
    for v in values {
        let valid = match check {
            Check::None => true,
            Check::Str => v.is_ascii(),
            Check::Bool => matches!(v.as_str(), "true" | "false" | "1" | "0"),
            Check::Numeric => is_python_float(&v),
        };
        if !valid {
            let kind = match check {
                Check::Bool => "boolean",
                Check::Numeric => "numeric",
                _ => "string",
            };
            push(format!("'{v}' is not a valid {kind}"));
        }
    }
    if let Some(t) = only_type
        && entry_type != t
    {
        push(format!("Key '{key}' only allowed in Type={t}"));
    }
}

/// `xdg.Locale.regex`: `[lang_COUNTRY.ENCODING@MODIFIER]`, all parts but
/// `lang` optional.
fn is_locale_suffix(s: &str) -> bool {
    let Some(inner) = s.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    let alpha = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphabetic());
    let (rest, modifier) = match inner.split_once('@') {
        Some((r, m)) => (r, Some(m)),
        None => (inner, None),
    };
    let (rest, encoding) = match rest.split_once('.') {
        Some((r, e)) => (r, Some(e)),
        None => (rest, None),
    };
    let (lang, country) = match rest.split_once('_') {
        Some((l, c)) => (l, Some(c)),
        None => (rest, None),
    };
    alpha(lang)
        && country.is_none_or(alpha)
        && encoding.is_none_or(|e| {
            !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && modifier.is_none_or(alpha)
}

/// `re.match("^X-[a-zA-Z0-9-]+", key)`: only the start has to match.
fn is_x_key(key: &str) -> bool {
    key.strip_prefix("X-")
        .and_then(|r| r.chars().next())
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// pyxdg's `IniFile.getList`: splits on unescaped `;`, else `|`, else `,`,
/// and drops one trailing empty item. Escapes stay as written.
fn pyxdg_list(value: &str) -> Vec<String> {
    let unescaped = |sep: char| {
        let mut prev = None;
        value.chars().any(|c| {
            let hit = c == sep && prev != Some('\\');
            prev = Some(c);
            hit
        })
    };
    let Some(sep) = [';', '|', ','].into_iter().find(|&c| unescaped(c)) else {
        return if value.is_empty() {
            Vec::new()
        } else {
            vec![value.to_string()]
        };
    };
    let mut items = vec![String::new()];
    let mut prev = None;
    for c in value.chars() {
        if c == sep && prev != Some('\\') {
            items.push(String::new());
        } else {
            items.last_mut().unwrap().push(c);
        }
        prev = Some(c);
    }
    if items.last().is_some_and(String::is_empty) {
        items.pop();
    }
    items
}

/// Whether Python's `float()` accepts the string.
fn is_python_float(s: &str) -> bool {
    let t = py_strip(s);
    let chars: Vec<char> = t.chars().collect();
    // an underscore is only allowed between two digits
    let underscores_ok = chars.iter().enumerate().all(|(i, &c)| {
        c != '_'
            || (i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
    });
    let t = t.replace('_', "");
    underscores_ok && !t.is_empty() && t.parse::<f64>().is_ok()
}

/// Python's `str.strip()`: Unicode whitespace plus the separators
/// U+001C to U+001F, which `char::is_whitespace` leaves out.
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Locale variants in lookup order, like pyxdg's `xdg.Locale.langs`:
/// `lang_COUNTRY@MOD`, `lang_COUNTRY`, `lang@MOD`, `lang`.
fn locale_variants() -> Vec<String> {
    let raw = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for locale in raw.split(':') {
        if locale.is_empty() || locale == "C" || locale == "POSIX" {
            continue;
        }
        let (rest, modifier) = match locale.split_once('@') {
            Some((r, m)) => (r, Some(m)),
            None => (locale, None),
        };
        let rest = rest.split('.').next().unwrap_or(rest);
        let (lang, country) = match rest.split_once('_') {
            Some((l, c)) => (l, Some(c)),
            None => (rest, None),
        };
        let mut push = |s: String| {
            if !out.contains(&s) {
                out.push(s);
            }
        };
        if let (Some(c), Some(m)) = (country, modifier) {
            push(format!("{lang}_{c}@{m}"));
        }
        if let Some(c) = country {
            push(format!("{lang}_{c}"));
        }
        if let Some(m) = modifier {
            push(format!("{lang}@{m}"));
        }
        push(lang.to_string());
    }
    out
}

/// Port of `entry_expand_str`: expands `\s \n \t \r \\`; other escapes keep the character.
pub fn expand_str(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            out.push(match c {
                's' => ' ',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                other => other,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// Port of `entry_tokenize_exec` (spec §6.1, P-ENTRY-EXEC). Expects an already
/// expanded value.
pub fn tokenize_exec(value: &str) -> Result<Vec<String>> {
    let value = value.trim();
    let mut cmd = Vec::new();
    let mut arg = String::new();
    let mut quoted = false;
    let mut in_space = false;
    let mut escaped = false;
    for c in value.chars() {
        if in_space && c.is_whitespace() {
            continue;
        }
        in_space = false;
        if quoted {
            if escaped {
                arg.push(c);
                escaped = false;
            } else if c == '"' {
                quoted = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '`' || c == '$' {
                bail!("Encountered unescaped {} in Exec", py_repr(&c.to_string()));
            } else {
                arg.push(c);
            }
            continue;
        }
        if c == '"' {
            quoted = true;
            continue;
        }
        if c.is_whitespace() {
            in_space = true;
            cmd.push(std::mem::take(&mut arg));
            continue;
        }
        if "\t\n'\\><~|&;$*?#()`".contains(c) {
            bail!("Encountered unquoted {} in Exec", py_repr(&c.to_string()));
        }
        arg.push(c);
    }
    cmd.push(arg);
    Ok(cmd)
}

/// Name, tokenized Exec and Icon of an entry or one of its actions
/// (port of `entry_action_keys`).
#[derive(Debug, Clone)]
pub struct ActionKeys {
    pub name: String,
    pub exec: Vec<String>,
    pub icon: String,
}

impl DesktopEntry {
    pub fn action_keys(&self, action: Option<&str>) -> Result<ActionKeys> {
        let mut out = ActionKeys {
            name: expand_str(self.name()),
            exec: tokenize_exec(&expand_str(self.exec()))?,
            icon: self.icon().to_string(),
        };
        let Some(action) = action else {
            return Ok(out);
        };
        if !self.actions().iter().any(|a| a == action) {
            bail!(
                "entry \"{}\" does not have action \"{action}\"",
                self.path.display()
            );
        }
        let group = format!("Desktop Action {action}");
        out.name = expand_str(self.get_localized_in(&group, "Name").unwrap_or_default());
        out.exec = tokenize_exec(&expand_str(
            self.get_raw_in(&group, "Exec").unwrap_or_default(),
        ))?;
        if let Some(icon) = self
            .get_localized_in(&group, "Icon")
            .filter(|i| !i.is_empty())
        {
            out.icon = icon.to_string();
        }
        Ok(out)
    }

    /// Port of `check_entry_basic`: validation, Hidden, TryExec, action presence,
    /// and that the Exec binary resolves.
    pub fn check_basic(&self, action: Option<&str>) -> Result<()> {
        let file = self.path.display();
        let errors: Vec<String> = self
            .validation_errors()
            .into_iter()
            .filter(|e| {
                !e.contains(" is not a registered")
                    && !TOLERATED_ERRORS.contains(&e.as_str())
                    && !is_invalid_locale_key_error(e)
            })
            .collect();
        if !errors.is_empty() {
            let mut msg = format!("Entry {file} failed validation:");
            for e in errors {
                msg.push_str(&format!("\n  err: {e}"));
            }
            bail!(msg);
        }
        if self.hidden() {
            bail!("Entry {file} is hidden");
        }
        if !self.try_exec_ok() {
            bail!("Entry {file} is discarded by TryExec");
        }
        let exec = if let Some(action) = action {
            if !self.actions().iter().any(|a| a == action) {
                bail!("Entry {file} has no action {action}");
            }
            if !self.has_group(&format!("Desktop Action {action}")) {
                bail!("Entry {file} has no action group Desktop Action {action}");
            }
            let keys = self.action_keys(Some(action))?;
            if keys.name.is_empty() {
                bail!("Entry {file} action {action} does not have Name");
            }
            if keys.exec.iter().all(String::is_empty) {
                bail!("Entry {file} action {action} does not have Exec");
            }
            keys.exec
        } else {
            if self.exec().is_empty() {
                bail!("Entry {file} does not have Exec");
            }
            tokenize_exec(&expand_str(self.exec()))?
        };
        if xdg::which(&exec[0]).is_none() {
            bail!("Entry {file} points to missing executable {}", exec[0]);
        }
        Ok(())
    }

    /// Port of `check_entry_showin` against `XDG_CURRENT_DESKTOP`.
    pub fn check_showin(&self) -> Result<()> {
        let xcd_raw = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let xcd: Vec<&str> = sane_split(&xcd_raw, ":");
        let only = self.get_list("OnlyShowIn");
        let not = self.get_list("NotShowIn");
        let file = self.path.display();
        if !only.is_empty() && !only.iter().any(|d| xcd.contains(&d.as_str())) {
            bail!("Entry {file} discarded by OnlyShowIn");
        }
        if not.iter().any(|d| xcd.contains(&d.as_str())) {
            bail!("Entry {file} discarded by NotShowIn");
        }
        Ok(())
    }
}

/// uwsm's `^Invalid key: \\w+\\[.+\\]$`.
fn is_invalid_locale_key_error(e: &str) -> bool {
    let Some((base, rest)) = e
        .strip_prefix("Invalid key: ")
        .and_then(|k| k.split_once('['))
    else {
        return false;
    };
    !base.is_empty()
        && base.chars().all(|c| c.is_alphanumeric() || c == '_')
        && rest.len() > 1
        && rest.ends_with(']')
        && !rest.contains('\n')
}

/// `^[a-zA-Z0-9_][a-zA-Z0-9_.-]*.desktop$` (the unescaped `.` matches any char).
pub fn is_valid_entry_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < "x.desktop".len() || !id.ends_with("desktop") {
        return false;
    }
    let ok = |b: &u8| b.is_ascii_alphanumeric() || *b == b'_';
    let stem = &bytes[..bytes.len() - "desktop".len()];
    let (body, _any) = stem.split_at(stem.len() - 1);
    !body.is_empty() && ok(&body[0]) && body[1..].iter().all(|b| ok(b) || *b == b'.' || *b == b'-')
}

/// `^[a-zA-Z0-9-]+$`
pub fn is_valid_action_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// What a parser callback wants [`find_entries`] to do with an entry.
pub enum Found<T> {
    Append(T),
    Extend(Vec<T>),
    Return(T),
    Drop,
}

/// Port of `find_entries` (spec §6.1): walks `subpath` in the XDG data
/// hierarchy, deduplicating by entry ID (first data dir wins).
pub fn find_entries<T>(
    subpath: &str,
    reject_mtimes: Option<&HashMap<PathBuf, f64>>,
    reject_ids: &[String],
    mut parser: impl FnMut(&str, &Path) -> Found<T>,
) -> Vec<T> {
    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::new();
    for data_dir in xdg::load_data_paths(subpath) {
        let mut files = Vec::new();
        walk(&data_dir, &mut files);
        files.sort();
        for path in files {
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(map) = reject_mtimes
                && let Some(cached) = map.get(&path)
                && mtime(&path) == Some(*cached)
            {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&data_dir) else {
                continue;
            };
            let id = rel.to_string_lossy().replace('/', "-");
            if reject_ids.contains(&id) || !is_valid_entry_id(&id) || !seen.insert(id.clone()) {
                continue;
            }
            match parser(&id, &path) {
                Found::Return(v) => return vec![v],
                Found::Append(v) => results.push(v),
                Found::Extend(v) => results.extend(v),
                Found::Drop => {}
            }
        }
    }
    results
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        // follow symlinks like os.walk(followlinks=True)
        if path.is_dir() {
            walk(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// File mtime as float seconds, like `os.path.getmtime`.
pub fn mtime(path: &Path) -> Option<f64> {
    let m = fs::metadata(path).ok()?.modified().ok()?;
    Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64())
}

/// Finds an entry by ID under `subpath` and checks it (port of
/// `entry_parser_by_ids`). A matching entry that fails checks is an error.
pub fn find_entry_by_id(
    subpath: &str,
    id: &str,
    action: Option<&str>,
) -> Result<Option<DesktopEntry>> {
    find_entries(subpath, None, &[], |entry_id, path| {
        if entry_id != id {
            return Found::Drop;
        }
        Found::Return(
            DesktopEntry::load_or(path, || {
                format!("Failed to parse entry \"{id}\" from \"{}\"", path.display())
            })
            .and_then(|e| e.check_basic(action).map(|_| e)),
        )
    })
    .into_iter()
    .next()
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> DesktopEntry {
        DesktopEntry::parse(Path::new("/x/test.desktop"), text).unwrap()
    }

    #[test]
    fn expand() {
        assert_eq!(expand_str("a\\sb\\\\c\\q"), "a b\\cq");
        assert_eq!(expand_str("plain"), "plain");
    }

    #[test]
    fn tokenize() {
        assert_eq!(tokenize_exec("sway").unwrap(), vec!["sway"]);
        assert_eq!(
            tokenize_exec("  foo  --bar \"a b\" \"c\\\"d\" ").unwrap(),
            vec!["foo", "--bar", "a b", "c\"d"]
        );
        assert_eq!(tokenize_exec("").unwrap(), vec![""]);
        assert_eq!(tokenize_exec("a \"\"").unwrap(), vec!["a", ""]);
        assert!(tokenize_exec("a 'b'").is_err());
        assert!(tokenize_exec("a $HOME").is_err());
        assert!(tokenize_exec("a \"$HOME\"").is_err());
        assert_eq!(tokenize_exec("a \"\\$HOME\"").unwrap(), vec!["a", "$HOME"]);
    }

    #[test]
    fn entry_ids() {
        assert!(is_valid_entry_id("sway.desktop"));
        assert!(is_valid_entry_id("org.kde-foo_1.desktop"));
        assert!(is_valid_entry_id("a-desktop")); // unescaped '.' upstream
        assert!(!is_valid_entry_id(".desktop"));
        assert!(!is_valid_entry_id("-a.desktop"));
        assert!(!is_valid_entry_id("a b.desktop"));
        assert!(is_valid_action_id("debug-mode"));
        assert!(!is_valid_action_id("a_b"));
    }

    #[test]
    fn localized_and_lists() {
        let _g = crate::xdg::test_env::lock();
        unsafe {
            std::env::remove_var("LANGUAGE");
            std::env::remove_var("LC_ALL");
            std::env::remove_var("LC_MESSAGES");
            std::env::set_var("LANG", "de_DE.UTF-8");
        }
        let e = entry(
            "[Desktop Entry]\nType=Application\nName=Foo\nName[de]=Fu\nExec=foo\n\
             Categories=A;B\\;C;\nDesktopNames=sway;wlroots\n",
        );
        assert_eq!(e.name(), "Fu");
        // pyxdg keeps escapes as written
        assert_eq!(e.categories(), vec!["A", "B\\;C"]);
        assert_eq!(e.get_list("DesktopNames"), vec!["sway", "wlroots"]);

        // a localized variant without its base key is ignored
        let e = entry(
            "[Desktop Entry]\nType=Application\nName=Foo\nExec=foo\nActions=x;\n\
             [Desktop Action x]\nName[de]=Nur\nExec=foo\n",
        );
        assert_eq!(e.get_localized_in("Desktop Action x", "Name"), None);
    }

    /// Expected lists from pyxdg's `IniFile.getList`.
    #[test]
    fn lists_split_like_pyxdg() {
        for (value, expected) in [
            ("A;B\\;C;", &["A", "B\\;C"][..]),
            ("GNOME,KDE", &["GNOME", "KDE"]),
            ("a|b", &["a", "b"]),
            ("", &[]),
            ("x", &["x"]),
            ("a\\,b,c", &["a\\,b", "c"]),
        ] {
            assert_eq!(pyxdg_list(value), expected, "{value:?}");
        }
    }

    #[test]
    fn kde_header_and_line_endings() {
        let e = entry("[KDE Desktop Entry]\nType=Application\nName=K\nExec=sh\n");
        assert_eq!(e.name(), "K");
        assert_eq!(e.validation_errors(), Vec::<String>::new());
        let both = entry(
            "[Desktop Entry]\nType=Application\nName=D\nExec=sh\n[KDE Desktop Entry]\nName=K\n",
        );
        assert_eq!(both.name(), "D");
        assert_eq!(
            both.validation_errors(),
            vec!["Invalid Group name: KDE Desktop Entry"]
        );
        let cr = entry("[Desktop Entry]\rType=Application\rName=C\r\nExec=sh\n");
        assert_eq!(cr.name(), "C");
        assert_eq!(cr.exec(), "sh");
        assert_eq!(py_strip("\u{1f} x \u{a0}"), "x");
    }

    #[test]
    fn floats_like_python() {
        for ok in [
            "1_0.5e3", "1.5", " 7 ", ".5", "5.", "1e5", "inf", "nan", "1_000",
        ] {
            assert!(is_python_float(ok), "{ok}");
        }
        for bad in ["1_e5", "1_.5", "1e_5", "_1", "1_", "1__0", "abc", ""] {
            assert!(!is_python_float(bad), "{bad}");
        }
    }

    #[test]
    fn validation() {
        let e = entry("[Desktop Entry]\nName=x\nExec=sh\nFoo=1\nDesktopNames=a\nTerminal=yes\n");
        let errs = e.validation_errors();
        assert!(errs.contains(&"Key 'Type' is missing".to_string()));
        assert!(errs.contains(&"Invalid key: Foo".to_string()));
        assert!(errs.contains(&"Invalid key: DesktopNames".to_string()));
        assert!(errs.contains(&"'yes' is not a valid boolean".to_string()));
        assert!(e.check_basic(None).is_err());

        let ok = entry(
            "[Desktop Entry]\nType=Application\nName=x\nExec=sh -c true\nDesktopNames=a\nX-Foo=1\n",
        );
        ok.check_basic(None).unwrap();
    }

    /// Expected errors come from pyxdg 0.28's `validate()`, minus the
    /// "is not a registered" ones rwsm doesn't produce (uwsm ignores them).
    #[test]
    fn validation_matches_pyxdg() {
        let h = "[Desktop Entry]\nType=Application\nName=E\n";
        let cases: &[(String, &[&str])] = &[
            (format!("{h}Exec=a\nExec=b\n"), &[]),
            (
                format!("{h}Exec=a\n[Desktop Entry]\nType=Application\nName=F\nExec=b\n"),
                &[],
            ),
            (
                format!("{h}Exec=fakecomp ñ\n"),
                &["'fakecomp ñ' is not a valid string"],
            ),
            (format!("{h}Exec=a\nTerminal=1\n"), &[]),
            (
                format!("{h}Exec=a\nImplements=org.foo\n"),
                &["Invalid key: Implements"],
            ),
            (
                format!("{h}Exec=a\nURL=http://x\n"),
                &["Key 'URL' only allowed in Type=URL"],
            ),
            (
                "[Desktop Entry]\nType=Bogus\nName=B\nExec=a\n".into(),
                &[
                    "Key 'Exec' only allowed in Type=Application",
                    "Value of key 'Type' must be Application, Link or Directory, but is 'Bogus'",
                ],
            ),
            (
                format!("{h}Exec=a\nOnlyShowIn=GNOME;\nNotShowIn=KDE;\n"),
                &["Group may either have OnlyShowIn or NotShowIn, but not both"],
            ),
            (
                format!("{h}Exec=a\n[Bad Group]\nName=x\n"),
                &["Invalid Group name: Bad Group"],
            ),
            (
                format!("{h}Exec=a\nActions=a;b;\n[Desktop Action a]\nName=A\nExec=a\n"),
                &[],
            ),
            (
                format!("{h}Exec=a\nInitialPreference=abc\n"),
                &["'abc' is not a valid numeric"],
            ),
            (format!("{h}Exec=a\nInitialPreference=1_0.5e3\n"), &[]),
            (
                "[Desktop Entry]\nExec=a\n".into(),
                &[
                    "Key 'Exec' only allowed in Type=Application",
                    "Key 'Name' is missing",
                    "Key 'Type' is missing",
                ],
            ),
            (
                format!("{h}Exec=a\nFoo-bar[x]=1\n"),
                &["Invalid key: Foo-bar[x]"],
            ),
            (format!("{h}Exec=a\nName[de_DE.UTF-8@euro]=x\n"), &[]),
            (
                format!("{h}Exec=a\nExec[de]=b\n"),
                &["Invalid key: Exec[de]"],
            ),
            (
                format!("{h}Exec=a\nOnlyShowIn=a|b|ü\n"),
                &["'ü' is not a valid string"],
            ),
            (
                format!("{h}Exec=a\nOnlyShowIn=a\\;b;ü,c\n"),
                &["'ü,c' is not a valid string"],
            ),
            (format!("{h}Exec=a\nX-=1\n"), &["Invalid key: X-"]),
            (h.to_string(), &[]),
        ];
        for (text, expected) in cases {
            let mut errs = entry(text).validation_errors();
            errs.sort();
            assert_eq!(errs, *expected, "{text:?}");
        }
    }

    #[test]
    fn parse_failure_hides_cause() {
        let dir = std::env::temp_dir().join(format!("rwsm-test-{}", crate::env::random_hex(8)));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.desktop");
        fs::write(&path, "[Desktop Entry]\nnot a key line\n").unwrap();
        let err = DesktopEntry::load_or(&path, || "Failed to parse".into()).unwrap_err();
        assert_eq!(format!("{err:#}"), "Failed to parse");
        fs::write(
            &path,
            b"[Desktop Entry]\nType=Application\nName=caf\xe9\nExec=a\n",
        )
        .unwrap();
        assert_eq!(DesktopEntry::load(&path).unwrap().name(), "caf\u{fffd}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn locale_key_error_filter_matches_uwsm_regex() {
        assert!(is_invalid_locale_key_error("Invalid key: Exec[de]"));
        assert!(!is_invalid_locale_key_error("Invalid key: Foo-bar[x]"));
        assert!(!is_invalid_locale_key_error("Invalid key: Exec[]"));
        assert!(!is_invalid_locale_key_error("Invalid key: Exec"));
    }

    #[test]
    fn actions() {
        let e = entry(
            "[Desktop Entry]\nType=Application\nName=Hypr\nExec=Hyprland\nActions=dbg;\n\n\
             [Desktop Action dbg]\nName=Debug\nExec=sh --debug\n",
        );
        let k = e.action_keys(Some("dbg")).unwrap();
        assert_eq!(k.name, "Debug");
        assert_eq!(k.exec, vec!["sh", "--debug"]);
        e.check_basic(Some("dbg")).unwrap();
        assert!(e.check_basic(Some("nope")).is_err());
    }
}
