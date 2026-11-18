//! The service owns the audio: `canalis --daemon` runs the control plane and its chains
//! and serves them on a socket (`canalis_pw::remote`); windows, the tray and CLI calls are
//! its clients. A window starts the service when none answers and never stops it.
//! systemd runs it when it can, through a user unit that is rewritten whenever the
//! binary it names has moved. Inside the Flatpak the portal starts it in a sandbox of its
//! own and the Background portal starts it at login (`background`).

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::i18n::t;
use canalis_chain::ipc::ToChain;
use canalis_pw::remote::{self, Connection, Down, Remote, Up};
use canalis_pw::{ChainState, Command, Graph, Handle, HoldTarget};
use log::{info, warn};

pub const APP_ID: &str = "io.github.claritas_audio.Canalis";
const UNIT: &str = "canalis.service";
/// How long a CLI call waits for PipeWire to answer the service, or the service to stop.
const CLI_WAIT: Duration = Duration::from_secs(10);

/// The control plane with no window, serving its socket until SIGINT, SIGTERM or a
/// client's `shutdown`.
pub fn daemon() -> std::io::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let path = socket()?;
    let listener = remote::bind(&path)?;
    let pw = match std::env::var_os("CANALIS_CONFIG") {
        Some(config) => canalis_pw::spawn_with_config(|| {}, config.into())?,
        None => canalis_pw::spawn(|| {})?,
    };
    crate::tray::spawn(path.clone(), Arc::clone(&stop));
    crate::notify::spawn(path.clone(), Arc::clone(&stop));
    info!("Canalis service running; stop with SIGTERM or `canalis quit`");
    remote::serve(&pw, listener, &path, &stop);
    info!("Canalis service stopping");
    drop(pw);
    Ok(())
}

/// Brings the window already open forward instead of opening a second one; `true` when
/// it did. A private control plane (`CANALIS_CONFIG`) and a screenshot run always get
/// their own window.
pub fn raise_open_window() -> bool {
    std::env::var_os("CANALIS_CONFIG").is_none()
        && std::env::var_os("CANALIS_SCREENSHOT").is_none()
        && socket().is_ok_and(|path| remote::raise_window(&path))
}

fn socket() -> std::io::Result<PathBuf> {
    remote::socket_path().ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is not set"))
}

/// What the window talks to: the service, or with `CANALIS_CONFIG` a private control
/// plane of its own (screenshot runs, tests).
pub enum Plane {
    Local(Handle),
    Remote(Remote),
}

impl Plane {
    pub fn open(wake: impl Fn() + Send + 'static) -> std::io::Result<Plane> {
        if let Some(config) = std::env::var_os("CANALIS_CONFIG") {
            return Ok(Plane::Local(canalis_pw::spawn_with_config(
                wake,
                config.into(),
            )?));
        }
        Ok(Plane::Remote(Remote::spawn(socket()?, true, wake, start)?))
    }

    pub fn latest(&self) -> Option<Arc<Graph>> {
        match self {
            Plane::Local(handle) => handle.latest(),
            Plane::Remote(remote) => remote.latest(),
        }
    }

    pub fn send(&self, command: Command) {
        match self {
            Plane::Local(handle) => handle.send(command),
            Plane::Remote(remote) => remote.send(command),
        }
    }

    /// `None` when the service is attached (or there is none to wait for), else why not.
    pub fn waiting(&self) -> Option<&'static str> {
        match self {
            Plane::Local(_) => None,
            Plane::Remote(r) if r.stopped() => Some(t("Canalis was quit")),
            Plane::Remote(r) if !r.attached() => Some(t("starting the Canalis service")),
            Plane::Remote(_) => None,
        }
    }

    /// Another launch asked this window to come forward.
    pub fn take_raise(&self) -> bool {
        matches!(self, Plane::Remote(r) if r.take_raise())
    }

    /// The service was stopped on purpose (Quit in the tray, `canalis quit`), or the tray
    /// icon's click closed this window.
    pub fn stopped(&self) -> bool {
        matches!(self, Plane::Remote(r) if r.stopped() || r.closing())
    }
}

