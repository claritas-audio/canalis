//! The control-plane thread: owns the PipeWire main loop, reconnects when the daemon
//! goes away, publishes one `Graph` snapshot per burst of registry events and executes
//! the GUI's commands.

mod chains;
mod commands;
mod connection;
mod hold;
mod playback;
mod plugins;
mod reconcile;
mod rules;
mod soundboard;
mod triggers;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use log::{error, info, warn};
use pipewire::context::ContextRc;
use pipewire::core::PW_ID_CORE;
use pipewire::main_loop::MainLoopRc;
use pipewire::spa::utils::result::AsyncSeq;
use pipewire::{channel, keys};

use crate::config::{
    Config, ConfigProblem, ControlMap, ControlTarget, ControlTrigger, PluginEntry, Scene,
};
use crate::midi::MidiEvent;
use crate::mirror::Mirror;
use crate::model::{ChainKind, Graph, NodeId, PortId, VirtualKind};
use chains::{ChainEvent, Supervisor};
use connection::Connection;
pub use hold::{HOLD_TAIL, HoldTarget};

/// The retry timer ticks at this rate while the daemon is unreachable.
const RETRY_TICK: Duration = Duration::from_secs(1);
const MAX_RETRY_TICKS: u64 = 10;

/// What the GUI asks the control plane to do. Everything is by id of the current
/// snapshot except what gets persisted, which goes by name.
/// A step of an A/B comparison (`Command::Compare`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareStep {
    Start,
    Switch,
    Keep,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Link {
        output: PortId,
        input: PortId,
    },
    Unlink {
        link: u32,
    },
    CreateVirtual {
        kind: VirtualKind,
        label: String,
    },
    DeleteVirtual {
        name: String,
    },
    /// WirePlumber's configured default for the class the node belongs to.
    SetDefault {
        kind: VirtualKind,
        name: String,
    },
    /// `target.object` on a stream; `None` goes back to the default.
    SetTarget {
        stream: NodeId,
        target: Option<String>,
    },
    /// Slider value 0..=1; applied as the cube to every channel.
    SetVolume {
        node: NodeId,
        cubic: f32,
    },
    /// The device's own mute (its `Props`), as a desktop's volume control sets it.
    SetMute {
        node: NodeId,
        mute: bool,
    },
    /// A soundboard pad pressed: what it does depends on its mode (`PadMode`).
    PressPad {
        name: String,
    },
    /// A pad let go; only a Hold pad stops.
    ReleasePad {
        name: String,
    },
    StopPad {
        name: String,
    },
    StopPads,
    /// Learn each target in turn from the next MIDI messages (a bank of pads in order).
    LearnMany {
        targets: Vec<ControlTarget>,
    },
    /// A pad's new name, carried into the MIDI controls learned for it.
    RenamePad {
        name: String,
        to: String,
    },
    /// What happens when `device` connects; `None` (or an empty rule) removes its rule.
    SetDeviceRule {
        device: String,
        rule: Option<crate::config::DeviceRule>,
    },
    /// Which problems the service tells the desktop about.
    SetNotifications {
        notifications: crate::config::Notifications,
    },
    /// The whole board, as the window edited it.
    SetSoundboard {
        board: crate::config::Soundboard,
    },
    /// Unmute the target now and mute it again `HOLD_TAIL` after the last `Hold` for it:
    /// push-to-talk from a key that repeats while held.
    Hold {
        target: HoldTarget,
    },
    CreateChain {
        label: String,
        kind: ChainKind,
    },
    DeleteChain {
        name: String,
    },
    /// The whole slot list of a chain, as `canalis_chain::SlotConfig` tables.
    SetChainSlots {
        name: String,
        slots: Vec<toml::Value>,
    },
    /// One `canalis_chain::ipc::ToChain` object forwarded live (bypass, mix, eq_bands);
    /// the matching field of the saved slot is updated too.
    ChainMessage {
        name: String,
        message: toml::Value,
    },
    /// Where a chain's stream ends are linked, by node name. Refused if it closes a cycle.
    SetChainEndpoints {
        name: String,
        inputs: Vec<String>,
        outputs: Vec<String>,
    },
    /// The sidechain source of a chain, by node name, or `None` for no key input.
    SetChainKey {
        name: String,
        key: Option<String>,
    },
    /// The send gain of one destination of a chain, in dB; persisted.
    SetChainGain {
        name: String,
        target: String,
        gain_db: f32,
    },
    /// Fade the chain's output to silence or back; persisted.
    SetChainMute {
        name: String,
        mute: bool,
    },
    /// The user's name for a node (`None` goes back to PipeWire's); shown everywhere.
    SetLabel {
        node_name: String,
        label: Option<String>,
    },
    /// A chain's label; its process restarts so PipeWire shows the new description.
    RenameChain {
        name: String,
        label: String,
    },
    /// One of the GUI's icons for a chain, or `None` for none.
    SetChainIcon {
        name: String,
        icon: Option<String>,
    },
    /// Folds the chain's strip to its header, or opens it.
    SetChainCollapsed {
        name: String,
        collapsed: bool,
    },
    /// Start the chain afresh with every slot loaded, after a crash bypassed or stopped it.
    RestartChain {
        name: String,
    },
    /// An A/B comparison on a chain: start it (B is a copy of now), switch between the
    /// two, or keep the one playing; persisted.
    Compare {
        name: String,
        step: CompareStep,
    },
    /// The chain steers its output toward its loudness target, or stops; persisted.
    SetAutoLevel {
        name: String,
        on: bool,
    },
    /// The integrated loudness the strip aims at, or `None` for the default −16 LUFS.
    SetLoudnessTarget {
        name: String,
        target: Option<f32>,
    },
    /// The chains in this order; names not listed keep their place after the listed ones.
    ReorderChains {
        names: Vec<String>,
    },
    /// Keep a device (by `node.name`) or an app (by `application.name`) out of the views.
    SetHidden {
        key: String,
        hidden: bool,
    },
    /// The user's label and colour for an app; both `None` forgets the entry.
    SetAppStyle {
        app: String,
        label: Option<String>,
        color: Option<String>,
    },
    /// `#rrggbb` for a chain, or `None` for the kind's colour.
    SetChainColor {
        name: String,
        color: Option<String>,
    },
    /// Bring the routing to this state, touching only what differs.
    ApplyScene {
        scene: Scene,
    },
    /// Keep this state under its name, replacing a scene with the same name.
    SaveScene {
        scene: Scene,
    },
    /// Play a recording to the default output; `delete_after` for a mic check's take.
    PlayFile {
        path: std::path::PathBuf,
        delete_after: bool,
    },
    /// Stop what `PlayFile` started.
    StopPlayback,
    /// What recalls the scene by itself, or `None` for by hand only; persisted.
    SetSceneTrigger {
        name: String,
        when: Option<crate::config::SceneTrigger>,
    },
    DeleteScene {
        name: String,
    },
    /// Map the next MIDI message to `target`; `None` stops waiting.
    Learn {
        target: Option<ControlTarget>,
    },
    /// Drop the mapping at `index` in the config's list.
    ForgetControl {
        index: usize,
    },
    /// The MIDI source to link into `canalis.midi`, by node name.
    SetMidiInput {
        node: Option<String>,
    },
    /// Rebuild the CLAP catalogue; bundles unchanged since the last scan come from the cache.
    ScanPlugins,
    /// A plugin was picked in the plugin picker: first in its Recent group from now on.
    RememberPlugin {
        plugin: crate::config::PluginRef,
    },
    /// Save over a config file that has a `ConfigProblem`: a newer file is copied to
    /// `config.toml.v<version>` first, an unreadable one already has its copy.
    ReplaceConfig,
    Quit,
}

