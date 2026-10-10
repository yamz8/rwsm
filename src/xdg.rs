//! XDG base directory lookups with pyxdg semantics (spec §3.2).
//!
//! pyxdg puts the user dir first in both hierarchies (`xdg_config_dirs`
//! includes `XDG_CONFIG_HOME`), treats empty variables as unset, and the
//! `load_*_paths` helpers only yield paths that exist.

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

fn home() -> PathBuf {
    env_nonempty("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_home() -> PathBuf {
    env_nonempty("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

pub fn data_home() -> PathBuf {
    env_nonempty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
}

pub fn cache_home() -> PathBuf {
    env_nonempty("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
}

fn with_home(home: PathBuf, var: &str, default: &str) -> Vec<PathBuf> {
    let rest = env_nonempty(var).unwrap_or_else(|| default.to_string());
    std::iter::once(home)
        .chain(rest.split(':').filter(|p| !p.is_empty()).map(PathBuf::from))
        .collect()
}

/// `xdg_config_dirs`: config home, then `XDG_CONFIG_DIRS` (default `/etc/xdg`).
pub fn config_dirs() -> Vec<PathBuf> {
    with_home(config_home(), "XDG_CONFIG_DIRS", "/etc/xdg")
}

/// `xdg_data_dirs`: data home, then `XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`).
pub fn data_dirs() -> Vec<PathBuf> {
    with_home(data_home(), "XDG_DATA_DIRS", "/usr/local/share:/usr/share")
}

/// `load_config_paths(resource)`: existing `<dir>/<resource>` in priority order.
pub fn load_config_paths(resource: &str) -> Vec<PathBuf> {
    existing(config_dirs(), resource)
}

/// `load_data_paths(resource)`: existing `<dir>/<resource>` in priority order.
pub fn load_data_paths(resource: &str) -> Vec<PathBuf> {
    existing(data_dirs(), resource)
}

fn existing(dirs: Vec<PathBuf>, resource: &str) -> Vec<PathBuf> {
    dirs.into_iter()
        .map(|d| d.join(resource))
        .filter(|p| p.exists())
        .collect()
}

/// `get_runtime_dir(strict=True)`: `XDG_RUNTIME_DIR` must be set.
pub fn runtime_dir() -> Result<PathBuf> {
    match env_nonempty("XDG_RUNTIME_DIR") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => bail!("Fatal: empty or undefined XDG_RUNTIME_DIR!"),
    }
}

/// `$XDG_RUNTIME_DIR/uwsm`, where uwsm keeps its runtime files.
pub fn uwsm_runtime_dir() -> Result<PathBuf> {
    Ok(runtime_dir()?.join(crate::config::BIN_NAME))
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Resolves a command like `which`: names with `/` are checked directly,
/// others are searched in `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable_file(&p).then_some(p);
    }
    let path = env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".into());
    path.split(':')
        .map(|dir| {
            if dir.is_empty() {
                PathBuf::from(name)
            } else {
                Path::new(dir).join(name)
            }
        })
        .find(|p| is_executable_file(p))
}

#[cfg(test)]
pub(crate) mod test_env {
    //! Tests that touch process-wide env vars must hold this lock.
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    pub fn lock() -> MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy_order_and_defaults() {
        let _g = test_env::lock();
        unsafe {
            env::set_var("HOME", "/home/u");
            env::remove_var("XDG_CONFIG_HOME");
            env::set_var("XDG_CONFIG_DIRS", "");
            env::set_var("XDG_DATA_HOME", "/d");
            env::set_var("XDG_DATA_DIRS", "/a::/b");
        }
        assert_eq!(
            config_dirs(),
            vec![PathBuf::from("/home/u/.config"), PathBuf::from("/etc/xdg")]
        );
        assert_eq!(
            data_dirs(),
            vec![
                PathBuf::from("/d"),
                PathBuf::from("/a"),
                PathBuf::from("/b")
            ]
        );
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh").is_some());
        assert!(which("/bin/sh").is_some());
        assert!(which("definitely-not-a-command-xyz").is_none());
    }
}