/// Starts the service: through systemd when there is a user instance, else as a child
/// in its own session so it outlives the window.
pub fn start() {
    if in_flatpak() {
        match crate::background::spawn_service(service_log().as_ref()) {
            Ok(pid) => info!("the Flatpak portal started the service ({pid})"),
            Err(e) => warn!("cannot start the service through the Flatpak portal: {e}"),
        }
        return;
    }
    match write_unit() {
        Ok(()) if systemctl_ok(&["start", UNIT]) => {
            info!("asked systemd to start {UNIT}");
            return;
        }
        Ok(()) => warn!("systemd did not start {UNIT}"),
        Err(e) => warn!("cannot write {UNIT}: {e}"),
    }
    spawn_detached();
}

/// This program's path. After an update replaced the file under a running process, Linux
/// reports the old one as "<path> (deleted)"; the new file at `<path>` is the one to run.
pub fn executable() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let replaced = exe
        .to_str()
        .and_then(|p| p.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
        .filter(|p| p.is_file());
    Ok(replaced.unwrap_or(exe))
}

/// `service.log` next to the socket, emptied.
fn service_log() -> Option<std::fs::File> {
    socket()
        .ok()
        .and_then(|p| std::fs::File::create(p.with_file_name("service.log")).ok())
}

fn spawn_detached() {
    let Ok(exe) = executable() else {
        warn!("cannot find this executable to start the service");
        return;
    };
    let log = service_log();
    let output = || {
        log.as_ref()
            .and_then(|f| f.try_clone().ok())
            .map_or_else(Stdio::null, Stdio::from)
    };
    // `setsid` puts the service in its own session, so closing the terminal or the
    // window's process group never takes it down.
    let spawn = |program: &std::path::Path, args: &[&std::ffi::OsStr]| {
        Process::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(output())
            .stderr(output())
            .process_group(0)
            .spawn()
    };
    let daemon = std::ffi::OsStr::new("--daemon");
    let spawned =
        spawn("setsid".as_ref(), &[exe.as_os_str(), daemon]).or_else(|_| spawn(&exe, &[daemon]));
    match spawned {
        Ok(mut child) => {
            info!("started the service as a child ({})", child.id());
            std::thread::spawn(move || child.wait());
        }
        Err(e) => warn!("cannot start the service: {e}"),
    }
}

fn home() -> std::io::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other("HOME is not set"))
}

fn unit_path() -> std::io::Result<PathBuf> {
    Ok(home()?.join(".config/systemd/user").join(UNIT))
}

fn desktop_path() -> std::io::Result<PathBuf> {
    Ok(home()?.join(format!(".local/share/applications/{APP_ID}.desktop")))
}

fn icon_path() -> std::io::Result<PathBuf> {
    Ok(home()?.join(format!(
        ".local/share/icons/hicolor/scalable/apps/{APP_ID}.svg"
    )))
}

pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// How systemd and the desktop start this build, without its arguments.
fn launcher() -> std::io::Result<String> {
    let exe = executable()?;
    Ok(format!("\"{}\"", exe.to_string_lossy()))
}

fn unit_text() -> std::io::Result<String> {
    // `%` starts a specifier in a unit file.
    let exec = launcher()?.replace('%', "%%");
    Ok(format!(
        "[Unit]\nDescription=Canalis audio routing\nAfter=pipewire.service wireplumber.service\n\n\
         [Service]\nType=simple\nExecStart={exec} --daemon\nRestart=on-failure\nRestartSec=2\n\
         Environment=RUST_LOG=canalis=info,canalis_pw=info,canalis_chain=info\n\n\
         [Install]\nWantedBy=default.target\n"
    ))
}

/// Writes the user unit when it is missing or names another binary, then reloads systemd.
fn write_unit() -> std::io::Result<()> {
    let path = unit_path()?;
    let text = unit_text()?;
    if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    info!("wrote {}", path.display());
    systemctl(&["daemon-reload"]);
    Ok(())
}

/// The unit, the desktop entry and the icon; nothing inside the Flatpak, which exports its
/// own entry and starts at login through the Background portal.
pub fn install() -> std::io::Result<()> {
    if in_flatpak() {
        return Ok(());
    }
    write_unit()?;
    let exec = launcher()?;
    let files = [
        (
            desktop_path()?,
            format!(
                "[Desktop Entry]\nType=Application\nName=Canalis\nComment=PipeWire router with effect chains\n\
                 Exec={exec}\nIcon={APP_ID}\nTerminal=false\nCategories=AudioVideo;Audio;Mixer;\n\
                 StartupWMClass={APP_ID}\n"
            ),
        ),
        (
            icon_path()?,
            include_str!("../../../assets/canalis-app-icon.svg").to_owned(),
        ),
    ];
    for (path, text) in &files {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)?;
        info!("wrote {}", path.display());
    }
    Ok(())
}