/// The GUI's end of the control plane. Dropping it stops the thread.
pub struct Handle {
    graphs: mpsc::Receiver<Arc<Graph>>,
    commands: channel::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// The newest snapshot published since the last call, if any.
    pub fn latest(&self) -> Option<Arc<Graph>> {
        self.graphs.try_iter().last()
    }

    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            warn!("control plane is gone; command dropped");
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the control-plane thread with the default config path; `wake` runs on that
/// thread after every snapshot.
pub fn spawn(wake: impl Fn() + Send + 'static) -> std::io::Result<Handle> {
    let config = Config::default_path().unwrap_or_else(|| {
        warn!("no config directory; using ./canalis.toml");
        PathBuf::from("canalis.toml")
    });
    let cache =
        plugins::default_cache_path().unwrap_or_else(|| config.with_file_name("plugins.toml"));
    spawn_with_paths(wake, config, cache)
}

/// `spawn` with an explicit config file (tests, private daemons); the plugin cache goes
/// next to it as `plugins.toml`.
pub fn spawn_with_config(
    wake: impl Fn() + Send + 'static,
    config_path: PathBuf,
) -> std::io::Result<Handle> {
    let cache = config_path.with_file_name("plugins.toml");
    spawn_with_paths(wake, config_path, cache)
}

fn spawn_with_paths(
    wake: impl Fn() + Send + 'static,
    config_path: PathBuf,
    plugins_path: PathBuf,
) -> std::io::Result<Handle> {
    let (graph_tx, graph_rx) = mpsc::channel();
    let (command_tx, command_rx) = channel::channel();
    let (event_tx, event_rx) = channel::channel();
    let publisher = Publisher {
        tx: graph_tx,
        wake: Box::new(wake),
    };
    let thread = thread::Builder::new()
        .name("canalis-pw".into())
        .spawn(move || {
            let paths = (config_path, plugins_path);
            if let Err(e) = run(publisher, command_rx, event_tx, event_rx, paths) {
                error!("control plane stopped: {e}");
            }
        })?;
    Ok(Handle {
        graphs: graph_rx,
        commands: command_tx,
        thread: Some(thread),
    })
}

struct Publisher {
    tx: mpsc::Sender<Arc<Graph>>,
    wake: Box<dyn Fn() + Send>,
}

impl Publisher {
    fn publish(&self, graph: Graph) {
        if self.tx.send(Arc::new(graph)).is_ok() {
            (self.wake)();
        }
    }
}

struct Session {
    context: ContextRc,
    publisher: Publisher,
    mirror: Mirror,
    config: Config,
    config_path: PathBuf,
    /// Set when saving would destroy the file on disk; nothing is saved until cleared.
    config_problem: Option<ConfigProblem>,
    /// The catalogue as last scanned, from and to `plugins_path`.
    plugins: Vec<PluginEntry>,
    plugins_path: PathBuf,
    /// Why the last scan found no LV2 support (lilv not installed).
    lv2_off: Option<String>,
    connection: Option<Connection>,
    /// A recording playing back to the default output.
    playback: Option<playback::Playback>,
    /// What scene triggers could match in the last snapshot; `None` until the first one
    /// after connecting, which only sets the baseline.
    present: Option<std::collections::BTreeSet<crate::config::SceneTrigger>>,
    /// The `sync` whose `done` publishes the next snapshot.
    pending: Option<AsyncSeq>,
    dirty: bool,
    /// Set by the core error callback; the connection is dropped at the next tick.
    lost: bool,
    ticks_to_retry: u64,
    retry_ticks: u64,
    /// Objects `reconcile` asked for and how often, so a failing one is not retried forever.
    attempts: HashMap<String, u8>,
    supervisor: Supervisor,
    events: channel::Sender<ChainEvent>,
    scanning: bool,
    /// The target the next MIDI message is learned for.
    learning: Option<ControlTarget>,
    /// Targets to learn after `learning`, in order (a whole bank of pads).
    learn_queue: std::collections::VecDeque<ControlTarget>,
    /// What the sequence learned last, so a pad's second message is not the next pad's.
    last_learned: Option<ControlTrigger>,
    holds: hold::Holds,
    hold_wake: channel::Sender<()>,
    board: soundboard::Board,
    rules: rules::Rules,
}

impl Session {
    fn connect(session: &Rc<RefCell<Session>>) {
        let context = session.borrow().context.clone();
        let opened = Connection::open(&context, Rc::downgrade(session));
        let mut s = session.borrow_mut();
        match opened {
            Ok(connection) => {
                info!("connected to PipeWire");
                s.connection = Some(connection);
                s.retry_ticks = 1;
                s.attempts.clear();
                s.changed();
            }
            Err(e) => {
                warn!("PipeWire unreachable ({e}); retrying in {}s", s.retry_ticks);
                s.ticks_to_retry = s.retry_ticks;
                s.retry_ticks = (s.retry_ticks * 2).min(MAX_RETRY_TICKS);
                s.publisher.publish(Graph::default());
            }
        }
    }

