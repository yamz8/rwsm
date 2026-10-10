//! D-Bus calls to systemd, logind, the bus daemon and the notification
//! service (spec §8), ported from uwsm's `dbus.py`.
//!
//! Connections are opened lazily and cached per process. zbus starts a
//! background thread, so code that forks must do so before the first call.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";
const LOGIN: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";
const LOGIN_MANAGER: &str = "org.freedesktop.login1.Manager";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Session,
    System,
}

pub struct Bus {
    conn: Connection,
}

/// One row of `ListUnitsByPatterns`.
#[derive(Debug, Clone)]
pub struct UnitInfo {
    pub name: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
}

/// One row of `ListJobs`.
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: u32,
    pub unit: String,
    pub job_type: String,
    pub state: String,
    pub path: OwnedObjectPath,
}

/// One row of logind `ListSessionsEx`.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub user_name: String,
    pub seat: String,
    pub class: String,
}

type UnitRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);
type JobRow = (
    u32,
    String,
    String,
    String,
    OwnedObjectPath,
    OwnedObjectPath,
);
type SessionRow = (
    String,
    u32,
    String,
    String,
    u32,
    String,
    String,
    bool,
    u64,
    OwnedObjectPath,
);

impl Bus {
    /// Cached connection for the given bus level.
    pub fn get(level: Level) -> Result<&'static Bus> {
        static SESSION: OnceLock<Bus> = OnceLock::new();
        static SYSTEM: OnceLock<Bus> = OnceLock::new();
        let cell = match level {
            Level::Session => &SESSION,
            Level::System => &SYSTEM,
        };
        if let Some(bus) = cell.get() {
            return Ok(bus);
        }
        let conn = match level {
            Level::Session => Connection::session().context("Could not connect to session bus")?,
            Level::System => Connection::system().context("Could not connect to system bus")?,
        };
        Ok(cell.get_or_init(|| Bus { conn }))
    }

    pub fn session() -> Result<&'static Bus> {
        Self::get(Level::Session)
    }

    pub fn system() -> Result<&'static Bus> {
        Self::get(Level::System)
    }

    fn proxy<'a>(&'a self, dest: &'a str, path: &'a str, iface: &'a str) -> Result<Proxy<'a>> {
        Ok(Proxy::new(&self.conn, dest, path, iface)?)
    }

    fn manager(&self) -> Result<Proxy<'_>> {
        self.proxy(SYSTEMD, SYSTEMD_PATH, SYSTEMD_MANAGER)
    }

    fn get_property(&self, dest: &str, path: &str, iface: &str, prop: &str) -> Result<OwnedValue> {
        let props = self.proxy(dest, path, "org.freedesktop.DBus.Properties")?;
        let value: OwnedValue = props
            .call("Get", &(iface, prop))
            .with_context(|| format!("Could not get property {iface}.{prop} of {path}"))?;
        Ok(value)
    }

    /// `Manager.Reload()`, returns the job path.
    pub fn reload(&self) -> Result<OwnedObjectPath> {
        Ok(self.manager()?.call("Reload", &())?)
    }

    pub fn list_jobs(&self) -> Result<Vec<JobInfo>> {
        let rows: Vec<JobRow> = self.manager()?.call("ListJobs", &())?;
        Ok(rows
            .into_iter()
            .map(|(id, unit, job_type, state, path, _)| JobInfo {
                id,
                unit,
                job_type,
                state,
                path,
            })
            .collect())
    }

    /// Polls `ListJobs` every 100 ms until `job` is gone.
    pub fn wait_for_job(&self, job: &OwnedObjectPath) -> Result<()> {
        loop {
            if !self.list_jobs()?.iter().any(|j| &j.path == job) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    pub fn list_units_by_patterns(
        &self,
        states: &[&str],
        patterns: &[&str],
    ) -> Result<Vec<UnitInfo>> {
        let rows: Vec<UnitRow> = self
            .manager()?
            .call("ListUnitsByPatterns", &(states, patterns))?;
        Ok(rows
            .into_iter()
            .map(|r| UnitInfo {
                name: r.0,
                description: r.1,
                load_state: r.2,
                active_state: r.3,
                sub_state: r.4,
            })
            .collect())
    }

    /// Unit property, looked up on the generic Unit interface first unless
    /// `skip_generic`, then on the unit type's interface (e.g. `.Service`).
    pub fn get_unit_property(
        &self,
        unit: &str,
        prop: &str,
        skip_generic: bool,
    ) -> Result<OwnedValue> {
        let path: OwnedObjectPath = self
            .manager()?
            .call("GetUnit", &(unit,))
            .with_context(|| format!("Could not get unit {unit}"))?;
        if !skip_generic
            && let Ok(v) = self.get_property(
                SYSTEMD,
                path.as_str(),
                "org.freedesktop.systemd1.Unit",
                prop,
            )
        {
            return Ok(v);
        }
        let suffix = unit.rsplit('.').next().unwrap_or_default();
        let mut type_iface = String::from("org.freedesktop.systemd1.");
        let mut chars = suffix.chars();
        if let Some(first) = chars.next() {
            type_iface.extend(first.to_uppercase());
            type_iface.push_str(&chars.as_str().to_lowercase());
        }
        self.get_property(SYSTEMD, path.as_str(), &type_iface, prop)
    }

    pub fn get_unit_property_string(
        &self,
        unit: &str,
        prop: &str,
        skip_generic: bool,
    ) -> Result<String> {
        Ok(String::try_from(self.get_unit_property(
            unit,
            prop,
            skip_generic,
        )?)?)
    }

    pub fn get_unit_property_u64(&self, unit: &str, prop: &str, skip_generic: bool) -> Result<u64> {
        Ok(u64::try_from(self.get_unit_property(
            unit,
            prop,
            skip_generic,
        )?)?)
    }

    pub fn get_manager_property_string(&self, prop: &str) -> Result<String> {
        Ok(String::try_from(self.get_property(
            SYSTEMD,
            SYSTEMD_PATH,
            SYSTEMD_MANAGER,
            prop,
        )?)?)
    }

    /// The systemd activation environment as a map.
    pub fn get_systemd_vars(&self) -> Result<BTreeMap<String, String>> {
        let value = self.get_property(SYSTEMD, SYSTEMD_PATH, SYSTEMD_MANAGER, "Environment")?;
        let assignments: Vec<String> = value.try_into()?;
        Ok(assignments
            .into_iter()
            .filter_map(|a| {
                a.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect())
    }

    pub fn set_systemd_vars(&self, vars: &BTreeMap<String, String>) -> Result<()> {
        let assignments: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _: () = self.manager()?.call("SetEnvironment", &(assignments,))?;
        Ok(())
    }

    pub fn unset_systemd_vars(&self, names: &[String]) -> Result<()> {
        let _: () = self.manager()?.call("UnsetEnvironment", &(names,))?;
        Ok(())
    }

    /// `org.freedesktop.DBus.UpdateActivationEnvironment`.
    pub fn set_dbus_vars(&self, vars: &BTreeMap<String, String>) -> Result<()> {
        let proxy = self.proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )?;
        let map: std::collections::HashMap<&str, &str> =
            vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let _: () = proxy.call("UpdateActivationEnvironment", &(map,))?;
        Ok(())
    }

    pub fn stop_unit(&self, unit: &str, mode: &str) -> Result<OwnedObjectPath> {
        Ok(self.manager()?.call("StopUnit", &(unit, mode))?)
    }

    pub fn list_login_sessions_ex(&self) -> Result<Vec<SessionInfo>> {
        let proxy = self.proxy(LOGIN, LOGIN_PATH, LOGIN_MANAGER)?;
        let rows: Vec<SessionRow> = proxy.call("ListSessionsEx", &())?;
        Ok(rows
            .into_iter()
            .map(|r| SessionInfo {
                id: r.0,
                user_name: r.2,
                seat: r.3,
                class: r.5,
            })
            .collect())
    }

    pub fn get_session_property(&self, session_id: &str, prop: &str) -> Result<OwnedValue> {
        let proxy = self.proxy(LOGIN, LOGIN_PATH, LOGIN_MANAGER)?;
        let path: OwnedObjectPath = proxy
            .call("GetSession", &(session_id,))
            .with_context(|| format!("Could not get login session {session_id}"))?;
        self.get_property(LOGIN, path.as_str(), "org.freedesktop.login1.Session", prop)
    }

    pub fn get_session_property_u32(&self, session_id: &str, prop: &str) -> Result<u32> {
        Ok(u32::try_from(self.get_session_property(session_id, prop)?)?)
    }

    pub fn get_session_property_bool(&self, session_id: &str, prop: &str) -> Result<bool> {
        Ok(bool::try_from(
            self.get_session_property(session_id, prop)?,
        )?)
    }

    /// Whether the D-Bus daemon is dbus-broker, which shares systemd's
    /// activation environment instead of keeping its own.
    pub fn is_dbus_broker(&self) -> Result<bool> {
        Ok(self.get_unit_property_string("dbus.service", "Id", false)? == "dbus-broker.service")
    }
}

/// Sends a desktop notification on the session bus.
pub fn notify(summary: &str, body: &str, icon: &str, urgency: u8) -> Result<()> {
    let bus = Bus::session()?;
    let proxy = bus.proxy(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;
    let mut hints: std::collections::HashMap<&str, zbus::zvariant::Value> = Default::default();
    hints.insert("urgency", zbus::zvariant::Value::U8(urgency.min(2)));
    let actions: Vec<&str> = Vec::new();
    let _: u32 = proxy.call(
        "Notify",
        &("UWSM", 0u32, icon, summary, body, actions, hints, -1i32),
    )?;
    Ok(())
}
