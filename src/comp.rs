//! Compositor identity: main argument classification and `fill_comp_globals`
//! (spec §4.2).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::cli::{Cli, Mode, WmMeta};
use crate::config::BIN_NAME;
use crate::desktop_entry::{self, DesktopEntry, expand_str, is_valid_action_id, is_valid_entry_id};
use crate::escape::simple_systemd_escape;
use crate::misc::sane_split;
use crate::{debug, output, xdg};

/// Lexical `os.path.normpath`.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            p => parts.push(p),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// `os.path.expanduser` for `~` and `~/...`.
pub fn expanduser(path: &str) -> String {
    if (path == "~" || path.starts_with("~/"))
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}{}", &path[1..]);
    }
    path.to_string()
}

/// `os.path.abspath`.
pub fn abspath(path: &str) -> String {
    if path.starts_with('/') {
        normpath(path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        normpath(&format!("{}/{path}", cwd.display()))
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn access(path: &Path, mode: libc::c_int) -> bool {
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    unsafe { libc::access(c.as_ptr(), mode) == 0 }
}

/// Port of uwsm's `MainArg`: a desktop entry ID (maybe with action, maybe a
/// path), or an executable (maybe a path).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MainArg {
    pub entry_id: Option<String>,
    pub entry_action: Option<String>,
    pub path: Option<String>,
    pub executable: Option<String>,
}

impl MainArg {
    pub fn new(arg: Option<&str>) -> Result<Self> {
        let mut out = Self::default();
        let Some(arg) = arg else { return Ok(out) };

        if !(arg.ends_with(".desktop") || arg.contains(".desktop:")) {
            out.executable = Some(arg.to_string());
            if arg.contains('/') {
                out.path = Some(normpath(&expanduser(arg)));
            }
            return Ok(out);
        }

        let mut entry_id = arg.to_string();
        if let Some((id, action)) = arg.split_once(".desktop:") {
            entry_id = format!("{id}.desktop");
            if !action.is_empty() {
                if !is_valid_action_id(action) {
                    bail!("Invalid Desktop Entry Action \"{action}\"");
                }
                out.entry_action = Some(action.to_string());
            }
        }

        if entry_id.contains('/') {
            let path = normpath(&expanduser(&entry_id));
            entry_id = basename(&path).to_string();
            let abs = abspath(&path);
            for data_dir in xdg::load_data_paths("applications") {
                let dir = normpath(&data_dir.to_string_lossy());
                if let Some(rel) = abs.strip_prefix(&format!("{dir}/")) {
                    entry_id = rel.replace('/', "-");
                    break;
                }
            }
            out.path = Some(path);
        }

        if !is_valid_entry_id(&entry_id) {
            if out.path.is_none() && Path::new(&entry_id).is_file() {
                out.path = Some(format!("./{entry_id}"));
            } else if out.path.is_none() {
                bail!("Invalid Desktop Entry ID \"{entry_id}\"");
            }
            output::warning_notify(
                &format!("Invalid Desktop Entry ID \"{entry_id}\""),
                output::Notify::IfNotTty,
            );
        }
        out.entry_id = Some(entry_id);
        Ok(out)
    }

    pub fn check_exec(&self) -> Result<()> {
        let Some(exe) = &self.executable else {
            bail!("Argument is not an executable: {self:?}");
        };
        if xdg::which(exe).is_none() {
            bail!("\"{exe}\" not found or is not executable!");
        }
        Ok(())
    }

    pub fn check_path(&self) -> Result<()> {
        let Some(path) = &self.path else {
            bail!("Argument is not a path: {self:?}");
        };
        let p = Path::new(path);
        if !p.is_file() {
            bail!("Path \"{path}\" does not exist!");
        }
        if !access(p, libc::R_OK) {
            bail!("Path \"{path}\" is not readable!");
        }
        if self.executable.is_some() && !access(p, libc::X_OK) {
            bail!("Path \"{path}\" is not executable!");
        }
        debug!("Path {path} OK");
        Ok(())
    }
}

/// `^[a-zA-Z0-9_:.-]+$`
pub fn is_valid_wm_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:.-".contains(&b))
}

/// `^[a-zA-Z0-9_.-]+(:[a-zA-Z0-9_.-]+)*$`
pub fn is_valid_desktop_names(names: &str) -> bool {
    !names.is_empty()
        && names.split(':').all(|n| {
            !n.is_empty()
                && n.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        })
}

/// `re.sub(r"(^[^a-zA-Z]|[^a-zA-Z0-9_])+", "_", name).lower()`
pub fn bin_id(bin_name: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for (i, c) in bin_name.chars().enumerate() {
        let bad = if i == 0 {
            !c.is_ascii_alphabetic()
        } else {
            !(c.is_ascii_alphanumeric() || c == '_')
        };
        // The regex's first alternative only matches at position 0, but a run
        // started there continues through following invalid characters.
        if bad {
            if !in_run {
                out.push('_');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out.to_lowercase()
}

/// Which command is filling the compositor globals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompMode {
    Start,
    PrepareEnv,
    Exec,
}

/// Port of uwsm's `CompGlobals`.
#[derive(Debug, Clone, Default)]
pub struct Comp {
    pub cmdline: Vec<String>,
    pub cli_args: Vec<String>,
    pub id: String,
    pub id_unit_string: String,
    pub bin_name: String,
    pub bin_id: String,
    pub desktop_names: Vec<String>,
    pub cli_desktop_names: Vec<String>,
    pub cli_desktop_names_exclusive: bool,
    pub name: String,
    pub description: String,
    /// Hardcode mode, possibly forced by a path argument or inherited from a nested entry.
    pub hardcode: bool,
}

/// Input for [`fill_comp_globals`].
pub struct CompInput<'a> {
    pub mode: CompMode,
    /// `start`: the command line; `aux`: the raw command line after the ID.
    pub wm_cmdline: &'a [String],
    /// `aux` only.
    pub wm_id: Option<&'a str>,
    pub meta: &'a WmMeta,
    pub hardcode: bool,
}

fn dedup(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

fn split_names(s: &str) -> Vec<String> {
    sane_split(s, ":").into_iter().map(String::from).collect()
}

fn load_entry(main_arg: &MainArg) -> Result<DesktopEntry> {
    let id = main_arg.entry_id.as_deref().unwrap_or_default();
    let action = main_arg.entry_action.as_deref();
    if let Some(path) = &main_arg.path {
        let entry = DesktopEntry::load(Path::new(path))
            .with_context(|| format!("Failed to parse entry \"{id}\" from \"{path}\"!"))?;
        entry.check_basic(action)?;
        Ok(entry)
    } else {
        desktop_entry::find_entry_by_id("wayland-sessions", id, action)?
            .with_context(|| format!("Could not find entry \"{id}\""))
    }
}

/// Port of `fill_comp_globals` (spec §4.2).
pub fn fill_comp_globals(input: CompInput) -> Result<Comp> {
    let mut comp = Comp {
        hardcode: input.hardcode,
        ..Default::default()
    };
    let meta = input.meta;
    let uses_meta = matches!(input.mode, CompMode::Start | CompMode::PrepareEnv);

    let mut main_arg;
    match input.mode {
        CompMode::Start => {
            let first = &input.wm_cmdline[0];
            comp.id = basename(first).to_string();
            main_arg = MainArg::new(Some(first))?;
            if main_arg.path.is_some() {
                debug!("hardcode mode due to main argument being a path");
                comp.hardcode = true;
            }
        }
        CompMode::PrepareEnv | CompMode::Exec => {
            let id = input.wm_id.unwrap_or_default();
            comp.id = id.to_string();
            main_arg = MainArg::new(Some(id))?;
            if main_arg.path.is_some() {
                bail!("Aux Compositor ID argument cannot be a path");
            }
            if input.wm_cmdline.first().is_some_and(|a| !a.is_empty()) {
                main_arg = MainArg::new(Some(&input.wm_cmdline[0]))?;
                comp.cmdline = input.wm_cmdline.to_vec();
            }
        }
    }

    if !is_valid_wm_id(&comp.id) {
        bail!(
            "\"{}\" does not conform to \"^[a-zA-Z0-9_:.-]+$\" pattern!",
            comp.id
        );
    }
    comp.id_unit_string = simple_systemd_escape(&comp.id, false);

    if input.mode == CompMode::Exec && !comp.cmdline.is_empty() {
        return Ok(comp);
    }

    if main_arg.path.is_some() {
        main_arg.check_path()?;
    }

    let login_xcd = || -> String {
        crate::env::load_env("env_login", false)
            .ok()
            .and_then(|e| e.get("XDG_CURRENT_DESKTOP").cloned())
            .unwrap_or_default()
    };
    let start_xcd = || std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();

    if main_arg.entry_id.is_some() {
        debug!("Main arg is a Desktop Entry: {:?}", main_arg.entry_id);
        let mut entry = load_entry(&main_arg)?;
        let keys = entry.action_keys(main_arg.entry_action.as_deref())?;
        if comp.cmdline.is_empty() {
            comp.cmdline = keys.exec;
        }
        comp.bin_name = basename(&comp.cmdline[0]).to_string();

        // An entry whose Exec runs uwsm itself (spec P-SELF-ENTRY).
        let mut nested: Option<Cli> = None;
        if comp.bin_name == BIN_NAME {
            let result = (|| -> Result<()> {
                if comp.cmdline.get(1).map(String::as_str) != Some("start") {
                    bail!(
                        "Entry \"{}\" uses {BIN_NAME}, but the second argument \"{}\" is not \"start\"!",
                        comp.id,
                        comp.cmdline.get(1).cloned().unwrap_or_default()
                    );
                }
                comp.cmdline.remove(0);
                output::normal(&format!(
                    "Entry \"{}\" uses {BIN_NAME}, reparsing args...",
                    comp.id
                ));
                let parsed = Cli::try_parse_from(
                    std::iter::once(BIN_NAME.to_string()).chain(comp.cmdline.iter().cloned()),
                )
                .map_err(|e| anyhow::anyhow!("{}", e.render().to_string().trim()))?;
                let Mode::Start(nested_args) = &parsed.mode else {
                    unreachable!("checked above")
                };
                let nested_main = MainArg::new(Some(&nested_args.wm_cmdline[0]))?;
                if nested_main.entry_id.is_some()
                    && (&main_arg.entry_id, &main_arg.entry_action)
                        == (&nested_main.entry_id, &nested_main.entry_action)
                {
                    bail!(
                        "Entry \"{}\" uses {BIN_NAME} that points to itself!",
                        comp.id
                    );
                }
                if nested_args.dry_run {
                    bail!("Entry \"{}\" uses {BIN_NAME} in \"dry run\" mode!", comp.id);
                }
                if nested_args.only_generate {
                    bail!(
                        "Entry \"{}\" uses {BIN_NAME} in \"only generate\" mode!",
                        comp.id
                    );
                }
                let dn = &nested_args.meta.desktop_names;
                if !dn.is_empty() && !is_valid_desktop_names(dn) {
                    bail!(
                        "Entry \"{}\" uses {BIN_NAME} with malformed desktop names: \"{dn}\"!",
                        comp.id
                    );
                }
                let mut nested_cmdline = nested_args.wm_cmdline.clone();
                if nested_main.entry_id.is_some() {
                    entry = load_entry(&nested_main)?;
                    let inner = entry.action_keys(nested_main.entry_action.as_deref())?.exec;
                    if basename(&inner[0]) == BIN_NAME {
                        bail!(
                            "Entry \"{}\" uses {BIN_NAME} that points to another Entry that also uses {BIN_NAME}!",
                            comp.id
                        );
                    }
                    nested_cmdline = inner
                        .into_iter()
                        .chain(nested_cmdline.into_iter().skip(1))
                        .collect();
                }
                comp.cmdline = nested_cmdline;
                comp.bin_name = basename(&comp.cmdline[0]).to_string();
                nested = Some(parsed);
                Ok(())
            })();
            if let Err(e) = result {
                output::error(&format!("{e:#}"));
                std::process::exit(1);
            }
        }
        let nested_args = nested.as_ref().and_then(|c| match &c.mode {
            Mode::Start(s) => Some(s),
            _ => None,
        });

        // Extra CLI args are appended without % field expansion, like upstream.
        comp.cmdline
            .extend(input.wm_cmdline.iter().skip(1).cloned());
        debug!("cmdline {:?}", comp.cmdline);

        if uses_meta {
            if !meta.desktop_names.is_empty() && !is_valid_desktop_names(&meta.desktop_names) {
                output::error(&format!(
                    "Got malformed desktop names: \"{}\"!",
                    meta.desktop_names
                ));
                std::process::exit(1);
            }
            if meta.desktop_names_exclusive {
                if meta.desktop_names.is_empty() {
                    output::error(
                        "Requested exclusive desktop names (\"-e\") but no desktop names were given via \"-D\"!",
                    );
                    std::process::exit(1);
                }
                comp.desktop_names = split_names(&meta.desktop_names);
            } else if let Some(n) = nested_args.filter(|n| n.meta.desktop_names_exclusive) {
                if n.meta.desktop_names.is_empty() {
                    bail!(
                        "{BIN_NAME} in entry \"{}\" requests exclusive desktop names (\"-e\") but has no desktop names listed via \"-D\"!",
                        comp.id
                    );
                }
                comp.desktop_names = split_names(&n.meta.desktop_names);
            } else {
                let xcd = if input.mode == CompMode::Start {
                    start_xcd()
                } else {
                    login_xcd()
                };
                comp.desktop_names.extend(split_names(&xcd));
                comp.desktop_names.extend(entry.get_list("DesktopNames"));
                if let Some(n) = nested_args {
                    comp.desktop_names
                        .extend(split_names(&n.meta.desktop_names));
                }
                comp.desktop_names.extend(split_names(&meta.desktop_names));
                if comp.desktop_names.is_empty() {
                    comp.desktop_names.push(comp.bin_name.clone());
                }
            }

            comp.name = if !meta.wm_name.is_empty() {
                meta.wm_name.clone()
            } else if let Some(n) = nested_args.filter(|n| !n.meta.wm_name.is_empty()) {
                n.meta.wm_name.clone()
            } else {
                [expand_str(entry.name()), expand_str(entry.generic_name())]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" - ")
            };
            if !meta.wm_comment.is_empty() {
                comp.description = meta.wm_comment.clone();
            } else if let Some(n) = nested_args.filter(|n| !n.meta.wm_comment.is_empty()) {
                comp.description = n.meta.wm_comment.clone();
            } else if !entry.comment().is_empty() {
                comp.description = expand_str(entry.comment());
            }

            if input.mode == CompMode::Start && nested_args.is_some_and(|n| n.hardcode) {
                comp.hardcode = true;
            }
        }

        main_arg = MainArg::new(Some(&comp.cmdline[0]))?;
        if main_arg.path.is_none() {
            main_arg.check_exec()?;
        } else {
            main_arg.check_path()?;
        }
    } else if let Some(exe) = main_arg.executable.clone() {
        debug!("Main arg is an executable: {exe}");
        if main_arg.path.is_none() {
            main_arg.check_exec()?;
        } else {
            main_arg.check_path()?;
        }
        if comp.cmdline.is_empty() {
            comp.cmdline = input.wm_cmdline.to_vec();
        }
        if comp.cmdline.is_empty() {
            comp.cmdline = vec![exe];
        } else if comp.cmdline[0].is_empty() {
            comp.cmdline[0] = exe;
        }
        comp.bin_name = basename(&comp.cmdline[0]).to_string();

        if uses_meta {
            if meta.desktop_names_exclusive {
                comp.desktop_names = split_names(&meta.desktop_names);
            } else {
                let xcd = if input.mode == CompMode::Start {
                    start_xcd()
                } else {
                    login_xcd()
                };
                comp.desktop_names.extend(split_names(&xcd));
                comp.desktop_names.extend(split_names(&meta.desktop_names));
                if comp.desktop_names.is_empty() {
                    comp.desktop_names.push(comp.bin_name.clone());
                }
            }
            comp.name = meta.wm_name.clone();
            comp.description = meta.wm_comment.clone();
        }
    } else {
        bail!("Could not determine or parse main argument");
    }

    if main_arg.entry_id.is_some() {
        bail!("Could not parse {main_arg:?} down to executable!");
    }

    if input.mode == CompMode::Start && (comp.hardcode || main_arg.path.is_some()) {
        let target = match (&main_arg.path, &main_arg.executable) {
            (Some(p), _) => p.clone(),
            (None, Some(e)) => xdg::which(e)
                .map(|p| p.to_string_lossy().into_owned())
                .with_context(|| format!("\"{e}\" not found"))?,
            _ => unreachable!(),
        };
        comp.cmdline[0] = abspath(&target);
    }

    comp.cli_args = input.wm_cmdline.iter().skip(1).cloned().collect();

    if uses_meta {
        comp.cli_desktop_names = dedup(split_names(&meta.desktop_names));
        comp.cli_desktop_names_exclusive = meta.desktop_names_exclusive;
        comp.desktop_names = dedup(std::mem::take(&mut comp.desktop_names));
    }

    comp.bin_id = bin_id(&comp.bin_name);
    Ok(comp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normpath_like_python() {
        assert_eq!(normpath("a//b/./c/.."), "a/b");
        assert_eq!(normpath("/../a"), "/a");
        assert_eq!(normpath("../a"), "../a");
        assert_eq!(normpath("./"), ".");
    }

    #[test]
    fn bin_ids() {
        assert_eq!(bin_id("Hyprland"), "hyprland");
        assert_eq!(bin_id("start-hyprland"), "start_hyprland");
        assert_eq!(bin_id("niri-session"), "niri_session");
        assert_eq!(bin_id("9wm"), "_wm");
        assert_eq!(bin_id("-.x"), "_x");
        assert_eq!(bin_id("a--b"), "a_b");
    }

    #[test]
    fn main_arg_kinds() {
        let a = MainArg::new(Some("sway")).unwrap();
        assert_eq!(a.executable.as_deref(), Some("sway"));
        assert!(a.path.is_none());

        let a = MainArg::new(Some("/usr/bin//sway")).unwrap();
        assert_eq!(a.path.as_deref(), Some("/usr/bin/sway"));

        let a = MainArg::new(Some("hyprland.desktop:debug")).unwrap();
        assert_eq!(a.entry_id.as_deref(), Some("hyprland.desktop"));
        assert_eq!(a.entry_action.as_deref(), Some("debug"));

        assert!(MainArg::new(Some("hyprland.desktop:bad_action")).is_err());
        assert!(MainArg::new(Some("bad id.desktop")).is_err());
    }

    #[test]
    fn validators() {
        assert!(is_valid_wm_id("Hyprland.desktop:debug"));
        assert!(!is_valid_wm_id("a b"));
        assert!(is_valid_desktop_names("sway:wlroots"));
        assert!(!is_valid_desktop_names("sway::wlroots"));
        assert!(!is_valid_desktop_names(":x"));
    }
}