    fn tick(session: &Rc<RefCell<Session>>) {
        let mut s = session.borrow_mut();
        let chains = s.config.chains.clone();
        if s.supervisor.tick(&chains) {
            s.changed();
        }
        if s.playback.as_ref().is_some_and(playback::Playback::done) {
            s.end_playback();
        }
        s.pads_report();
        if s.lost {
            s.lost = false;
            let stale = s.connection.take();
            s.mirror.clear();
            s.present = None;
            s.playback = None;
            s.board.disconnected();
            s.pending = None;
            s.dirty = false;
            s.publisher.publish(Graph::default());
            drop(s);
            drop(stale);
            Session::connect(session);
            return;
        }
        if s.connection.is_some() {
            return;
        }
        if s.ticks_to_retry > 0 {
            s.ticks_to_retry -= 1;
            return;
        }
        drop(s);
        Session::connect(session);
    }

    /// Marks the mirror changed and asks the server for a barrier if none is in flight.
    fn changed(&mut self) {
        self.dirty = true;
        if self.pending.is_some() {
            return;
        }
        let Some(connection) = &self.connection else {
            return;
        };
        match connection.core.sync(0) {
            Ok(seq) => self.pending = Some(seq),
            Err(e) => warn!("core sync failed: {e}"),
        }
    }

