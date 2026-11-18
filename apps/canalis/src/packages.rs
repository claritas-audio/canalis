//! Plugin packages through PackageKit, the system service behind GNOME Software and
//! Discover: it speaks dnf, apt, zypper and pacman, and asks for the password through the
//! desktop's polkit agent. A worker thread does the D-Bus calls; the views read a shared
//! snapshot and queue jobs. Without PackageKit (or in a Flatpak, where host packages never
//! reach the sandbox) the picker falls back to naming the command.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedObjectPath;

use crate::i18n::{t, tf};

/// A plugin family and its package names on the distributions we know; PackageKit says
/// which of them this system's repositories carry.
pub struct Family {
    pub name: &'static str,
    pub about: &'static str,
    packages: &'static [&'static str],
    /// lilv and suil: offered only while LV2 is off.
    pub lv2_support: bool,
}

pub const FAMILIES: &[Family] = &[
    Family {
        name: "LSP Plugins",
        about: "EQs, compressors, gates, reverbs, analyzers; CLAP and LV2",
        packages: &["lsp-plugins-clap", "lsp-plugins-lv2"],
        lv2_support: false,
    },
    Family {
        name: "Calf Studio Gear",
        about: "vintage-style compressors, EQs and effects; LV2",
        packages: &["calf", "calf-plugins"],
        lv2_support: false,
    },
    Family {
        name: "ZAM Plugins",
        about: "mastering compressors, EQs and limiters; LV2",
        packages: &["zam-plugins", "zam-plugins-lv2", "zam-plugins-clap"],
        lv2_support: false,
    },
    Family {
        name: "x42 Plugins",
        about: "meters, EQ, delay, auto-tune; LV2",
        packages: &["x42-plugins"],
        lv2_support: false,
    },
    Family {
        name: "Dragonfly Reverb",
        about: "hall, room, plate and early-reflection reverbs",
        packages: &[
            "dragonfly-reverb-clap",
            "dragonfly-reverb-lv2",
            "dragonfly-reverb",
        ],
        lv2_support: false,
    },
    Family {
        name: "LV2 support",
        about: "lilv and suil, so Canalis can load LV2 plugins and their windows",
        packages: &["lilv-libs", "suil", "liblilv-0-0", "libsuil-0-0", "lilv"],
        lv2_support: true,
    },
];

impl Family {
    pub fn label(&self) -> &'static str {
        match self.name {
            "LV2 support" => t("LV2 support"),
            other => other,
        }
    }

    pub fn blurb(&self) -> &'static str {
        match self.name {
            "LSP Plugins" => t("EQs, compressors, gates, reverbs, analyzers; CLAP and LV2"),
            "Calf Studio Gear" => t("vintage-style compressors, EQs and effects; LV2"),
            "ZAM Plugins" => t("mastering compressors, EQs and limiters; LV2"),
            "x42 Plugins" => t("meters, EQ, delay, auto-tune; LV2"),
            "Dragonfly Reverb" => t("hall, room, plate and early-reflection reverbs"),
            "LV2 support" => t("lilv and suil, so Canalis can load LV2 plugins and their windows"),
            _ => self.about,
        }
    }
}

/// What this system's repositories have of a family.
#[derive(Clone, Debug, Default)]
pub struct FamilyState {
    pub installed: Vec<String>,
    pub available: Vec<String>,
}

impl FamilyState {
    pub fn carried(&self) -> bool {
        !self.installed.is_empty() || !self.available.is_empty()
    }
}

/// A package that owns a plugin file, by PackageKit id (`name;version;arch;data`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub id: String,
}

impl Owner {
    pub fn name(&self) -> &str {
        self.id.split(';').next().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, Default)]
pub enum Backend {
    /// Not asked yet, or the first resolve is running.
    #[default]
    Checking,
    Ready,
    /// Why there are no installs: the command line is the way.
    Off(String),
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub backend: Backend,
    /// By family name.
    pub families: BTreeMap<&'static str, FamilyState>,
    /// Plugin path (a `.clap` file or an `.lv2` bundle) → the package that installed it.
    pub owners: BTreeMap<String, Owner>,
    /// The install or removal running now, in words.
    pub busy: Option<String>,
    /// How the last install or removal went, when it failed.
    pub error: Option<String>,
    /// Bumped after every install or removal that changed the system.
    pub changes: u64,
}