/// Stops starting at login and removes what `install` wrote; a running service keeps
/// running until it is quit.
pub fn uninstall() -> std::io::Result<()> {
    if in_flatpak() {
        crate::background::set_autostart(false);
        return Ok(());
    }
    systemctl(&["disable", UNIT]);
    for path in [unit_path()?, desktop_path()?, icon_path()?] {
        match std::fs::remove_file(&path) {
            Ok(()) => info!("removed {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("cannot remove {}: {e}", path.display()),
        }
    }
    systemctl(&["daemon-reload"]);
    Ok(())
}

/// What the settings popover shows about the service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceState {
    /// The desktop entry is there (or this is a Flatpak, which has its own).
    pub installed: bool,
    pub enabled: bool,
}

pub fn state() -> ServiceState {
    ServiceState {
        installed: in_flatpak() || desktop_path().is_ok_and(|p| p.is_file()),
        enabled: if in_flatpak() {
            crate::background::enabled()
        } else {
            systemctl(&["is-enabled", UNIT]).trim() == "enabled"
        },
    }
}

/// Starts the service at login from now on, or stops doing so; it keeps running either way.
pub fn set_enabled(enabled: bool) {
    if in_flatpak() {
        crate::background::set_autostart(enabled);
    } else if enabled {
        if let Err(e) = write_unit() {
            warn!("cannot write {UNIT}: {e}");
        }
        systemctl(&["enable", UNIT]);
    } else {
        systemctl(&["disable", UNIT]);
    }
}

fn systemctl_process(args: &[&str]) -> Process {
    let mut process = Process::new("systemctl");
    process.arg("--user").args(args);
    process
}

fn systemctl(args: &[&str]) -> String {
    match systemctl_process(args).stderr(Stdio::null()).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(e) => {
            warn!("systemctl --user {}: {e}", args.join(" "));
            String::new()
        }
    }
}

fn systemctl_ok(args: &[&str]) -> bool {
    systemctl_process(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

const USAGE: &str =
    "usage: canalis [--daemon | --install | --uninstall | --version | quit | scene [name]
             | mute mic|chains [on|off|toggle] | chain <name> mute|bypass [on|off|toggle]
             | ptt mic|<chain> press|release|hold | pad [<name> [press|release|stop] | --stop]]";

/// `canalis quit` and the requests a keybind or script makes to the running service. None
/// of them starts the service: a hotkey must not spawn one behind the user's back.
pub fn cli(args: &[String]) -> std::io::Result<()> {
    let path = socket()?;
    if let [quit] = args
        && quit == "quit"
    {
        if Connection::open(&path).is_err() {
            println!("the Canalis service is not running");
        } else if remote::shut_down(&path, CLI_WAIT) {
            println!("the Canalis service stopped");
        } else {
            return Err(std::io::Error::other("the service did not stop"));
        }
        return Ok(());
    }
    let request =
        parse(args).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, USAGE))?;
    let mut connection = running(&path)?;
    let graph = connected_graph(&mut connection)?;
    let (commands, said) = resolve(request, &graph).map_err(std::io::Error::other)?;
    for command in commands {
        connection.send(&Down::Command { command })?;
    }
    if !said.is_empty() {
        println!("{said}");
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
enum Request {
    Scenes,
    Scene(String),
    Pads,
    Pad(String, PadAction),
    StopPads,
    MuteMic(Switch),
    MuteChains(Switch),
    Chain {
        name: String,
        what: ChainSwitch,
        switch: Switch,
    },
    Talk {
        target: Talker,
        key: Key,
    },
}

/// What `canalis pad <name>` does; a key bound to press and release plays a Hold pad.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PadAction {
    Press,
    Release,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Switch {
    On,
    Off,
    Toggle,
}

impl Switch {
    fn apply(self, now: bool) -> bool {
        match self {
            Switch::On => true,
            Switch::Off => false,
            Switch::Toggle => !now,
        }
    }
}

#[derive(Debug, PartialEq)]
enum ChainSwitch {
    Mute,
    Bypass,
}

/// `mic` is the default input's own mute; anything else names a chain.
#[derive(Debug, PartialEq)]
enum Talker {
    Mic,
    Chain(String),
}

#[derive(Debug, PartialEq)]
enum Key {
    Press,
    Release,
    /// Sent on every repeat of a held key, for compositors with no release bind.
    Hold,
}

fn parse(args: &[String]) -> Option<Request> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Some(match args.as_slice() {
        ["scene"] => Request::Scenes,
        ["scene", name] => Request::Scene((*name).to_owned()),
        ["pad"] => Request::Pads,
        ["pad", "--stop"] => Request::StopPads,
        ["pad", name] => Request::Pad((*name).to_owned(), PadAction::Press),
        ["pad", name, action] => Request::Pad(
            (*name).to_owned(),
            match *action {
                "press" => PadAction::Press,
                "release" => PadAction::Release,
                "stop" => PadAction::Stop,
                _ => return None,
            },
        ),
        ["mute", "mic", rest @ ..] => Request::MuteMic(switch(rest)?),
        ["mute", "chains", rest @ ..] => Request::MuteChains(switch(rest)?),
        ["chain", name, what, rest @ ..] => Request::Chain {
            name: (*name).to_owned(),
            what: match *what {
                "mute" => ChainSwitch::Mute,
                "bypass" => ChainSwitch::Bypass,
                _ => return None,
            },
            switch: switch(rest)?,
        },
        ["ptt", target, key] => Request::Talk {
            target: match *target {
                "mic" => Talker::Mic,
                name => Talker::Chain(name.to_owned()),
            },
            key: match *key {
                "press" => Key::Press,
                "release" => Key::Release,
                "hold" => Key::Hold,
                _ => return None,
            },
        },
        _ => return None,
    })
}