    fn done(&mut self, id: u32, seq: AsyncSeq) {
        if id != PW_ID_CORE || self.pending != Some(seq) {
            return;
        }
        self.pending = None;
        if self.dirty {
            self.dirty = false;
            let graph = self.snapshot();
            info!(
                "graph: {} nodes, {} ports, {} links, {} chains",
                graph.nodes.len(),
                graph.ports.len(),
                graph.links.len(),
                graph.chains.len()
            );
            let present = triggers::present(&graph);
            self.publisher.publish(graph);
            self.reconcile();
            self.recall_triggered(present);
        }
    }

    /// Plays `path` to the default output, replacing what was playing.
    fn play(&mut self, path: &std::path::Path, delete_after: bool) {
        self.end_playback();
        let Some(connection) = &self.connection else {
            warn!("not connected; cannot play {}", path.display());
            return;
        };
        match playback::play(&connection.core, path, delete_after) {
            Ok(playing) => {
                info!("playing {}", path.display());
                self.playback = Some(playing);
            }
            Err(e) => warn!("cannot play {}: {e}", path.display()),
        }
        self.changed();
    }

    /// Stops the playback (or forgets a finished one), deleting a mic check's take.
    fn end_playback(&mut self) {
        let Some(playing) = self.playback.take() else {
            return;
        };
        if playing.delete_after
            && let Err(e) = std::fs::remove_file(&playing.path)
        {
            warn!("cannot remove {}: {e}", playing.path.display());
        }
        self.changed();
    }

