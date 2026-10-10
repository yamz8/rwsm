//! Default compositor selection via whiptail (spec §4.6).

use std::fs;
use std::io::IsTerminal;
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

use crate::config::{BIN_NAME, WHIPTAIL_BIN};
use crate::desktop_entry::{DesktopEntry, Found, expand_str, find_entries, tokenize_exec};
use crate::{debug, output, xdg};

/// First non-blank line of the first existing `uwsm/default-id` in config
/// paths, then in data paths.
pub fn get_default_comp_entry() -> String {
    let resource = format!("{BIN_NAME}/default-id");
    let file = xdg::load_config_paths(&resource)
        .into_iter()
        .chain(xdg::load_data_paths(&resource))
        .find(|p| p.is_file());
    let Some(file) = file else {
        return String::new();
    };
    match fs::read_to_string(&file) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_string(),
        Err(e) => {
            output::error(&e.to_string());
            String::new()
        }
    }
}

/// Writes `$XDG_CONFIG_HOME/uwsm/default-id`.
pub fn save_default_comp_entry(default: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        output::ok(&format!("Would save default compositor ID: {default}."));
        return Ok(());
    }
    let dir = xdg::config_home().join(BIN_NAME);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("default-id"), format!("{default}\n"))?;
    output::ok(&format!("Saved default compositor ID: {default}."));
    Ok(())
}

/// The shared `select`/`start select|default` flow: read the default, run the
/// menu (or just confirm the default), save a changed choice. Returns an empty
/// string when nothing was selected.
pub fn select_or_default(just_confirm: bool, dry_run: bool) -> Result<String> {
    let default = get_default_comp_entry();
    let selected = select_comp_entry(&default, just_confirm)?;
    if !selected.is_empty() {
        if selected == default {
            output::normal(&format!("Default compositor ID unchanged: {selected}."));
        } else {
            save_default_comp_entry(&selected, dry_run)?;
        }
    }
    Ok(selected)
}

/// `uwsm select`.
pub fn select_main() -> u8 {
    match select_or_default(false, false) {
        Ok(id) if !id.is_empty() => 0,
        Ok(_) => {
            output::warning("No compositor was selected.");
            1
        }
        Err(e) => {
            output::error(&format!("{e:#}"));
            1
        }
    }
}

fn terminal_columns() -> Option<usize> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    (rc == 0).then_some(ws.ws_col as usize)
}