fn switch(rest: &[&str]) -> Option<Switch> {
    match rest {
        [] | ["toggle"] => Some(Switch::Toggle),
        ["on"] => Some(Switch::On),
        ["off"] => Some(Switch::Off),
        _ => None,
    }
}

/// The commands a request makes against the service's current graph, and the line to
/// print; an error names what is missing.
fn resolve(request: Request, graph: &Graph) -> Result<(Vec<Command>, String), String> {
    match request {
        Request::Scenes => {
            let names: Vec<&str> = graph.scenes.iter().map(|s| s.name.as_str()).collect();
            Ok((Vec::new(), names.join("\n")))
        }
        Request::Scene(name) => {
            let scene = graph
                .scenes
                .iter()
                .find(|s| s.name == name)
                .cloned()
                .ok_or_else(|| format!("no scene named {name:?}"))?;
            Ok((
                vec![Command::ApplyScene { scene }],
                format!("recalled {name}"),
            ))
        }
        Request::Pads => {
            let names: Vec<&str> = graph
                .soundboard
                .pads
                .iter()
                .map(|p| p.name.as_str())
                .collect();
            Ok((Vec::new(), names.join("\n")))
        }
        Request::Pad(name, action) => {
            if !graph.soundboard.enabled {
                return Err("the soundboard is off; switch it on in Settings".to_owned());
            }
            if graph.soundboard.pad(&name).is_none() {
                return Err(format!("no pad named {name:?}"));
            }
            let state = graph.pads.iter().find(|p| p.name == name);
            if let Some(e) = state.and_then(|p| p.error.as_ref()) {
                return Err(format!("pad {name:?} cannot play: {e}"));
            }
            let (command, said) = match action {
                PadAction::Press => (Command::PressPad { name: name.clone() }, "pressed"),
                PadAction::Release => (Command::ReleasePad { name: name.clone() }, "released"),
                PadAction::Stop => (Command::StopPad { name: name.clone() }, "stopped"),
            };
            Ok((vec![command], format!("{name} {said}")))
        }
        Request::StopPads => Ok((vec![Command::StopPads], "pads stopped".to_owned())),
        Request::MuteMic(switch) => {
            let (node, muted) = microphone(graph)?;
            let mute = switch.apply(muted);
            let said = if mute {
                "microphone muted"
            } else {
                "microphone unmuted"
            };
            Ok((vec![Command::SetMute { node, mute }], said.to_owned()))
        }
        Request::MuteChains(switch) => {
            if graph.chains.is_empty() {
                return Err("there are no chains".to_owned());
            }
            let mute = switch.apply(graph.chains.iter().all(|c| c.mute));
            let commands = graph
                .chains
                .iter()
                .map(|c| Command::SetChainMute {
                    name: c.name.clone(),
                    mute,
                })
                .collect();
            let said = if mute {
                "every chain muted"
            } else {
                "every chain unmuted"
            };
            Ok((commands, said.to_owned()))
        }
        Request::Chain {
            name,
            what: ChainSwitch::Mute,
            switch,
        } => {
            let chain = find_chain(graph, &name)?;
            let mute = switch.apply(chain.mute);
            let said = if mute { "muted" } else { "unmuted" };
            Ok((
                vec![Command::SetChainMute {
                    name: chain.name.clone(),
                    mute,
                }],
                format!("{} {said}", chain.label),
            ))
        }
        Request::Chain {
            name,
            what: ChainSwitch::Bypass,
            switch,
        } => {
            let chain = find_chain(graph, &name)?;
            if chain.slots.is_empty() {
                return Err(format!("{} has no slots to bypass", chain.label));
            }
            let all = chain
                .slots
                .iter()
                .all(|s| s.get("bypass").and_then(toml::Value::as_bool) == Some(true));
            let bypass = switch.apply(all);
            let commands = (0..chain.slots.len())
                .map(|slot| {
                    toml::Value::try_from(ToChain::Bypass { slot, bypass }).map(|message| {
                        Command::ChainMessage {
                            name: chain.name.clone(),
                            message,
                        }
                    })
                })
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let said = if bypass { "bypassed" } else { "active" };
            Ok((commands, format!("{} {said}", chain.label)))
        }
        Request::Talk { target, key } => {
            let (target, who) = match target {
                Talker::Mic => (
                    HoldTarget::Node {
                        node: microphone(graph)?.0,
                    },
                    "microphone".to_owned(),
                ),
                Talker::Chain(name) => {
                    let chain = find_chain(graph, &name)?;
                    (
                        HoldTarget::Chain {
                            name: chain.name.clone(),
                        },
                        chain.label.clone(),
                    )
                }
            };
            let command = match (key, target) {
                (Key::Hold, target) => Command::Hold { target },
                (key, HoldTarget::Node { node }) => Command::SetMute {
                    node,
                    mute: key == Key::Release,
                },
                (key, HoldTarget::Chain { name }) => Command::SetChainMute {
                    name,
                    mute: key == Key::Release,
                },
            };
            let said = match &command {
                Command::SetMute { mute: true, .. } | Command::SetChainMute { mute: true, .. } => {
                    format!("{who} muted")
                }
                _ => format!("{who} open"),
            };
            Ok((vec![command], said))
        }
    }
}