    /// Runs the device rules and recalls the scenes whose device or app has just appeared.
    fn recall_triggered(
        &mut self,
        present: std::collections::BTreeSet<crate::config::SceneTrigger>,
    ) {
        let Some(before) = self.present.replace(present) else {
            return;
        };
        let now = self.present.as_ref().expect("just set");
        let actions = self.rules.changed(
            &self.config.device_rules,
            &triggers::devices(&before),
            &triggers::devices(now),
            &self.mirror.defaults,
        );
        let scenes: Vec<Scene> = triggers::fired(&self.config.scenes, &before, now)
            .into_iter()
            .cloned()
            .collect();
        for scene in scenes {
            info!("scene {} recalled: {:?} appeared", scene.name, scene.when);
            self.apply_scene(&scene);
        }
        for action in actions {
            match action {
                rules::Action::Scene(name) => {
                    match self.config.scenes.iter().find(|s| s.name == name).cloned() {
                        Some(scene) => {
                            info!("scene {name} recalled by a device rule");
                            self.apply_scene(&scene);
                        }
                        None => warn!("a device rule names scene {name}, which is gone"),
                    }
                }
                rules::Action::Default { kind, name } => self.set_default(kind, name.as_deref()),
            }
        }
    }

    fn snapshot(&self) -> Graph {
        let mut graph = self.mirror.snapshot(true);
        for node in &mut graph.nodes {
            let app_label = node
                .app
                .as_ref()
                .and_then(|app| self.config.apps.get(app))
                .and_then(|style| style.label.as_ref());
            if let Some(label) = self.config.labels.get(&node.name).or(app_label) {
                node.label = label.clone();
                node.short_label = label.clone();
            }
        }
        graph.chains = self.supervisor.states(&self.config.chains);
        graph.plugins = self.plugins.clone();
        graph.config_problem = self.config_problem.clone();
        graph.lv2_off = self.lv2_off.clone();
        graph.playing = self.playback.as_ref().map(|p| p.path.display().to_string());
        graph.scanning = self.scanning;
        graph.scenes = self.config.scenes.clone();
        graph.hidden = self.config.hidden.clone();
        graph.apps = self.config.apps.clone();
        graph.midi_input = self.config.midi_input.clone();
        graph.controls = self.config.controls.clone();
        graph.recent_plugins = self.config.recent_plugins.clone();
        graph.learning = self.learning.clone();
        graph.pads = self.board.states(&self.config.soundboard);
        graph.soundboard = self.config.soundboard.clone();
        graph.device_rules = self.config.device_rules.clone();
        graph.notifications = self.config.notifications;
        graph
    }

    /// A message on the MIDI input: learned, or fired through the mappings.
    pub(super) fn on_midi(&mut self, event: MidiEvent) {
        let Some(port) = self.config.midi_input.clone() else {
            return;
        };
        if let Some(target) = self.learning.take() {
            // A pad's release (CC 0, a note on at velocity 0) is not a control to learn.
            let release = matches!(
                event,
                MidiEvent::Cc { value: 0, .. } | MidiEvent::NoteOn { velocity: 0, .. }
            );
            let trigger = ControlTrigger::from_event(&port, event)
                .filter(|t| !release && self.last_learned.as_ref() != Some(t));
            match trigger {
                Some(trigger) => {
                    info!("learned {:?} for {}", trigger, target.label());
                    if matches!(target, ControlTarget::Pad { .. }) {
                        self.config.controls.retain(|c| c.target != target);
                    }
                    self.last_learned = Some(trigger.clone());
                    self.config.controls.push(ControlMap { trigger, target });
                    self.learning = self.learn_queue.pop_front();
                    self.save_config();
                }
                None => self.learning = Some(target),
            }
            self.changed();
            return;
        }
        let commands: Vec<Command> = self
            .config
            .controls
            .iter()
            .filter(|m| m.trigger.port() == port)
            .filter_map(|m| m.fire(event, &self.config))
            .collect();
        for command in commands {
            self.execute(command);
        }
    }