enum Job {
    Resolve,
    Owners(Vec<String>),
    Install(&'static str),
    Remove { label: String, ids: Vec<String> },
}

pub struct Packages {
    shared: Arc<Mutex<Snapshot>>,
    jobs: Mutex<mpsc::Sender<Job>>,
}

/// The process's one PackageKit worker; `wake` repaints the window when a job ends.
pub fn start(wake: impl Fn() + Send + 'static) -> &'static Packages {
    start_once().get_or_init(|| {
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let (tx, rx) = mpsc::channel();
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("packagekit".into())
            .spawn(move || run(&worker, &rx, &wake));
        if let Err(e) = spawned {
            lock(&shared).backend = Backend::Off(tf("no worker thread: {error}", &[("error", &e)]));
        }
        let _ = tx.send(Job::Resolve);
        Packages {
            shared,
            jobs: Mutex::new(tx),
        }
    })
}

/// The worker, once `start` ran; views that draw before it read nothing.
pub fn get() -> Option<&'static Packages> {
    start_once().get()
}

fn start_once() -> &'static OnceLock<Packages> {
    static PACKAGES: OnceLock<Packages> = OnceLock::new();
    &PACKAGES
}

fn lock(shared: &Mutex<Snapshot>) -> std::sync::MutexGuard<'_, Snapshot> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

impl Packages {
    pub fn snapshot(&self) -> Snapshot {
        lock(&self.shared).clone()
    }

    fn send(&self, job: Job) {
        let _ = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(job);
    }

    pub fn install(&self, family: &'static str) {
        self.send(Job::Install(family));
    }

    pub fn remove(&self, label: String, ids: Vec<String>) {
        self.send(Job::Remove { label, ids });
    }

    /// Looks up the packages that own these plugin paths (system ones only matter).
    pub fn find_owners(&self, paths: Vec<String>) {
        self.send(Job::Owners(paths));
    }
}

fn run(shared: &Mutex<Snapshot>, jobs: &mpsc::Receiver<Job>, wake: &dyn Fn()) {
    let set = |f: &dyn Fn(&mut Snapshot)| {
        f(&mut lock(shared));
        wake();
    };
    if in_flatpak() {
        set(&|s| {
            s.backend = Backend::Off(
                t(
                    "inside the Flatpak, plugins come as Flathub extensions: run the command, \
                     then quit Canalis from the tray and open it again",
                )
                .into(),
            );
        });
        return;
    }
    let kit = match Kit::connect() {
        Ok(kit) => kit,
        Err(e) => {
            log::info!("PackageKit not available ({e}); the picker shows commands");
            set(&|s| {
                s.backend =
                    Backend::Off(tf("PackageKit is not available: {error}", &[("error", &e)]))
            });
            return;
        }
    };
    let mut asked_owners: Vec<String> = Vec::new();
    for job in jobs {
        match job {
            Job::Resolve => match kit.resolve_families() {
                Ok(families) => set(&|s| {
                    s.families = families.clone();
                    s.backend = Backend::Ready;
                }),
                Err(e) => {
                    log::warn!("PackageKit resolve: {e}");
                    set(&|s| s.backend = Backend::Off(tf("PackageKit: {error}", &[("error", &e)])));
                }
            },
            Job::Owners(paths) => {
                let mut found = BTreeMap::new();
                for path in &paths {
                    if !path.starts_with("/usr/") && !path.starts_with("/opt/") {
                        continue;
                    }
                    match kit.owner_of(path) {
                        Ok(Some(owner)) => {
                            found.insert(path.clone(), owner);
                        }
                        Ok(None) => {}
                        Err(e) => log::warn!("PackageKit owner of {path}: {e}"),
                    }
                }
                asked_owners = paths;
                set(&|s| s.owners = found.clone());
            }
            Job::Install(name) => {
                let Some(family) = lock(shared).families.get(name).cloned() else {
                    continue;
                };
                let ids = family.available.clone();
                let label = FAMILIES
                    .iter()
                    .find(|f| f.name == name)
                    .map_or(name, Family::label);
                set(&|s| {
                    s.busy = Some(tf("installing {name}…", &[("name", &label)]));
                    s.error = None;
                });
                let result = kit.install(&ids);
                let failed = |e: &str| {
                    tf(
                        "installing {name} failed: {error}",
                        &[("name", &label), ("error", &e)],
                    )
                };
                finish(shared, wake, &format!("installing {name}"), &failed, result);
                let _ = kit
                    .resolve_families()
                    .map(|f| set(&|s| s.families = f.clone()));
            }
            Job::Remove { label, ids } => {
                set(&|s| {
                    s.busy = Some(tf("removing {name}…", &[("name", &label)]));
                    s.error = None;
                });
                let result = kit.remove(&ids);
                let failed = |e: &str| {
                    tf(
                        "removing {name} failed: {error}",
                        &[("name", &label), ("error", &e)],
                    )
                };
                finish(shared, wake, &format!("removing {label}"), &failed, result);
                let _ = kit
                    .resolve_families()
                    .map(|f| set(&|s| s.families = f.clone()));
                let mut found = BTreeMap::new();
                for path in &asked_owners {
                    if let Ok(Some(owner)) = kit.owner_of(path) {
                        found.insert(path.clone(), owner);
                    }
                }
                set(&|s| s.owners = found.clone());
            }
        }
    }
}

