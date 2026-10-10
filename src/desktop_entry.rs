//! Desktop entry parsing and checks (spec §6.1), covering what uwsm uses from
//! pyxdg's `DesktopEntry` plus uwsm's own value expansion and Exec tokenizer.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::misc::sane_split;
use crate::xdg;

const MAIN_GROUP: &str = "Desktop Entry";

/// Keys pyxdg knows for the main group. Other keys that are not `X-`
/// prefixed produce an `Invalid key: K` validation error, as in pyxdg.
const KNOWN_KEYS: &[&str] = &[
    "Type",
    "Version",
    "Name",
    "GenericName",
    "NoDisplay",
    "Comment",
    "Icon",
    "Hidden",
    "OnlyShowIn",
    "NotShowIn",
    "TryExec",
    "Exec",
    "Path",
    "Terminal",
    "MimeType",
    "Categories",
    "StartupNotify",
    "StartupWMClass",
    "URL",
    "Actions",
    "Keywords",
    "Implements",
    // deprecated keys pyxdg only warns about
    "Encoding",
    "MiniIcon",
    "TerminalOptions",
    "DefaultApp",
    "Protocols",
    "Extensions",
    "BinaryPattern",
    "MapNotify",
    "SwallowTitle",
    "SwallowExec",
    "SortOrder",
    "FilePattern",
];

const BOOLEAN_KEYS: &[&str] = &["NoDisplay", "Hidden", "Terminal", "StartupNotify"];

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
    /// Problems found while parsing, reported by [`DesktopEntry::validation_errors`].
    parse_errors: Vec<String>,
}

impl DesktopEntry {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("Could not read \"{}\"", path.display()))?;
        Self::parse(path, &text)
    }

    pub fn parse(path: &Path, text: &str) -> Result<Self> {
        let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut parse_errors = Vec::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if groups.iter().any(|(g, _)| g == name) {
                    parse_errors.push(format!("Duplicate group: {name}"));
                }
                groups.push((name.to_string(), Vec::new()));
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bail!(
                    "Invalid line {} in \"{}\": {raw}",
                    lineno + 1,
                    path.display()
                );
            };
            let Some((_, entries)) = groups.last_mut() else {
                bail!("\"{}\" does not start with a group header", path.display());
            };
            let key = key.trim().to_string();
            if entries.iter().any(|(k, _)| *k == key) {
                parse_errors.push(format!("Duplicate key: {key}"));
                continue;
            }
            entries.push((key, value.trim().to_string()));
        }
        if !groups.iter().any(|(g, _)| g == MAIN_GROUP) {
            bail!("\"{}\" has no [{MAIN_GROUP}] group", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
            groups,
            parse_errors,
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
        self.get_raw_in(MAIN_GROUP, key)
    }

    pub fn has_key(&self, key: &str) -> bool {
        self.get_raw(key).is_some()
    }

    /// Localized value: `key[locale]` for the first matching locale, else `key`.
    pub fn get_localized_in(&self, group: &str, key: &str) -> Option<&str> {
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
        self.get_localized_in(MAIN_GROUP, "Name")
            .unwrap_or_default()
    }

    pub fn generic_name(&self) -> &str {
        self.get_localized_in(MAIN_GROUP, "GenericName")
            .unwrap_or_default()
    }

    pub fn comment(&self) -> &str {
        self.get_localized_in(MAIN_GROUP, "Comment")
            .unwrap_or_default()
    }

    pub fn icon(&self) -> &str {
        self.get_localized_in(MAIN_GROUP, "Icon")
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

    /// A `;`-separated list value (`\;` escapes a separator).
    pub fn get_list(&self, key: &str) -> Vec<String> {
        split_list(self.get(key))
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

    /// Errors in the style of pyxdg's `validate()`, for the checks uwsm relies on.
    pub fn validation_errors(&self) -> Vec<String> {
        let mut errors = self.parse_errors.clone();
        let main = self.group(MAIN_GROUP).unwrap_or_default();
        let has = |k: &str| main.iter().any(|(key, _)| key == k);
        match self.get_raw("Type") {
            None => errors.push("Key 'Type' is missing".into()),
            Some("Application" | "Link" | "Directory") => {}
            Some("ServiceType" | "Service" | "FSDevice") => {}
            Some(other) => errors.push(format!("'{other}' is not a valid type")),
        }
        if !has("Name") {
            errors.push("Key 'Name' is missing".into());
        }
        if self.get_raw("Type") == Some("Application") && !has("Exec") && !has("DBusActivatable") {
            errors.push("Key 'Exec' is missing".into());
        }
        for (key, value) in main {
            if key.starts_with("X-") {
                continue;
            }
            let base = key.split('[').next().unwrap_or(key);
            if key.contains('[') {
                if !KNOWN_KEYS.contains(&base) {
                    errors.push(format!("Invalid key: {key}"));
                }
                continue;
            }
            if !KNOWN_KEYS.contains(&base) {
                errors.push(format!("Invalid key: {key}"));
            } else if BOOLEAN_KEYS.contains(&base) && value != "true" && value != "false" {
                errors.push(format!("'{value}' is not a valid boolean"));
            }
        }
        for action in self.actions() {
            if !self.has_group(&format!("Desktop Action {action}")) {
                errors.push(format!("Action group for action '{action}' is missing"));
            }
        }
        errors
    }
}

fn split_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&';') => {
                current.push(';');
                chars.next();
            }
            ';' => items.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        items.push(current);
    }
    items
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
                bail!("Encountered unescaped {c:?} in Exec");
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
            bail!("Encountered unquoted {c:?} in Exec");
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

fn is_invalid_locale_key_error(e: &str) -> bool {
    e.strip_prefix("Invalid key: ")
        .is_some_and(|k| k.contains('[') && k.ends_with(']'))
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
            DesktopEntry::load(path)
                .with_context(|| {
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
        assert_eq!(e.categories(), vec!["A", "B;C"]);
        assert_eq!(e.get_list("DesktopNames"), vec!["sway", "wlroots"]);
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