/// `textwrap.shorten(text, width)` with the default " [...]" placeholder.
/// Breaks on whitespace only (Python also breaks after hyphens; this only
/// affects how a long comment is truncated in the menu).
fn shorten(text: &str, width: isize) -> Result<String> {
    const PLACEHOLDER: &str = " [...]";
    let words: Vec<&str> = text.split_whitespace().collect();
    let collapsed = words.join(" ");
    if collapsed.is_empty() {
        return Ok(collapsed);
    }
    if width <= 0 {
        bail!("invalid width {width} (must be > 0)");
    }
    let width = width as usize;
    if PLACEHOLDER.trim_start().chars().count() > width {
        bail!("placeholder too large for max width");
    }
    if collapsed.chars().count() <= width {
        return Ok(collapsed);
    }
    let mut out = String::new();
    let mut len = 0;
    for word in words {
        let add = word.chars().count() + usize::from(!out.is_empty());
        if len + add + PLACEHOLDER.len() > width {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        len += add;
    }
    if out.is_empty() {
        Ok(PLACEHOLDER.trim_start().to_string())
    } else {
        Ok(out + PLACEHOLDER)
    }
}

fn ljust(s: &str, width: usize) -> String {
    let pad = width.saturating_sub(s.chars().count());
    format!("{s}{}", " ".repeat(pad))
}

/// Port of `select_comp_entry`.
pub fn select_comp_entry(default: &str, just_confirm: bool) -> Result<String> {
    let mut entries: Vec<(String, String, DesktopEntry)> = find_entries(
        "wayland-sessions",
        None,
        &[],
        |id, path| match DesktopEntry::load(path) {
            Ok(entry) if entry.check_basic(None).is_ok() => Found::Append((
                id.trim_end_matches(".desktop").to_string(),
                id.to_string(),
                entry,
            )),
            Ok(_) => Found::Drop,
            Err(_) => {
                debug!("failed parsing {}, skipping", path.display());
                Found::Drop
            }
        },
    );
    entries.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

    // (tag, description, comment)
    let mut raw: Vec<(String, String, String)> = Vec::new();
    for (_, id, entry) in &entries {
        let name = expand_str(entry.name());
        let generic_name = expand_str(entry.generic_name());
        let description = [name, generic_name]
            .into_iter()
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        raw.push((id.clone(), description, expand_str(entry.comment())));

        for action in entry.actions() {
            debug!("parsing action {action}");
            let group = format!("Desktop Action {action}");
            if !entry.has_group(&group) {
                continue;
            }
            let exec = entry.get_raw_in(&group, "Exec").unwrap_or_default();
            let name = entry.get_localized_in(&group, "Name").unwrap_or_default();
            if exec.is_empty() || name.is_empty() {
                continue;
            }
            let argv = tokenize_exec(&expand_str(exec))?;
            let Some(bin) = argv.first() else {
                bail!("list index out of range");
            };
            if xdg::which(bin).is_none() {
                continue;
            }
            raw.push((
                format!("{id}:{action}"),
                format!(" └▶ {}", expand_str(name)),
                String::new(),
            ));
        }
    }

    let desc_len = raw.iter().map(|c| c.1.chars().count()).max().unwrap_or(0);
    let col = terminal_columns().unwrap_or_else(|| {
        output::warning("Could not get terminal width, assuming 128");
        128
    });

    let mut choices: Vec<String> = Vec::new();
    for (tag, description, comment) in &raw {
        choices.push(tag.clone());
        if comment.is_empty() {
            choices.push(description.clone());
        } else {
            let width = col as isize - desc_len as isize - 10;
            choices.push(format!(
                "{}  {}",
                ljust(description, desc_len),
                shorten(comment, width)?
            ));
        }
    }
    if choices.is_empty() {
        bail!("No choices found");
    }

    let mut default = default.to_string();
    if !default.is_empty() && !choices.iter().step_by(2).any(|c| *c == default) {
        output::warning(&format!(
            "Default \"{default}\" was not found in wayland-sessions."
        ));
        default.clear();
    }
    if !default.is_empty() && just_confirm {
        return Ok(default);
    }

    let Some(whiptail) = xdg::which(WHIPTAIL_BIN) else {
        bail!(
            "\"{WHIPTAIL_BIN}\" is not {}, \"select\" feature is not supported!",
            if WHIPTAIL_BIN.contains('/') {
                "found"
            } else {
                "in PATH"
            }
        );
    };

    let mut io = Vec::new();
    if !std::io::stdin().is_terminal() {
        io.push("stdin");
    }
    if !std::io::stdout().is_terminal() {
        io.push("stdout");
    }
    if !io.is_empty() {
        bail!(
            "{} {} not attached to interactive terminal! Cannot launch menu!",
            io.join(", "),
            if io.len() == 1 { "is" } else { "are" }
        );
    }

    let mut cmd = Command::new(whiptail);
    cmd.args([
        "--clear",
        "--backtitle",
        "Universal Wayland Session Manager",
        "--title",
        "Choose compositor",
    ]);
    if !default.is_empty() {
        cmd.args(["--default-item", &default]);
    }
    cmd.args(["--menu", "", "0", "0", "0", "--notags"])
        .args(&choices)
        .env("NEWT_MONO", "true")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped());
    let out = cmd.output()?;
    debug!("whiptail exited with {:?}", out.status);
    let chosen = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Ok(if out.status.success() {
        chosen
    } else {
        String::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_matches_textwrap() {
        assert_eq!(shorten("Hello  world", 20).unwrap(), "Hello world");
        assert_eq!(shorten("Hello world", 11).unwrap(), "Hello world");
        assert_eq!(
            shorten("Hello world, how are you", 17).unwrap(),
            "Hello [...]"
        );
        assert_eq!(shorten("Supercalifragilistic word", 10).unwrap(), "[...]");
        assert!(shorten("abc def", 0).is_err());
        assert_eq!(shorten("", 0).unwrap(), "");
    }
}