fn finish(
    shared: &Mutex<Snapshot>,
    wake: &dyn Fn(),
    what: &str,
    failed: &dyn Fn(&str) -> String,
    result: Result<(), String>,
) {
    let mut s = lock(shared);
    s.busy = None;
    match result {
        Ok(()) => {
            s.changes += 1;
            log::info!("{what}: done");
        }
        Err(e) => {
            log::warn!("{what}: {e}");
            s.error = Some(failed(&e));
        }
    }
    drop(s);
    wake();
}

/// PackageKit's numbers (pk-enum.h); bitfields are `1 << value`.
const FILTER_INSTALLED: u64 = 1 << 2;
const FILTER_ARCH: u64 = 1 << 18;
const INFO_INSTALLED: u32 = 1;
const FLAG_ONLY_TRUSTED: u64 = 1 << 1;
const EXIT_SUCCESS: u32 = 1;

struct Kit {
    bus: Connection,
}

/// What a transaction said before it finished.
struct Outcome {
    packages: Vec<(u32, String)>,
    error: Option<String>,
    exit: u32,
}

impl Kit {
    fn connect() -> zbus::Result<Kit> {
        let bus = Connection::system()?;
        let kit = Kit { bus };
        let version: u32 = kit.root()?.get_property("VersionMajor")?;
        log::info!("PackageKit {version}.x");
        Ok(kit)
    }

    fn root(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(
            &self.bus,
            "org.freedesktop.PackageKit",
            "/org/freedesktop/PackageKit",
            "org.freedesktop.PackageKit",
        )
    }