    fn chain_event(&mut self, event: ChainEvent) {
        if let ChainEvent::Params { name, slot, params } = event {
            let spec = self
                .config
                .chains
                .iter_mut()
                .find(|c| c.name == name)
                .and_then(|c| commands::slot_table_mut(&mut c.slots, slot))
                .and_then(|s| s.get_mut("spec"))
                .and_then(|s| s.as_table_mut());
            if let Some(spec) = spec {
                spec.insert("params".into(), params);
                self.save_config();
                self.changed();
            }
            return;
        }
        if let ChainEvent::Measured {
            name,
            slot,
            samples,
        } = event
        {
            let spec = self
                .config
                .chains
                .iter_mut()
                .find(|c| c.name == name)
                .and_then(|c| commands::slot_table_mut(&mut c.slots, slot))
                .and_then(|s| s.get_mut("spec"))
                .and_then(|s| s.as_table_mut());
            match (spec, samples) {
                (Some(spec), Some(samples)) => {
                    info!("chain {name} slot {slot}: round trip {samples} samples");
                    spec.insert("latency".into(), toml::Value::Integer(i64::from(samples)));
                    self.save_config();
                    self.changed();
                }
                _ => warn!("chain {name} slot {slot}: the click never came back"),
            }
            return;
        }
        if let ChainEvent::State { name, slot, state } = event {
            let slot = self
                .config
                .chains
                .iter_mut()
                .find(|c| c.name == name)
                .and_then(|c| commands::slot_table_mut(&mut c.slots, slot));
            if let Some(slot) = slot {
                commands::store_state(slot, &state);
                self.save_config();
                self.changed();
            }
            return;
        }
        if let ChainEvent::PluginsScanned(found, lv2_off) = event {
            self.scanning = false;
            self.lv2_off = lv2_off;
            if found != self.plugins {
                self.plugins = found;
                plugins::save_cache(&self.plugins_path, &self.plugins);
            }
            info!("plugin catalogue: {} plugin(s)", self.plugins.len());
            self.changed();
            return;
        }
        if let ChainEvent::Overload { name, slot, .. } = &event {
            // The chain already bypassed it; the config says so too, so it stays bypassed.
            let table = self
                .config
                .chains
                .iter_mut()
                .find(|c| &c.name == name)
                .and_then(|c| commands::slot_table_mut(&mut c.slots, *slot));
            if let Some(table) = table {
                table.insert("bypass".into(), toml::Value::Boolean(true));
                self.save_config();
            }
        }
        let chains = self.config.chains.clone();
        if self.supervisor.on_event(event, &chains) {
            self.changed();
        }
    }

    /// Writes the config unless the file on disk must be protected (`config_problem`).
    fn save_config(&self) {
        if self.config_problem.is_none() {
            self.config.save(&self.config_path);
        }
    }

    /// `Command::ReplaceConfig`: from now on this session's config is the file's content.
    fn replace_config(&mut self) {
        if let Some(ConfigProblem::Newer { version }) = self.config_problem
            && let Err(e) = crate::config::back_up(&self.config_path, &format!("v{version}"))
        {
            warn!("not replacing {}: {e}", self.config_path.display());
            return;
        }
        self.config_problem = None;
        self.save_config();
        self.changed();
    }

    /// Scans the CLAP search path on a worker thread; the result arrives as a `ChainEvent`.
    fn scan_plugins(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let binary = self.supervisor.binary().to_path_buf();
        let cached = self.plugins.clone();
        let events = self.events.clone();
        let spawned = thread::Builder::new()
            .name("plugin-scan".into())
            .spawn(move || {
                let bundles = plugins::bundles(&plugins::search_dirs());
                let (found, lv2_off) = plugins::catalogue(&binary, &cached, &bundles);
                let _ = events.send(ChainEvent::PluginsScanned(found, lv2_off));
            });
        if spawned.is_err() {
            warn!("no thread for the plugin scan");
            self.scanning = false;
        }
    }

