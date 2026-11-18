//! The service inside the Flatpak. It runs in a sandbox of its own that the Flatpak portal
//! spawns, so it outlives the window; at login the Background portal's `RequestBackground`
//! with `autostart` has the desktop start `canalis --daemon`. The portal cannot be asked
//! what it was told, so a marker file remembers the answer.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::i18n::t;
use log::{info, warn};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{Fd, OwnedObjectPath, OwnedValue, Value};

/// `canalis --daemon` in a new sandbox through `org.freedesktop.portal.Flatpak.Spawn`,
/// its output into `log`. Unlike `flatpak-spawn`, nothing waits for it in this sandbox.
pub fn spawn_service(log: Option<&std::fs::File>) -> Result<u32, String> {
    let text = |e: zbus::Error| e.to_string();
    let bus = Connection::session().map_err(text)?;
    let portal = Proxy::new(
        &bus,
        "org.freedesktop.portal.Flatpak",
        "/org/freedesktop/portal/Flatpak",
        "org.freedesktop.portal.Flatpak",
    )
    .map_err(text)?;
    let nul = |s: &str| [s.as_bytes(), b"\0"].concat();
    let argv: Vec<Vec<u8>> = ["/app/bin/canalis", "--daemon"].map(nul).into();
    let fds: HashMap<u32, Fd<'_>> = log
        .into_iter()
        .flat_map(|f| [(1, Fd::from(f)), (2, Fd::from(f))])
        .collect();
    let envs: HashMap<&str, &str> = HashMap::new();
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    portal
        .call("Spawn", &(nul("/"), argv, fds, envs, 0u32, options))
        .map_err(text)
}

/// What the last answer was; inside the sandbox `XDG_CONFIG_HOME` is the app's own.
fn marker() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("canalis").join("run-at-login"))
}

pub fn enabled() -> bool {
    marker().is_some_and(|p| p.is_file())
}

/// Asks for autostart on or off on a thread (the portal may show a dialog first), then
/// records what the desktop allowed.
pub fn set_autostart(on: bool) {
    let spawned = std::thread::Builder::new()
        .name("background-portal".into())
        .spawn(move || match request(on) {
            Ok(allowed) => {
                info!("Background portal: autostart {allowed}");
                record(allowed);
            }
            Err(e) => warn!("Background portal: {e}"),
        });
    if let Err(e) = spawned {
        warn!("cannot ask the Background portal: {e}");
    }
}

fn record(on: bool) {
    let Some(path) = marker() else { return };
    let result = if on {
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, ""))
    } else {
        std::fs::remove_file(&path).or_else(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(e),
        })
    };
    if let Err(e) = result {
        warn!("cannot record run at login in {}: {e}", path.display());
    }
}

/// One portal request: subscribe to the Request object first (its path follows from our
/// handle token), call, wait for its `Response`; `Ok(autostart)` as granted.
fn request(autostart: bool) -> Result<bool, String> {
    let text = |e: zbus::Error| e.to_string();
    let bus = Connection::session().map_err(text)?;
    let sender = bus
        .unique_name()
        .ok_or("no bus name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let token = format!("canalis_bg{}", std::process::id());
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(
        &bus,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(text)?;
    let mut responses = request.receive_signal("Response").map_err(text)?;
    let options: HashMap<&str, Value<'_>> = [
        ("handle_token", Value::from(token)),
        (
            "reason",
            Value::from(t(
                "start the chains when you log in, before any window opens",
            )),
        ),
        ("autostart", Value::from(autostart)),
        (
            "commandline",
            Value::from(vec!["canalis".to_owned(), "--daemon".to_owned()]),
        ),
        ("dbus-activatable", Value::from(false)),
    ]
    .into_iter()
    .collect();
    let background = Proxy::new(
        &bus,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Background",
    )
    .map_err(text)?;
    let _: OwnedObjectPath = background
        .call("RequestBackground", &("", options))
        .map_err(text)?;
    let message = responses.next().ok_or("the portal went away")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) =
        message.body().deserialize().map_err(text)?;
    if code != 0 {
        return Err(format!("the request was refused (code {code})"));
    }
    Ok(results
        .get("autostart")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false))
}
