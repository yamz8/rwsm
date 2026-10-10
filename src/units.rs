//! Generated unit drop-ins and their cleanup (spec §4.3–4.5).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cli::UnitRung;
use crate::comp::Comp;
use crate::config::{BIN_NAME, BIN_PATH};
use crate::dbus::Bus;
use crate::escape::shlex_join;
use crate::{debug, output, xdg};

/// Writes and removes unit files in one rung, tracking whether anything
/// changed (uwsm's `UnitsState.changed`).
pub struct UnitFiles {
    pub dry_run: bool,
    pub changed: bool,
}

/// `<rung dir>/systemd/user` (port of `get_unit_path`).
pub fn unit_dir(rung: UnitRung) -> Result<PathBuf> {
    let base = match rung {
        UnitRung::Run => xdg::runtime_dir()?,
        UnitRung::Home => xdg::config_home(),
    };
    Ok(base.join("systemd/user"))
}

fn is_unit_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_:.\\-".contains(c)
}

/// Port of the `Val.unit_ext` check: a unit name, or `<unit>.d/<name>.conf`.
pub fn is_supported_unit(unit: &str) -> bool {
    if let Some((dir, file)) = unit.rsplit_once('/') {
        let Some(dir_unit) = dir.strip_suffix(".d") else {
            return false;
        };
        let dir_unit = dir_unit.rsplit('/').next().unwrap_or(dir_unit);
        let dir_unit = dir_unit.strip_suffix('@').unwrap_or(dir_unit);
        let Some(stem) = file.strip_suffix("conf") else {
            return false;
        };
        let stem: Vec<char> = stem.chars().collect();
        return dir_unit.chars().last().is_some_and(is_unit_char)
            && stem.len() >= 2
            && stem[..stem.len() - 1].iter().all(|c| is_unit_char(*c));
    }
    ["service", "slice", "scope", "target", "socket"]
        .iter()
        .any(|ext| {
            unit.strip_suffix(ext)
                .and_then(|u| u.strip_suffix('.'))
                .map(|u| u.strip_suffix('@').unwrap_or(u))
                .is_some_and(|u| u.chars().last().is_some_and(is_unit_char))
        })
}

fn header(mark: &str) -> String {
    format!("# injected by {BIN_NAME}, do not edit\n[Unit]\nX-UWSMMark={mark}\n")
}