/// The default input and whether its own mute is on, as the tray's item reads it.
fn microphone(graph: &Graph) -> Result<(canalis_pw::NodeId, bool), String> {
    let node = graph
        .defaults
        .source
        .as_deref()
        .and_then(|name| graph.node_by_name(name))
        .ok_or("there is no default input")?;
    Ok((node.id, node.volume.as_ref().is_some_and(|v| v.mute)))
}

/// A chain by its name, else by the label the window shows.
fn find_chain<'a>(graph: &'a Graph, name: &str) -> Result<&'a ChainState, String> {
    graph
        .chain(name)
        .or_else(|| graph.chains.iter().find(|c| c.label == name))
        .ok_or_else(|| format!("no chain named {name:?}"))
}

/// A connection to the service of this build, or why there is none.
fn running(path: &std::path::Path) -> std::io::Result<Connection> {
    let connection = Connection::open(path)
        .map_err(|_| std::io::Error::other("the Canalis service is not running"))?;
    if !connection.compatible() {
        return Err(std::io::Error::other(format!(
            "the running Canalis service is another build ({}); open the window once to restart it",
            connection.build
        )));
    }
    Ok(connection)
}

/// The first graph that shows PipeWire connected.
fn connected_graph(connection: &mut Connection) -> std::io::Result<Graph> {
    let deadline = Instant::now() + CLI_WAIT;
    loop {
        if let Up::Graph { graph } = connection.receive()?
            && graph.connected
        {
            return Ok(*graph);
        }
        if Instant::now() > deadline {
            return Err(std::io::Error::other("PipeWire did not answer the service"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn keybind_lines_parse() {
        assert_eq!(
            parse(&args("mute mic")),
            Some(Request::MuteMic(Switch::Toggle))
        );
        assert_eq!(
            parse(&args("mute chains off")),
            Some(Request::MuteChains(Switch::Off))
        );
        assert_eq!(
            parse(&args("chain voice bypass on")),
            Some(Request::Chain {
                name: "voice".into(),
                what: ChainSwitch::Bypass,
                switch: Switch::On,
            })
        );
        assert_eq!(
            parse(&args("ptt mic hold")),
            Some(Request::Talk {
                target: Talker::Mic,
                key: Key::Hold,
            })
        );
        assert_eq!(
            parse(&args("ptt voice release")),
            Some(Request::Talk {
                target: Talker::Chain("voice".into()),
                key: Key::Release,
            })
        );
        assert_eq!(parse(&args("scene")), Some(Request::Scenes));
        assert_eq!(
            parse(&args("pad airhorn")),
            Some(Request::Pad("airhorn".into(), PadAction::Press))
        );
        assert_eq!(
            parse(&args("pad airhorn release")),
            Some(Request::Pad("airhorn".into(), PadAction::Release))
        );
        assert_eq!(parse(&args("pad --stop")), Some(Request::StopPads));
        for wrong in [
            "mute",
            "mute mic loud",
            "chain voice solo",
            "ptt mic",
            "ptt mic down",
        ] {
            assert_eq!(parse(&args(wrong)), None, "{wrong}");
        }
    }

    #[test]
    fn toggle_flips_and_on_off_set() {
        assert!(Switch::Toggle.apply(false));
        assert!(!Switch::Toggle.apply(true));
        assert!(Switch::On.apply(true));
        assert!(!Switch::Off.apply(true));
    }

    fn graph_with_mic() -> Graph {
        let mic = canalis_pw::Node::from_props(42, |key| match key {
            "node.name" => Some("usb-mic"),
            "media.class" => Some("Audio/Source"),
            _ => None,
        });
        let mut graph = Graph {
            connected: true,
            nodes: vec![mic],
            ..Graph::default()
        };
        graph.defaults.source = Some("usb-mic".into());
        graph
    }

    #[test]
    fn mic_requests_reach_the_default_input() {
        let graph = graph_with_mic();
        let (commands, said) = resolve(Request::MuteMic(Switch::Toggle), &graph).unwrap();
        assert_eq!(
            commands,
            vec![Command::SetMute {
                node: canalis_pw::NodeId(42),
                mute: true,
            }]
        );
        assert_eq!(said, "microphone muted");
        let talk = |key| {
            resolve(
                Request::Talk {
                    target: Talker::Mic,
                    key,
                },
                &graph,
            )
            .unwrap()
            .0
        };
        assert_eq!(
            talk(Key::Press),
            vec![Command::SetMute {
                node: canalis_pw::NodeId(42),
                mute: false,
            }]
        );
        assert_eq!(
            talk(Key::Hold),
            vec![Command::Hold {
                target: HoldTarget::Node {
                    node: canalis_pw::NodeId(42),
                },
            }]
        );
    }

    #[test]
    fn missing_names_are_errors() {
        let graph = graph_with_mic();
        assert_eq!(
            resolve(Request::Scene("live".into()), &graph).unwrap_err(),
            "no scene named \"live\""
        );
        assert_eq!(
            resolve(Request::Pad("airhorn".into(), PadAction::Press), &graph).unwrap_err(),
            "the soundboard is off; switch it on in Settings"
        );
        assert_eq!(
            resolve(Request::MuteChains(Switch::On), &graph).unwrap_err(),
            "there are no chains"
        );
        let talk = Request::Talk {
            target: Talker::Chain("voice".into()),
            key: Key::Press,
        };
        assert_eq!(
            resolve(talk, &graph).unwrap_err(),
            "no chain named \"voice\""
        );
        assert_eq!(
            resolve(Request::MuteMic(Switch::On), &Graph::default()).unwrap_err(),
            "there is no default input"
        );
    }
}