    /// Runs one transaction: `call` starts it, the signals are read until `Finished`.
    fn transaction(
        &self,
        call: impl FnOnce(&Proxy<'_>) -> zbus::Result<()>,
    ) -> Result<Outcome, String> {
        let text = |e: zbus::Error| e.to_string();
        let path: OwnedObjectPath = self
            .root()
            .and_then(|p| p.call("CreateTransaction", &()))
            .map_err(text)?;
        let proxy = Proxy::new(
            &self.bus,
            "org.freedesktop.PackageKit",
            path,
            "org.freedesktop.PackageKit.Transaction",
        )
        .map_err(text)?;
        let signals = proxy.receive_all_signals().map_err(text)?;
        let hints = ["interactive=true".to_owned()];
        proxy
            .call::<_, _, ()>("SetHints", &(hints.as_slice(),))
            .map_err(text)?;
        call(&proxy).map_err(text)?;
        let mut outcome = Outcome {
            packages: Vec::new(),
            error: None,
            exit: 0,
        };
        for message in signals {
            let header = message.header();
            let Some(member) = header.member() else {
                continue;
            };
            let body = message.body();
            match member.as_str() {
                "Package" => {
                    if let Ok((info, id, _summary)) = body.deserialize::<(u32, String, String)>() {
                        outcome.packages.push((info, id));
                    }
                }
                "ErrorCode" => {
                    if let Ok((_code, details)) = body.deserialize::<(u32, String)>() {
                        outcome.error = Some(details);
                    }
                }
                "Finished" => {
                    if let Ok((exit, _runtime)) = body.deserialize::<(u32, u32)>() {
                        outcome.exit = exit;
                    }
                    return Ok(outcome);
                }
                _ => {}
            }
        }
        Err(t("PackageKit went away").into())
    }

    fn resolve_families(&self) -> Result<BTreeMap<&'static str, FamilyState>, String> {
        let names: Vec<&str> = FAMILIES.iter().flat_map(|f| f.packages).copied().collect();
        let outcome = self.transaction(|p| p.call("Resolve", &(FILTER_ARCH, names.as_slice())))?;
        let mut families = BTreeMap::new();
        for family in FAMILIES {
            let mut state = FamilyState::default();
            for (info, id) in &outcome.packages {
                let name = id.split(';').next().unwrap_or("");
                if !family.packages.contains(&name) {
                    continue;
                }
                if *info == INFO_INSTALLED || id.split(';').nth(3).is_some_and(is_installed) {
                    if !state.installed.iter().any(|i| same_name(i, name)) {
                        state.installed.push(id.clone());
                    }
                } else if !state.available.iter().any(|i| same_name(i, name)) {
                    state.available.push(id.clone());
                }
            }
            state.available.retain(|a| {
                !state
                    .installed
                    .iter()
                    .any(|i| same_name(i, package_name(a)))
            });
            families.insert(family.name, state);
        }
        Ok(families)
    }

    fn owner_of(&self, path: &str) -> Result<Option<Owner>, String> {
        let path = path.trim_end_matches('/');
        let outcome =
            self.transaction(|p| p.call("SearchFiles", &(FILTER_INSTALLED, [path].as_slice())))?;
        Ok(outcome
            .packages
            .into_iter()
            .next()
            .map(|(_, id)| Owner { id }))
    }

    fn install(&self, ids: &[String]) -> Result<(), String> {
        if ids.is_empty() {
            return Err(t("nothing left to install").into());
        }
        let outcome = self.transaction(|p| p.call("InstallPackages", &(FLAG_ONLY_TRUSTED, ids)))?;
        outcome_result(outcome)
    }

    fn remove(&self, ids: &[String]) -> Result<(), String> {
        let outcome = self.transaction(|p| p.call("RemovePackages", &(0u64, ids, false, false)))?;
        outcome_result(outcome)
    }
}

fn outcome_result(outcome: Outcome) -> Result<(), String> {
    match (outcome.exit, outcome.error) {
        (EXIT_SUCCESS, _) => Ok(()),
        (_, Some(error)) => Err(error),
        (exit, None) => Err(tf("PackageKit exit code {exit}", &[("exit", &exit)])),
    }
}

/// The data field of an installed package id: `installed` or `installed:<repo>`.
fn is_installed(data: &str) -> bool {
    data == "installed" || data.starts_with("installed:")
}

fn package_name(id: &str) -> &str {
    id.split(';').next().unwrap_or(id)
}

fn same_name(id: &str, name: &str) -> bool {
    package_name(id) == name
}

