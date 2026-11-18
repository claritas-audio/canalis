//! Desktop notifications from the service (org.freedesktop.Notifications): a chain that
//! crashed or was stopped, a chain clipping, a device a running chain used going away.
//! The sender is a client of the service's own socket, as the tray is; `Watch` decides.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use canalis_pw::remote::Remote;
use canalis_pw::{ChainStatus, Command, Graph, MediaClass, Notifications};
use log::{info, warn};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

use crate::i18n::{t, tf};
use crate::service::APP_ID;

const POLL: Duration = Duration::from_millis(50);
/// One notification per subject in this time, so a flapping device does not spam.
const QUIET: Duration = Duration::from_secs(60);
/// Clipping counts when the output hit full scale in this many polls within a second.
const CLIP_POLLS: usize = 6;
/// A clipping chain is told about again only after this long without a clip.
const CLIP_CLEAR: Duration = Duration::from_secs(10);
const RESTART_ACTION: &str = "restart";
/// A peak at or over this counts as full scale (−0.01 dBFS: a clamped signal reads 0.9999).
const FULL_SCALE: f32 = 0.999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Crash,
    Clipping,
    LostDevice,
}

impl Kind {
    fn on(self, settings: Notifications) -> bool {
        match self {
            Kind::Crash => settings.crashes,
            Kind::Clipping => settings.clipping,
            Kind::LostDevice => settings.lost_devices,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub kind: Kind,
    /// What the rate limit counts by.
    pub key: String,
    pub summary: String,
    pub body: String,
    /// The chain "Start normally" restarts.
    pub restart: Option<String>,
}

/// What the last graphs looked like, to tell what changed.
#[derive(Default)]
pub struct Watch {
    /// Chain name → restarting, and stopped for good.
    chains: HashMap<String, (bool, bool)>,
    /// Device `node.name` → label, from the last connected graph.
    devices: Option<HashMap<String, String>>,
    /// Chain name → polls at full scale in the last second.
    clips: HashMap<String, VecDeque<Instant>>,
    /// Chain name → its last clip, while it counts as clipping.
    clipping: HashMap<String, Instant>,
    sent: HashMap<String, Instant>,
}

impl Watch {
    /// The events in `graph` since the last call, rate-limited, whatever the settings.
    pub fn update(&mut self, graph: &Graph, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        self.crashes(graph, &mut events);
        self.lost_devices(graph, &mut events);
        self.clipping(graph, now, &mut events);
        events.retain(|e| match self.sent.get(&e.key) {
            Some(at) if now.duration_since(*at) < QUIET => false,
            _ => {
                self.sent.insert(e.key.clone(), now);
                true
            }
        });
        events
    }

    fn crashes(&mut self, graph: &Graph, events: &mut Vec<Event>) {
        for chain in &graph.chains {
            let given_up =
                chain.status == ChainStatus::Stopped && !chain.restarting && chain.error.is_some();
            let now = (chain.restarting, given_up);
            let Some((restarting, was_given_up)) = self.chains.insert(chain.name.clone(), now)
            else {
                continue;
            };
            let why = chain.error.clone().unwrap_or_default();
            if given_up && !was_given_up {
                events.push(Event {
                    kind: Kind::Crash,
                    key: format!("stopped {}", chain.name),
                    summary: tf("{chain} stopped", &[("chain", &chain.label)]),
                    body: why,
                    restart: Some(chain.name.clone()),
                });
            } else if chain.restarting && !restarting {
                events.push(Event {
                    kind: Kind::Crash,
                    key: format!("crash {}", chain.name),
                    summary: tf("{chain} crashed", &[("chain", &chain.label)]),
                    body: tf(
                        "{why}. It comes back with its slots bypassed.",
                        &[("why", &why)],
                    ),
                    restart: Some(chain.name.clone()),
                });
            }
        }
        self.chains
            .retain(|name, _| graph.chains.iter().any(|c| &c.name == name));
    }

    fn lost_devices(&mut self, graph: &Graph, events: &mut Vec<Event>) {
        if !graph.connected {
            // PipeWire went away: every device would look lost.
            self.devices = None;
            return;
        }
        let now: HashMap<String, String> = graph
            .nodes
            .iter()
            .filter(|n| matches!(n.class, MediaClass::Sink | MediaClass::Source))
            .map(|n| (n.name.clone(), n.label.clone()))
            .collect();
        if let Some(before) = self.devices.replace(now) {
            let now = self.devices.as_ref().expect("just set");
            let gone: BTreeSet<&String> = before.keys().filter(|n| !now.contains_key(*n)).collect();
            for device in gone {
                for chain in graph
                    .chains
                    .iter()
                    .filter(|c| matches!(c.status, ChainStatus::Running | ChainStatus::Degraded))
                {
                    let body = if chain.outputs.contains(device) {
                        tf("{chain} was playing into it.", &[("chain", &chain.label)])
                    } else if chain.inputs.contains(device) {
                        tf("{chain} was listening to it.", &[("chain", &chain.label)])
                    } else {
                        continue;
                    };
                    events.push(Event {
                        kind: Kind::LostDevice,
                        key: format!("lost {device}"),
                        summary: tf("{device} went away", &[("device", &before[device])]),
                        body,
                        restart: None,
                    });
                }
            }
        }
    }

    fn clipping(&mut self, graph: &Graph, now: Instant, events: &mut Vec<Event>) {
        for chain in &graph.chains {
            let peak = chain.meters.output().into_iter().fold(0.0f32, f32::max);
            let polls = self.clips.entry(chain.name.clone()).or_default();
            if peak >= FULL_SCALE {
                polls.push_back(now);
            }
            while polls
                .front()
                .is_some_and(|at| now.duration_since(*at) > Duration::from_secs(1))
            {
                polls.pop_front();
            }
            if let Some(last) = polls.back().copied() {
                if self.clipping.contains_key(&chain.name) {
                    self.clipping.insert(chain.name.clone(), last);
                } else if polls.len() >= CLIP_POLLS {
                    self.clipping.insert(chain.name.clone(), last);
                    events.push(Event {
                        kind: Kind::Clipping,
                        key: format!("clip {}", chain.name),
                        summary: tf("{chain} is clipping", &[("chain", &chain.label)]),
                        body: t(
                            "Its output keeps hitting full scale. Lower the gain of its last slot, or put a limiter at the end.",
                        )
                        .to_owned(),
                        restart: None,
                    });
                }
            }
        }
        self.clipping
            .retain(|_, last| now.duration_since(*last) < CLIP_CLEAR);
    }
}

pub fn spawn(socket: PathBuf, stop: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new()
        .name("notifications".into())
        .spawn(move || run(socket, stop));
    if let Err(e) = spawned {
        warn!("no thread for notifications: {e}");
    }
}

fn run(socket: PathBuf, stop: Arc<AtomicBool>) {
    let remote = match Remote::spawn(socket, false, || {}, || {}) {
        Ok(remote) => remote,
        Err(e) => {
            warn!("notifications off: {e}");
            return;
        }
    };
    let bus = match Connection::session() {
        Ok(bus) => bus,
        Err(e) => {
            info!("no session bus ({e}); notifications off");
            return;
        }
    };
    let restarts: Arc<Mutex<HashMap<u32, String>>> = Arc::default();
    let (commands, pending) = mpsc::channel();
    listen_for_actions(bus.clone(), Arc::clone(&restarts), commands);
    let mut watch = Watch::default();
    // Graphs come only when something changes; meters move under the last one.
    let mut graph = None;
    while !stop.load(Ordering::Relaxed) && !remote.stopped() {
        for command in pending.try_iter() {
            remote.send(command);
        }
        if let Some(newer) = remote.latest() {
            graph = Some(newer);
        }
        if let Some(graph) = &graph {
            for event in watch.update(graph, Instant::now()) {
                if !event.kind.on(graph.notifications) {
                    continue;
                }
                info!("notifying: {} · {}", event.summary, event.body);
                match notify(&bus, &event) {
                    Ok(id) => {
                        if let Some(chain) = event.restart {
                            restarts
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(id, chain);
                        }
                    }
                    Err(e) => warn!("notification not sent: {e}"),
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

fn proxy(bus: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(
        bus,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
}

fn notify(bus: &Connection, event: &Event) -> zbus::Result<u32> {
    let actions: Vec<&str> = match event.restart {
        Some(_) => vec![RESTART_ACTION, t("Start normally")],
        None => vec![],
    };
    let hints: HashMap<&str, Value<'_>> = [("desktop-entry", Value::from(APP_ID))]
        .into_iter()
        .collect();
    proxy(bus)?.call(
        "Notify",
        &(
            "Canalis",
            0u32,
            APP_ID,
            event.summary.as_str(),
            event.body.as_str(),
            actions,
            hints,
            -1i32,
        ),
    )
}

/// "Start normally" on a notification restarts its chain.
fn listen_for_actions(
    bus: Connection,
    restarts: Arc<Mutex<HashMap<u32, String>>>,
    commands: mpsc::Sender<Command>,
) {
    let spawned = std::thread::Builder::new()
        .name("notification-actions".into())
        .spawn(move || {
            let signals = match proxy(&bus).and_then(|p| p.receive_signal("ActionInvoked")) {
                Ok(signals) => signals,
                Err(e) => {
                    warn!("notification actions off: {e}");
                    return;
                }
            };
            for message in signals {
                let Ok((id, action)) = message.body().deserialize::<(u32, String)>() else {
                    continue;
                };
                let chain = restarts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                if let (RESTART_ACTION, Some(name)) = (action.as_str(), chain) {
                    info!("{name} started normally from a notification");
                    if commands.send(Command::RestartChain { name }).is_err() {
                        return;
                    }
                }
            }
        });
    if let Err(e) = spawned {
        warn!("no thread for notification actions: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canalis_pw::{ChainKind, ChainState, Node};

    fn chain(name: &str) -> ChainState {
        ChainState {
            name: name.into(),
            label: name.into(),
            kind: ChainKind::Output,
            slots: vec![],
            inputs: vec!["usb.mic".into()],
            outputs: vec!["bt.headphones".into()],
            status: ChainStatus::Running,
            pid: Some(7),
            latency_samples: 0,
            slot_latencies: vec![],
            restarts: 0,
            error: None,
            meters: Arc::default(),
            slot_infos: Default::default(),
            gui_open: Default::default(),
            crashed_slot: None,
            restarting: false,
            overloads: Default::default(),
            recording: None,
            load: None,
            auto_level: false,
            compare: None,
            mute: false,
            color: None,
            icon: None,
            collapsed: false,
            loudness_target: None,
            key: None,
            gains: Default::default(),
        }
    }

    fn device(id: u32, name: &'static str, class: &'static str) -> Node {
        Node::from_props(id, |key| match key {
            "node.name" => Some(name),
            "node.description" => Some("Headphones"),
            "media.class" => Some(class),
            _ => None,
        })
    }

    fn graph(chains: Vec<ChainState>, nodes: Vec<Node>) -> Graph {
        Graph {
            connected: true,
            chains,
            nodes,
            ..Graph::default()
        }
    }

    #[test]
    fn only_crashes_are_on_by_default() {
        let defaults = Notifications::default();
        assert!(Kind::Crash.on(defaults));
        assert!(!Kind::Clipping.on(defaults));
        assert!(!Kind::LostDevice.on(defaults));
    }

    #[test]
    fn a_crash_and_a_stop_are_told_once_each() {
        let mut watch = Watch::default();
        let at = Instant::now();
        assert!(
            watch
                .update(&graph(vec![chain("eq")], vec![]), at)
                .is_empty()
        );

        let mut died = chain("eq");
        died.status = ChainStatus::Stopped;
        died.restarting = true;
        died.error = Some("crashed in slot 2 after 3s".into());
        let events = watch.update(&graph(vec![died.clone()], vec![]), at);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Kind::Crash);
        assert_eq!(events[0].summary, "eq crashed");
        assert_eq!(events[0].restart.as_deref(), Some("eq"));
        assert!(
            events[0].body.starts_with("crashed in slot 2"),
            "{}",
            events[0].body
        );
        assert!(
            watch
                .update(&graph(vec![died.clone()], vec![]), at)
                .is_empty()
        );

        // Back bypassed, then a second crash within the quiet minute stays quiet.
        let mut back = died.clone();
        back.status = ChainStatus::Degraded;
        back.restarting = false;
        assert!(watch.update(&graph(vec![back], vec![]), at).is_empty());
        assert!(
            watch
                .update(&graph(vec![died.clone()], vec![]), at)
                .is_empty()
        );

        // Given up: news, whatever the crash before.
        died.restarting = false;
        died.error = Some("stopped after 3 quick crashes".into());
        let events = watch.update(&graph(vec![died], vec![]), at);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary, "eq stopped");
    }

    #[test]
    fn a_lost_end_is_told_but_not_a_pipewire_restart_or_a_flapping_device() {
        let mut watch = Watch::default();
        let at = Instant::now();
        let headphones = || device(5, "bt.headphones", "Audio/Sink");
        watch.update(&graph(vec![chain("eq")], vec![headphones()]), at);
        let events = watch.update(&graph(vec![chain("eq")], vec![]), at);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Kind::LostDevice);
        assert!(events[0].summary.starts_with("Headphones"));

        watch.update(&graph(vec![chain("eq")], vec![headphones()]), at);
        assert!(
            watch
                .update(&graph(vec![chain("eq")], vec![]), at)
                .is_empty()
        );
        let later = at + QUIET;
        watch.update(&graph(vec![chain("eq")], vec![headphones()]), later);
        assert_eq!(
            watch.update(&graph(vec![chain("eq")], vec![]), later).len(),
            1
        );

        let mut gone = graph(vec![chain("eq")], vec![]);
        gone.connected = false;
        watch.update(&graph(vec![chain("eq")], vec![headphones()]), later);
        assert!(watch.update(&gone, later + QUIET).is_empty());
    }

    #[test]
    fn clipping_is_told_when_it_lasts_and_again_only_after_a_clean_spell() {
        let mut watch = Watch::default();
        let eq = chain("eq");
        let g = graph(vec![eq.clone()], vec![]);
        let start = Instant::now();
        let poll = |watch: &mut Watch, peak: f32, at: Instant| {
            eq.meters.set([0.0; 2], [peak, 0.5]);
            watch.update(&g, at)
        };
        let mut told = 0;
        for i in 0..(CLIP_POLLS as u32 - 1) {
            told += poll(&mut watch, 1.2, start + POLL * i).len();
        }
        assert_eq!(told, 0, "a moment at full scale is not clipping");
        let events = poll(&mut watch, 1.2, start + POLL * CLIP_POLLS as u32);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Kind::Clipping);

        // Still clipping two minutes on: no second notification.
        let mut at = start;
        for i in (CLIP_POLLS as u32 + 1)..2400 {
            at = start + POLL * i;
            assert!(poll(&mut watch, 1.2, at).is_empty());
        }
        let clean = at + CLIP_CLEAR + Duration::from_secs(1);
        assert!(poll(&mut watch, 0.5, clean).is_empty());
        let mut told = 0;
        for i in 1..=CLIP_POLLS as u32 {
            told += poll(&mut watch, 1.2, clean + POLL * i).len();
        }
        assert_eq!(told, 1);
    }
}
