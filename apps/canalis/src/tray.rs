//! The top-bar icon, owned by the service (the process that lives): a StatusNotifierItem
//! whose click opens or closes the window and whose menu opens it, picks the default output and input, recalls scenes, mutes
//! every chain and quits. It is one more client of the service's socket. Without a tray
//! host the service runs on and the icon appears if a host shows up later.

use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use canalis_pw::remote::{self, Remote};
use canalis_pw::{Command, Graph, MediaClass, VirtualKind};
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, ToolTip};
use log::{info, warn};

use crate::i18n::{t, tf};

/// How often the menu follows the graph.
const TICK: Duration = Duration::from_millis(250);
/// Ticks between looks at the language chosen in Settings.
const LANGUAGE_EVERY: u32 = 8;
const ICON_SIZES: [u32; 2] = [24, 48];

struct Tray {
    graph: Arc<Graph>,
    icons: Vec<Icon>,
    socket: PathBuf,
    commands: mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
}

/// Starts the icon on its own thread; it ends when `stop` is set or the service says bye.
pub fn spawn(socket: PathBuf, stop: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || run(socket, stop));
    if let Err(e) = spawned {
        warn!("no thread for the tray icon: {e}");
    }
}

fn run(socket: PathBuf, stop: Arc<AtomicBool>) {
    let remote = match Remote::spawn(socket.clone(), false, || {}, || {}) {
        Ok(remote) => remote,
        Err(e) => {
            warn!("tray icon off: {e}");
            return;
        }
    };
    let (commands, pending) = mpsc::channel();
    let tray = Tray {
        graph: Arc::default(),
        icons: ICON_SIZES.iter().filter_map(|&size| icon(size)).collect(),
        socket,
        commands,
        stop: Arc::clone(&stop),
    };
    let handle = match tray
        .disable_dbus_name(std::env::var_os("FLATPAK_ID").is_some())
        // At login the bar may come up after the service; wait for it instead of failing.
        .assume_sni_available(true)
        .spawn()
    {
        Ok(handle) => handle,
        Err(e) => {
            info!("no tray icon ({e}); the service runs without it");
            return;
        }
    };
    let mut ticks = 0u32;
    while !stop.load(Ordering::Relaxed) && !remote.stopped() {
        for command in pending.try_iter() {
            remote.send(command);
        }
        if let Some(graph) = remote.latest() {
            handle.update(|t| t.graph = Arc::clone(&graph));
        }
        ticks = ticks.wrapping_add(1);
        // The language is chosen in the window, another process: follow it.
        if ticks.is_multiple_of(LANGUAGE_EVERY) && crate::i18n::reload() {
            handle.update(|_| {});
        }
        std::thread::sleep(TICK);
    }
    handle.shutdown().wait();
}

impl Tray {
    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    fn device_picker(&self, label: &str, class: MediaClass, kind: VirtualKind) -> MenuItem<Self> {
        let current = match kind {
            VirtualKind::Sink => self.graph.defaults.sink.as_deref(),
            VirtualKind::Source => self.graph.defaults.source.as_deref(),
        };
        let devices: Vec<(String, String)> = self
            .graph
            .nodes
            .iter()
            .filter(|n| n.class == class && !self.graph.is_hidden(&n.name))
            .map(|n| (n.name.clone(), n.label.clone()))
            .collect();
        let selected = devices
            .iter()
            .position(|(name, _)| Some(name.as_str()) == current)
            .unwrap_or(usize::MAX);
        let options = devices
            .iter()
            .map(|(_, label)| RadioItem {
                label: label.clone(),
                ..Default::default()
            })
            .collect();
        let names: Vec<String> = devices.into_iter().map(|(name, _)| name).collect();
        SubMenu {
            label: label.into(),
            enabled: !names.is_empty(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |tray: &mut Self, i| {
                        if let Some(name) = names.get(i) {
                            tray.send(Command::SetDefault {
                                kind,
                                name: name.clone(),
                            });
                        }
                    }),
                    options,
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }
}

/// A window, detached so it outlives nothing it should not. It raises the open one
/// instead when there is one, and is a client of this service even when the service
/// runs a private config.
fn open_window() {
    let Ok(exe) = crate::service::executable() else {
        return;
    };
    let spawned = Process::new(exe)
        .env_remove("CANALIS_CONFIG")
        .env(crate::intro::FROM_TRAY, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => warn!("cannot open a window: {e}"),
    }
}

impl Tray {
    /// The default input and whether it is muted (its own flag, as a desktop sets it).
    fn microphone(&self) -> Option<(canalis_pw::NodeId, bool)> {
        let name = self.graph.defaults.source.as_deref()?;
        let node = self.graph.node_by_name(name)?;
        Some((node.id, node.volume.as_ref().is_some_and(|v| v.mute)))
    }