/// The command that installs a family's packages for this distribution, for systems
/// without PackageKit.
pub fn command_for(family: &Family) -> Option<String> {
    if in_flatpak() {
        return flatpak_command(family);
    }
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let ids: Vec<&str> = os
        .lines()
        .filter_map(|l| l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")))
        .flat_map(|v| v.trim_matches('"').split_whitespace())
        .collect();
    let is = |id: &str| ids.contains(&id);
    let pick = |names: &[&str]| names.join(" ");
    let (tool, packages) = if is("debian") || is("ubuntu") {
        (
            "sudo apt install",
            match family.name {
                "LSP Plugins" => pick(&["lsp-plugins-clap", "lsp-plugins-lv2"]),
                "Calf Studio Gear" => pick(&["calf-plugins"]),
                "ZAM Plugins" => pick(&["zam-plugins"]),
                "x42 Plugins" => pick(&["x42-plugins"]),
                "Dragonfly Reverb" => pick(&["dragonfly-reverb-clap", "dragonfly-reverb-lv2"]),
                _ => pick(&["liblilv-0-0", "libsuil-0-0"]),
            },
        )
    } else if is("fedora") {
        (
            "sudo dnf install",
            match family.name {
                "LSP Plugins" => pick(&["lsp-plugins-clap", "lsp-plugins-lv2"]),
                "Calf Studio Gear" => pick(&["calf"]),
                "ZAM Plugins" => pick(&["zam-plugins"]),
                "LV2 support" => pick(&["lilv-libs", "suil"]),
                _ => return None,
            },
        )
    } else {
        return None;
    };
    Some(format!("{tool} {packages}"))
}

fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// The family's Flathub LinuxAudio extension, on the branch of this Flatpak's runtime (the
/// manifest mounts them at /app/extensions/Plugins). LV2 support is built into the Flatpak.
fn flatpak_command(family: &Family) -> Option<String> {
    let info = std::fs::read_to_string("/.flatpak-info").unwrap_or_default();
    extension_command(family, &info)
}

fn extension_command(family: &Family, flatpak_info: &str) -> Option<String> {
    let extension = match family.name {
        "LSP Plugins" => "LSP",
        "Calf Studio Gear" => "Calf",
        "ZAM Plugins" => "ZamPlugins",
        "x42 Plugins" => "x42Plugins",
        "Dragonfly Reverb" => "DragonflyReverb",
        _ => return None,
    };
    // `runtime=runtime/org.freedesktop.Platform/x86_64/26.08`
    let branch = flatpak_info
        .lines()
        .find_map(|l| l.strip_prefix("runtime="))
        .and_then(|r| r.rsplit('/').next())
        .unwrap_or("26.08");
    Some(format!(
        "flatpak install flathub org.freedesktop.LinuxAudio.Plugins.{extension}//{branch}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_ids_split_into_name_and_state() {
        let id = "lsp-plugins-clap;1.2.29-1.fc44;x86_64;installed:updates";
        assert_eq!(package_name(id), "lsp-plugins-clap");
        assert!(is_installed(id.split(';').nth(3).unwrap()));
        assert!(!is_installed("fedora"));
        assert_eq!(
            Owner { id: id.into() }.name(),
            "lsp-plugins-clap",
            "the owner reads as its package name"
        );
    }

    #[test]
    fn every_family_has_packages_and_one_is_lv2_support() {
        assert!(FAMILIES.iter().all(|f| !f.packages.is_empty()));
        assert_eq!(FAMILIES.iter().filter(|f| f.lv2_support).count(), 1);
    }

    #[test]
    fn inside_the_flatpak_families_are_extensions_on_the_runtime_branch() {
        let info = "[Application]\nname=io.github.claritas_audio.Canalis\n\
                    runtime=runtime/org.freedesktop.Platform/x86_64/26.08\n";
        let command = |name| {
            let family = FAMILIES.iter().find(|f| f.name == name).unwrap();
            extension_command(family, info)
        };
        assert_eq!(
            command("LSP Plugins").as_deref(),
            Some("flatpak install flathub org.freedesktop.LinuxAudio.Plugins.LSP//26.08")
        );
        assert!(
            command("ZAM Plugins")
                .unwrap()
                .ends_with(".ZamPlugins//26.08")
        );
        assert_eq!(
            command("LV2 support"),
            None,
            "lilv and suil are in the Flatpak"
        );
    }
}