    fn error(&mut self, id: u32, res: i32, message: &str) {
        if id == PW_ID_CORE {
            warn!("PipeWire connection lost ({res}: {message})");
            self.lost = true;
        } else {
            warn!("PipeWire error on object {id} ({res}): {message}");
        }
    }
}

fn run(
    publisher: Publisher,
    commands: channel::Receiver<Command>,
    event_tx: channel::Sender<ChainEvent>,
    event_rx: channel::Receiver<ChainEvent>,
    (config_path, plugins_path): (PathBuf, PathBuf),
) -> Result<(), pipewire::Error> {
    pipewire::init();
    let main_loop = MainLoopRc::new(None)?;
    let context = ContextRc::new(&main_loop, None)?;
    let (config, config_problem) = Config::load(&config_path);
    let (hold_wake, hold_rx) = channel::channel();
    let (decoded_tx, decoded_rx) = channel::channel();
    let (pads_wake_tx, pads_wake_rx) = channel::channel();
    let session = Rc::new(RefCell::new(Session {
        context,
        publisher,
        mirror: Mirror::default(),
        config,
        config_path,
        config_problem,
        plugins: plugins::load_cache(&plugins_path),
        plugins_path,
        lv2_off: None,
        connection: None,
        playback: None,
        present: None,
        pending: None,
        dirty: false,
        lost: false,
        ticks_to_retry: 0,
        retry_ticks: 1,
        attempts: HashMap::new(),
        supervisor: Supervisor::new(event_tx.clone()),
        events: event_tx,
        scanning: false,
        learning: None,
        learn_queue: std::collections::VecDeque::new(),
        last_learned: None,
        holds: hold::Holds::default(),
        hold_wake,
        board: soundboard::Board::new(decoded_tx, pads_wake_tx),
        rules: rules::Rules::default(),
    }));
    session.borrow_mut().scan_plugins();
    session.borrow_mut().load_pads();

    let on_event = Rc::clone(&session);
    let _events = event_rx.attach(main_loop.loop_(), move |event| {
        on_event.borrow_mut().chain_event(event);
    });

    let holding = Rc::clone(&session);
    let _holds = hold_rx.attach(main_loop.loop_(), move |()| {
        holding.borrow_mut().close_holds();
    });

    let decoding = Rc::clone(&session);
    let _decoded = decoded_rx.attach(main_loop.loop_(), move |(path, sound)| {
        decoding.borrow_mut().pad_decoded(path, sound);
    });
    let reporting = Rc::clone(&session);
    let _pads_wake = pads_wake_rx.attach(main_loop.loop_(), move |()| {
        reporting.borrow_mut().pads_report();
    });

    let quit = main_loop.clone();
    let executing = Rc::clone(&session);
    let _commands = commands.attach(main_loop.loop_(), move |command| match command {
        Command::Quit => quit.quit(),
        command => executing.borrow_mut().execute(command),
    });

    let ticking = Rc::clone(&session);
    let timer = main_loop
        .loop_()
        .add_timer(move |_| Session::tick(&ticking));
    timer
        .update_timer(Some(RETRY_TICK), Some(RETRY_TICK))
        .into_result()?;

    Session::connect(&session);
    main_loop.run();

    let mut s = session.borrow_mut();
    let names: Vec<String> = s.supervisor.running().map(str::to_owned).collect();
    for name in names {
        s.supervisor.stop(&name);
    }
    s.connection = None;
    Ok(())
}

/// The client properties every connection announces.
fn client_properties() -> pipewire::properties::PropertiesBox {
    pipewire::properties::properties! {
        *keys::APP_NAME => "Canalis",
        // WirePlumber gives a Flatpak client only RX unless it is a Manager (a patchbay).
        *keys::MEDIA_CATEGORY => "Manager",
    }
}