    /// "Mute microphone": the default input's own mute, checked while muted.
    fn mute_mic(&self) -> MenuItem<Self> {
        let mic = self.microphone();
        CheckmarkItem {
            label: t("Mute microphone").into(),
            enabled: mic.is_some(),
            checked: mic.is_some_and(|(_, muted)| muted),
            activate: Box::new(move |tray: &mut Self| {
                if let Some((node, muted)) = mic {
                    tray.send(Command::SetMute { node, mute: !muted });
                }
            }),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "canalis".into()
    }

    fn title(&self) -> String {
        "Canalis".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        let running = self.graph.chains.len();
        let output = self
            .graph
            .defaults
            .sink
            .as_deref()
            .and_then(|name| self.graph.node_by_name(name))
            .map_or(t("none"), |n| n.label.as_str());
        let muted = self.microphone().is_some_and(|(_, muted)| muted);
        let args: &[(&str, &dyn std::fmt::Display)] = &[("n", &running), ("output", &output)];
        let description = match (running == 1, muted) {
            (true, false) => tf("{n} chain · output: {output}", args),
            (false, false) => tf("{n} chains · output: {output}", args),
            (true, true) => tf("{n} chain · output: {output} · microphone muted", args),
            (false, true) => tf("{n} chains · output: {output} · microphone muted", args),
        };
        ToolTip {
            title: "Canalis".into(),
            description,
            ..Default::default()
        }
    }

    /// A click toggles the window: closes the open one, else opens one.
    fn activate(&mut self, _x: i32, _y: i32) {
        let socket = self.socket.clone();
        std::thread::spawn(move || {
            if !remote::close_windows(&socket) {
                open_window();
            }
        });
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let scenes: Vec<MenuItem<Self>> = self
            .graph
            .scenes
            .iter()
            .map(|scene| {
                let scene = scene.clone();
                StandardItem {
                    label: scene.name.clone(),
                    activate: Box::new(move |tray: &mut Self| {
                        tray.send(Command::ApplyScene {
                            scene: scene.clone(),
                        });
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        let mut pads: Vec<MenuItem<Self>> = self
            .graph
            .soundboard
            .pads
            .iter()
            // A menu click cannot be held.
            .filter(|pad| pad.mode != canalis_pw::PadMode::Hold)
            .map(|pad| {
                let name = pad.name.clone();
                let broken = self
                    .graph
                    .pads
                    .iter()
                    .any(|s| s.name == pad.name && s.error.is_some());
                StandardItem {
                    label: pad.name.clone(),
                    enabled: !broken,
                    activate: Box::new(move |tray: &mut Self| {
                        tray.send(Command::PressPad { name: name.clone() });
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        if !self.graph.soundboard.enabled {
            pads.clear();
        }
        if !pads.is_empty() {
            pads.push(MenuItem::Separator);
            pads.push(
                StandardItem {
                    label: t("Stop all").into(),
                    enabled: self.graph.pads.iter().any(|p| p.playing.is_some()),
                    activate: Box::new(|tray: &mut Self| tray.send(Command::StopPads)),
                    ..Default::default()
                }
                .into(),
            );
        }
        let chains: Vec<String> = self.graph.chains.iter().map(|c| c.name.clone()).collect();
        let all_muted = !chains.is_empty() && self.graph.chains.iter().all(|c| c.mute);
        vec![
            StandardItem {
                label: t("Open Canalis").into(),
                activate: Box::new(|_: &mut Self| open_window()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            self.device_picker(t("Output"), MediaClass::Sink, VirtualKind::Sink),
            self.device_picker(t("Input"), MediaClass::Source, VirtualKind::Source),
            SubMenu {
                label: t("Scenes").into(),
                enabled: !scenes.is_empty(),
                submenu: scenes,
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: t("Soundboard").into(),
                enabled: !pads.is_empty(),
                submenu: pads,
                ..Default::default()
            }
            .into(),
            self.mute_mic(),
            CheckmarkItem {
                label: t("Mute all chains").into(),
                enabled: !chains.is_empty(),
                checked: all_muted,
                activate: Box::new(move |tray: &mut Self| {
                    for name in &chains {
                        tray.send(Command::SetChainMute {
                            name: name.clone(),
                            mute: !all_muted,
                        });
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: t("Quit Canalis (stops every chain)").into(),
                activate: Box::new(|tray: &mut Self| {
                    info!("Quit from the tray");
                    tray.stop.store(true, Ordering::Relaxed);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// `assets/canalis-icon.svg`, square, ARGB32 big-endian.
fn icon(size: u32) -> Option<Icon> {
    let svg = include_bytes!("../../../assets/canalis-icon.svg");
    let tree = resvg::usvg::Tree::from_data(svg, &Default::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let view = tree.size();
    let scale = size as f32 / view.width().max(view.height());
    let dy = (size as f32 - view.height() * scale) / 2.0;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(0.0, dy),
        &mut pixmap.as_mut(),
    );
    let data = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.alpha(), p.red(), p.green(), p.blue()]
        })
        .collect();
    Some(Icon {
        width: size as i32,
        height: size as i32,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_renders_at_both_sizes() {
        for size in ICON_SIZES {
            let icon = icon(size).expect("renders");
            assert_eq!(icon.data.len(), (size * size * 4) as usize);
            assert!(icon.data.chunks(4).any(|p| p[0] > 0), "not blank");
        }
    }
}