impl UnitFiles {
    pub fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            changed: false,
        }
    }

    fn resolve(&self, unit: &str, rung: UnitRung) -> Result<(PathBuf, String)> {
        if !is_supported_unit(unit) {
            bail!(
                "Trying to update unit with unsupported extension {}: {unit}",
                unit.rsplit('.').next().unwrap_or_default()
            );
        }
        if unit.matches('/').count() > 1 {
            bail!(
                "Only single subdir supported for relative unit, got {} ({unit})",
                unit.matches('/').count()
            );
        }
        Ok((unit_dir(rung)?, unit.to_string()))
    }

    /// Port of `update_unit`: writes `data` if it differs from the file.
    pub fn update(&mut self, unit: &str, data: &str, rung: UnitRung) -> Result<bool> {
        let (dir, unit) = self.resolve(unit, rung)?;
        let rung_name = rung.as_str();
        if !dir.is_dir() {
            if !self.dry_run {
                fs::create_dir_all(&dir)?;
                output::ok(&format!("Created dir \"{}/\"", dir.display()));
            } else if !self.changed {
                output::ok(&format!("Will create dir \"{}/\"", dir.display()));
            }
        }
        if let Some((sub, _)) = unit.split_once('/') {
            let sub_path = dir.join(sub);
            if !sub_path.is_dir() {
                if !self.dry_run {
                    fs::create_dir(&sub_path)?;
                    output::ok(&format!("Created unit subdir \"{sub}/\" ({rung_name})."));
                } else {
                    output::ok(&format!(
                        "Will create unit subdir \"{sub}/\" ({rung_name})."
                    ));
                }
            }
        }
        let path = dir.join(&unit);
        let old = fs::read_to_string(&path).unwrap_or_default();
        if old == data {
            return Ok(false);
        }
        debug!("for update: {}\n{data}", path.display());
        let verb = match (self.dry_run, old.is_empty()) {
            (false, false) => "Updated",
            (false, true) => "Created",
            (true, false) => "Will update",
            (true, true) => "Will create",
        };
        if !self.dry_run {
            fs::write(&path, data)
                .with_context(|| format!("Could not write {}", path.display()))?;
        }
        output::ok(&format!("{verb} \"{unit}\" ({rung_name})."));
        self.changed = true;
        Ok(true)
    }

    /// Port of `remove_unit`: removes the file and its subdir if now empty.
    pub fn remove(&mut self, unit: &str, rung: UnitRung) -> Result<bool> {
        let (dir, unit) = self.resolve(unit, rung)?;
        let rung_name = rung.as_str();
        let path = dir.join(&unit);
        let mut change = false;
        if path.is_file() {
            debug!("for removing: {}", path.display());
            if !self.dry_run {
                fs::remove_file(&path)?;
                output::ok(&format!("Removed \"{unit}\" ({rung_name})."));
            } else {
                output::ok(&format!("Will remove \"{unit}\" ({rung_name})."));
            }
            self.changed = true;
            change = true;
        }
        if let Some((sub, file)) = unit.split_once('/') {
            let sub_path = dir.join(sub);
            if sub_path.is_dir() {
                let others = fs::read_dir(&sub_path)?
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy() != file);
                if others {
                    output::warning(&format!(
                        "Unit subdir \"{sub}\" is not empty ({rung_name})."
                    ));
                } else if !self.dry_run {
                    fs::remove_dir(&sub_path)?;
                    output::ok(&format!("Removed unit subdir \"{sub}\" ({rung_name})."));
                } else {
                    output::ok(&format!("Will remove unit subdir \"{sub}\" ({rung_name})."));
                }
            }
        }
        Ok(change)
    }

    /// Port of `remove_units`: removes files carrying an `X-UWSMMark=` line,
    /// or only those whose mark is in `only`.
    pub fn remove_marked(&mut self, only: &[String], rung: UnitRung) -> Result<()> {
        let dir = unit_dir(rung)?;
        let marks: Vec<String> = only
            .iter()
            .map(|m| m.trim())
            .filter(|m| !m.is_empty())
            .map(|m| format!("X-UWSMMark={m}"))
            .collect();
        let mut files = Vec::new();
        collect_files(&dir, &mut files);
        files.sort();
        let mut matched = Vec::new();
        for path in files {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let hit = text.lines().any(|l| {
                let l = l.trim();
                if only.is_empty() {
                    l.starts_with("X-UWSMMark=")
                } else {
                    marks.iter().any(|m| m == l)
                }
            });
            if hit && let Ok(rel) = path.strip_prefix(&dir) {
                matched.push(rel.to_string_lossy().into_owned());
            }
        }
        for unit in matched {
            self.remove(&unit, rung)?;
        }
        Ok(())
    }

    /// Port of `generate_dropins` (spec §4.3).
    pub fn generate_dropins(
        &mut self,
        comp: &Comp,
        waitenv_timeout: u64,
        rung: UnitRung,
    ) -> Result<()> {
        let timeout_units = [
            "wayland-wm@.service.d/50_timeout.conf",
            "wayland-session-waitenv.service.d/50_timeout.conf",
        ];
        if waitenv_timeout != 30 {
            let data = format!(
                "{}[Service]\nTimeoutStartSec={waitenv_timeout}\n",
                header("generic")
            );
            for unit in timeout_units {
                self.update(unit, &data, rung)?;
            }
        } else {
            for unit in timeout_units {
                self.remove(unit, rung)?;
            }
        }

        let preloader = format!(
            "wayland-wm-env@{}.service.d/50_custom.conf",
            comp.id_unit_string
        );
        let service = format!(
            "wayland-wm@{}.service.d/50_custom.conf",
            comp.id_unit_string
        );
        let mut preloader_data = vec![header(&comp.id)];
        let mut service_data = vec![header(&comp.id)];

        if !comp.name.is_empty() || !comp.description.is_empty() {
            let name = if comp.name.is_empty() {
                &comp.bin_name
            } else {
                &comp.name
            };
            let desc: Vec<&str> = [name.as_str(), comp.description.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();
            let desc = desc.join(", ");
            preloader_data.push(format!("Description=Environment preloader for {desc}\n"));
            service_data.push(format!("Description=Main service for {desc}\n"));
        }

        let abs_first = comp.cmdline[0].starts_with('/');
        let mut preloader_exec: Vec<String> = Vec::new();
        if comp.cli_desktop_names_exclusive {
            preloader_exec.extend(["-eD".into(), comp.cli_desktop_names.join(":")]);
        } else if comp.desktop_names != [comp.bin_name.clone()] {
            preloader_exec.extend(["-D".into(), comp.desktop_names.join(":")]);
        }
        if !preloader_exec.is_empty() || abs_first {
            preloader_exec.extend(["--".into(), "%I".into()]);
            if abs_first {
                preloader_exec.push(comp.cmdline[0].clone());
            }
            let argv: Vec<String> = [BIN_PATH, "aux", "prepare-env"]
                .into_iter()
                .map(String::from)
                .chain(preloader_exec)
                .collect();
            preloader_data.push(format!(
                "[Service]\nExecStart=\nExecStart={}\n",
                shlex_join(&argv)
            ));
        }

        let service_exec: Vec<String> = if abs_first {
            comp.cmdline.clone()
        } else if !comp.cli_args.is_empty() {
            std::iter::once(String::new())
                .chain(comp.cli_args.iter().cloned())
                .collect()
        } else {
            Vec::new()
        };
        if !service_exec.is_empty() {
            let argv: Vec<String> = [BIN_PATH, "aux", "exec", "--", "%I"]
                .into_iter()
                .map(String::from)
                .chain(service_exec)
                .collect();
            service_data.push(format!(
                "[Service]\nExecStart=\nExecStart={}\n",
                shlex_join(&argv)
            ));
        }

        for (unit, data) in [(preloader, preloader_data), (service, service_data)] {
            if data.len() > 1 {
                self.update(&unit, &data.concat(), rung)?;
            } else {
                self.remove(&unit, rung)?;
            }
        }
        Ok(())
    }

    /// Port of `generate_tweaks` (spec §4.4).
    pub fn generate_tweaks(&mut self, rung: UnitRung) -> Result<()> {
        let h = header("tweaks");
        self.update(
            "app-@autostart.service.d/slice-tweak.conf",
            &format!(
                "{h}# make autostart apps stoppable/restartable by target\n\
                 PartOf=xdg-desktop-autostart.target\n\
                 After=xdg-desktop-autostart.target\n\
                 [Service]\n\
                 # also put them in special graphical app slice\n\
                 Slice=app-graphical.slice\n\
                 EnvironmentFile=-%t/{BIN_NAME}/env_session.conf\n"
            ),
            rung,
        )?;
        self.update(
            "app-flatpak-.scope.d/order-tweak.conf",
            &format!(
                "{h}# terminate with session properly\n\
                 PartOf=graphical-session.target\n\
                 After=graphical-session.target\n\
                 [Scope]\n\
                 # also put them in special graphical app slice\n\
                 Slice=app-graphical.slice\n"
            ),
            rung,
        )?;
        self.update(
            "plasma-xdg-desktop-portal-kde.service.d/order-tweak.conf",
            &format!("{h}After=graphical-session.target\n"),
            rung,
        )?;
        Ok(())
    }

    /// Port of `reload_systemd`.
    pub fn reload_systemd(&mut self) -> Result<()> {
        if self.dry_run {
            output::normal("Will reload systemd user manager.");
        } else {
            output::normal("Reloading systemd user manager.");
            let bus = Bus::session()?;
            let job = bus.reload()?;
            bus.wait_for_job(&job)?;
            debug!("reload systemd job {job} finished");
        }
        self.changed = false;
        Ok(())
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names() {
        assert!(is_supported_unit("wayland-wm@.service.d/50_timeout.conf"));
        assert!(is_supported_unit(
            "wayland-wm@sway.desktop.service.d/50_custom.conf"
        ));
        assert!(is_supported_unit(
            "wayland-wm-env@niri\\x2dsession.service.d/50_custom.conf"
        ));
        assert!(is_supported_unit("app-flatpak-.scope.d/order-tweak.conf"));
        assert!(is_supported_unit("foo.service"));
        assert!(!is_supported_unit("foo.txt"));
        assert!(!is_supported_unit("foo.d/bar.txt"));
    }

    fn comp(cmdline: &[&str], cli_args: &[&str], names: &[&str]) -> Comp {
        Comp {
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            cli_args: cli_args.iter().map(|s| s.to_string()).collect(),
            id: "sway.desktop".into(),
            id_unit_string: "sway.desktop".into(),
            bin_name: "sway".into(),
            bin_id: "sway".into(),
            desktop_names: names.iter().map(|s| s.to_string()).collect(),
            name: "Sway".into(),
            description: "An i3-compatible Wayland compositor".into(),
            ..Default::default()
        }
    }

    #[test]
    fn dropins_match_uwsm_bytes() {
        let _g = crate::xdg::test_env::lock();
        let dir = std::env::temp_dir().join(format!("rwsm-units-{}", crate::env::random_hex(8)));
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
        let mut uf = UnitFiles::new(false);
        let c = comp(
            &["sway", "--unsupported-gpu"],
            &["--unsupported-gpu"],
            &["sway", "wlroots"],
        );
        uf.generate_dropins(&c, 30, UnitRung::Run).unwrap();
        let base = dir.join("systemd/user");
        assert_eq!(
            fs::read_to_string(base.join("wayland-wm-env@sway.desktop.service.d/50_custom.conf"))
                .unwrap(),
            format!(
                "# injected by uwsm, do not edit\n[Unit]\nX-UWSMMark=sway.desktop\n\
                 Description=Environment preloader for Sway, An i3-compatible Wayland compositor\n\
                 [Service]\nExecStart=\nExecStart={BIN_PATH} aux prepare-env -D sway:wlroots -- %I\n"
            )
        );
        assert_eq!(
            fs::read_to_string(base.join("wayland-wm@sway.desktop.service.d/50_custom.conf"))
                .unwrap(),
            format!(
                "# injected by uwsm, do not edit\n[Unit]\nX-UWSMMark=sway.desktop\n\
                 Description=Main service for Sway, An i3-compatible Wayland compositor\n\
                 [Service]\nExecStart=\nExecStart={BIN_PATH} aux exec -- %I '' --unsupported-gpu\n"
            )
        );
        assert!(uf.changed);

        // Unchanged content is not rewritten.
        let mut uf = UnitFiles::new(false);
        uf.generate_dropins(&c, 30, UnitRung::Run).unwrap();
        assert!(!uf.changed);

        // Timeout drop-ins appear and are removed by mark.
        uf.generate_dropins(&c, 45, UnitRung::Run).unwrap();
        assert!(base.join("wayland-wm@.service.d/50_timeout.conf").is_file());
        uf.generate_tweaks(UnitRung::Run).unwrap();
        uf.remove_marked(&["generic".into(), "tweaks".into()], UnitRung::Run)
            .unwrap();
        assert!(!base.join("wayland-wm@.service.d").exists());
        assert!(!base.join("app-flatpak-.scope.d").exists());
        assert!(
            base.join("wayland-wm@sway.desktop.service.d/50_custom.conf")
                .is_file()
        );

        uf.remove_marked(&[], UnitRung::Run).unwrap();
        assert_eq!(fs::read_dir(&base).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plain_executable_needs_no_dropins() {
        let _g = crate::xdg::test_env::lock();
        let dir = std::env::temp_dir().join(format!("rwsm-units-{}", crate::env::random_hex(8)));
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
        let mut c = comp(&["sway"], &[], &["sway"]);
        c.name.clear();
        c.description.clear();
        let mut uf = UnitFiles::new(false);
        uf.generate_dropins(&c, 30, UnitRung::Run).unwrap();
        assert!(!uf.changed);
        let _ = fs::remove_dir_all(dir);
    }
}
