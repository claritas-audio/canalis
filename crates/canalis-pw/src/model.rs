//! The graph as the GUI sees it: a snapshot of PipeWire objects keyed by global id,
//! plus the chains the control plane supervises.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

/// `node.name` prefix of the virtual devices Canalis creates and owns.
pub const VIRTUAL_PREFIX: &str = "canalis.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PortId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Input,
    Output,
}

/// `media.class` reduced to what the Devices and Apps views need to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaClass {
    /// A playback device or a virtual one (`Audio/Sink`).
    Sink,
    /// A capture device or a virtual one (`Audio/Source`).
    Source,
    /// An application playing audio (`Stream/Output/Audio`).
    PlaybackStream,
    /// An application capturing audio (`Stream/Input/Audio`).
    CaptureStream,
    /// Anything else: filters, MIDI, video.
    Other,
}

/// `media.class` values and the class they map to; a value also matches as a
/// prefix followed by `/` (`Audio/Source/Virtual` is a source).
#[rustfmt::skip]
const MEDIA_CLASSES: &[(&str, MediaClass)] = &[
    ("Audio/Sink",          MediaClass::Sink),
    ("Audio/Source",        MediaClass::Source),
    ("Stream/Output/Audio", MediaClass::PlaybackStream),
    ("Stream/Input/Audio",  MediaClass::CaptureStream),
];

impl MediaClass {
    pub fn from_media_class(class: Option<&str>) -> Self {
        let Some(class) = class else {
            return MediaClass::Other;
        };
        MEDIA_CLASSES
            .iter()
            .find(|(prefix, _)| {
                class
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .map_or(MediaClass::Other, |(_, class)| *class)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// `node.name`, stable across sessions; ids are not.
    pub name: String,
    /// `node.description` or `node.nick` when set, else `name`.
    pub label: String,
    /// `node.nick`, the short form ("Speaker"); `label` when unset.
    pub short_label: String,
    pub class: MediaClass,
    /// `application.name` for streams.
    pub app: Option<String>,
    /// Belongs to a device (`device.id` set): ALSA, BlueZ; `false` for virtual nodes and streams.
    pub hardware: bool,
    /// `media.name` from the node info (streams: the title of what is playing).
    pub media_name: Option<String>,
    /// `target.object` metadata on this node: a `node.name` the stream was sent to.
    pub target: Option<String>,
    /// Current `Props` volume, once the node reported its params.
    pub volume: Option<Volume>,
}

impl Node {
    /// What `Config::hidden` lists a stream under: its app, else its node name.
    pub fn hide_key(&self) -> &str {
        self.app.as_deref().unwrap_or(&self.name)
    }

    /// Builds a node from its global properties; `get` looks a key up.
    pub fn from_props<'a>(id: u32, get: impl Fn(&str) -> Option<&'a str>) -> Self {
        let name = get("node.name").unwrap_or_default().to_owned();
        let label = get("node.description")
            .or_else(|| get("node.nick"))
            .unwrap_or(&name)
            .to_owned();
        let short_label = get("node.nick").unwrap_or(&label).to_owned();
        Node {
            id: NodeId(id),
            label,
            short_label,
            class: MediaClass::from_media_class(get("media.class")),
            app: get("application.name").map(str::to_owned),
            hardware: get("device.id").is_some(),
            media_name: None,
            target: None,
            volume: None,
            name,
        }
    }

    /// A virtual device Canalis created (by the `node.name` prefix).
    pub fn is_virtual_device(&self) -> bool {
        self.name.starts_with(VIRTUAL_PREFIX)
    }

    pub fn is_stream(&self) -> bool {
        matches!(
            self.class,
            MediaClass::PlaybackStream | MediaClass::CaptureStream
        )
    }
}

/// Per-channel linear gains and mute, as the node's `Props` report them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    pub channels: Vec<f32>,
    pub mute: bool,
}

impl Volume {
    /// The slider value, 0..=1: cube root of the mean linear gain, as wpctl shows it.
    pub fn cubic(&self) -> f32 {
        if self.channels.is_empty() {
            return 0.0;
        }
        let mean = self.channels.iter().sum::<f32>() / self.channels.len() as f32;
        mean.cbrt()
    }
}

/// Which side of apps a virtual device faces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VirtualKind {
    Sink,
    Source,
}

impl VirtualKind {
    pub fn media_class(self) -> &'static str {
        match self {
            VirtualKind::Sink => "Audio/Sink",
            VirtualKind::Source => "Audio/Source/Virtual",
        }
    }

    /// `node.name` for a user label: the prefix plus a lowercase slug.
    pub fn node_name(self, label: &str) -> String {
        virtual_node_name(label)
    }
}

/// `canalis.<slug>` for a user label.
pub fn virtual_node_name(label: &str) -> String {
    let mut slug = String::new();
    for c in label.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    format!("{VIRTUAL_PREFIX}{}", slug.trim_matches('-'))
}

/// The four chain types (ADR 0003); mirrors `canalis_chain::ChainKind` by its strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChainKind {
    Output,
    Mic,
    Cable,
    Insert,
}

impl ChainKind {
    pub const ALL: [ChainKind; 4] = [
        ChainKind::Output,
        ChainKind::Mic,
        ChainKind::Cable,
        ChainKind::Insert,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChainKind::Output => "output",
            ChainKind::Mic => "mic",
            ChainKind::Cable => "cable",
            ChainKind::Insert => "insert",
        }
    }

    /// The user picks what feeds the chain only when its capture end is a plain stream.
    pub fn picks_inputs(self) -> bool {
        matches!(self, ChainKind::Mic | ChainKind::Insert)
    }

    /// The user picks where the chain goes only when its playback end is a plain stream.
    pub fn picks_outputs(self) -> bool {
        matches!(self, ChainKind::Output | ChainKind::Insert)
    }

    /// `node.name` of the capture end, as `canalis-chain` names it.
    pub fn capture_node_name(self, name: &str) -> String {
        match self {
            ChainKind::Output | ChainKind::Cable => name.to_owned(),
            ChainKind::Mic | ChainKind::Insert => format!("{name}.in"),
        }
    }

    /// `node.name` of the playback end, as `canalis-chain` names it.
    pub fn playback_node_name(self, name: &str) -> String {
        format!("{name}.out")
    }

    /// `node.name` of the key input, present while the chain has a key.
    pub fn key_node_name(self, name: &str) -> String {
        format!("{name}.key")
    }

    /// `node.name` of the hardware insert's return, present while a slot is one.
    pub fn insert_return_node_name(self, name: &str) -> String {
        format!("{name}.insert.in")
    }

    /// A mic or cable chain also owns a null source, fed by its playback end; that node
    /// is what apps record from.
    pub fn source_device(self) -> bool {
        matches!(self, ChainKind::Mic | ChainKind::Cable)
    }

    /// `node.name` of the null source apps record from: the chain's own name for a mic,
    /// `<name>.source` for a cable, whose own name is already its sink. Two nodes with
    /// one name make every lookup by name (targets, links) pick either.
    pub fn source_node_name(self, name: &str) -> String {
        match self {
            ChainKind::Cable => format!("{name}.source"),
            _ => name.to_owned(),
        }
    }
}

/// A chain's output going to a WAV file (`canalis_chain::recorder`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub path: String,
    pub seconds: f32,
    /// Frames lost because the disk fell behind.
    pub dropped: u64,
    /// The file is complete; `error` says why it may not be.
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainStatus {
    /// Spawned, no `up` from the process yet.
    Starting,
    Running,
    /// Restarted after it died; every slot is bypassed until the user re-enables them.
    Degraded,
    /// Not running and not scheduled to.
    Stopped,
}

/// Latest peaks a chain process reported, written by its reader thread, read by the GUI.
#[derive(Debug)]
pub struct ChainMeters {
    input: [AtomicU32; 2],
    output: [AtomicU32; 2],
    loudness: [AtomicU32; 4],
    balance: AtomicU32,
    /// The chain's automatic level gain, dB, as f32 bits.
    auto_gain_db: AtomicU32,
    /// The latest spectrum, while the chain sends one; a lock held for a copy only.
    spectrum: std::sync::Mutex<Option<Vec<f32>>>,
}

impl Default for ChainMeters {
    /// Loudness starts as silence: 0.0 would read as full scale until the first report.
    fn default() -> Self {
        Self {
            input: Default::default(),
            output: Default::default(),
            loudness: std::array::from_fn(|_| AtomicU32::new(LOUDNESS_SILENCE.to_bits())),
            balance: AtomicU32::new(0),
            auto_gain_db: AtomicU32::new(0),
            spectrum: std::sync::Mutex::new(None),
        }
    }
}

/// What silence reads as in the loudness values.
pub const LOUDNESS_SILENCE: f32 = -200.0;

impl ChainMeters {
    pub fn set(&self, input: [f32; 2], output: [f32; 2]) {
        for (slot, value) in self.input.iter().zip(input) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
        for (slot, value) in self.output.iter().zip(output) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn input(&self) -> [f32; 2] {
        std::array::from_fn(|c| f32::from_bits(self.input[c].load(Ordering::Relaxed)))
    }

    pub fn output(&self) -> [f32; 2] {
        std::array::from_fn(|c| f32::from_bits(self.output[c].load(Ordering::Relaxed)))
    }

    pub fn set_balance(&self, balance: f32) {
        self.balance.store(balance.to_bits(), Ordering::Relaxed);
    }

    /// Where the output sits over 400 ms: −1 all left, 0 even, 1 all right.
    pub fn balance(&self) -> f32 {
        f32::from_bits(self.balance.load(Ordering::Relaxed))
    }

    pub fn set_loudness(&self, values: [f32; 4]) {
        for (slot, value) in self.loudness.iter().zip(values) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// Momentary, short-term, integrated LUFS and true peak dBTP; `LOUDNESS_SILENCE` when quiet.
    pub fn loudness(&self) -> [f32; 4] {
        std::array::from_fn(|i| f32::from_bits(self.loudness[i].load(Ordering::Relaxed)))
    }
    pub fn values(&self) -> MeterValues {
        MeterValues {
            input: self.input(),
            output: self.output(),
            loudness: self.loudness(),
            balance: self.balance(),
            auto_gain_db: self.auto_gain_db(),
            spectrum: self.spectrum(),
        }
    }

    pub fn set_values(&self, values: &MeterValues) {
        self.set(values.input, values.output);
        self.set_loudness(values.loudness);
        self.set_balance(values.balance);
        self.set_auto_gain_db(values.auto_gain_db);
        self.set_spectrum(values.spectrum.clone());
    }

    pub fn set_spectrum(&self, spectrum: Option<Vec<f32>>) {
        *self.spectrum.lock().unwrap_or_else(|e| e.into_inner()) = spectrum;
    }

    /// Input then output band levels (dBFS, low to high), while the chain analyzes.
    pub fn spectrum(&self) -> Option<Vec<f32>> {
        self.spectrum
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_auto_gain_db(&self, db: f32) {
        self.auto_gain_db.store(db.to_bits(), Ordering::Relaxed);
    }

    /// What the chain's automatic level adds now, in dB; 0 when it is off.
    pub fn auto_gain_db(&self) -> f32 {
        f32::from_bits(self.auto_gain_db.load(Ordering::Relaxed))
    }
}

/// `ChainMeters` read at one moment, as the service sends them to its windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeterValues {
    pub input: [f32; 2],
    pub output: [f32; 2],
    pub loudness: [f32; 4],
    pub balance: f32,
    #[serde(default)]
    pub auto_gain_db: f32,
    /// While the chain analyzes its spectrum: input band levels then output, dBFS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spectrum: Option<Vec<f32>>,
}

/// The loudness a chain aims at when it has no target of its own, in LUFS (streaming's).
pub const LOUDNESS_TARGET: f32 = -16.0;

/// One supervised chain as the GUI sees it. Slots are the chain's own JSON, untyped
/// here so the control plane never links the audio plane.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainState {
    pub name: String,
    pub label: String,
    pub kind: ChainKind,
    pub slots: Vec<toml::Value>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub status: ChainStatus,
    pub pid: Option<u32>,
    pub latency_samples: u32,
    /// Each top-level slot's share of `latency_samples`, as the chain reported it.
    #[serde(default)]
    pub slot_latencies: Vec<u32>,
    pub restarts: u32,
    pub error: Option<String>,
    /// Live in the control plane; a remote client fills its own from `meters` lines.
    #[serde(skip)]
    pub meters: Arc<ChainMeters>,
    /// By slot id (a top-level index, or `LANE_BASE + top * 100 + lane * 10 + sub` for a
    /// slot inside a group's lane): what the chain reported after building it.
    pub slot_infos: BTreeMap<usize, SlotInfo>,
    /// By slot id: whether its plugin window is open.
    pub gui_open: BTreeMap<usize, bool>,
    /// The slot the callback was in when the chain last crashed, until it is recreated.
    pub crashed_slot: Option<usize>,
    /// It died and comes back, bypassed, after a short wait.
    #[serde(default)]
    pub restarting: bool,
    /// By top-level slot: why the chain bypassed it on its own (it overran the cycle).
    #[serde(default)]
    pub overloads: BTreeMap<usize, String>,
    /// The output being recorded to a file, or the last recording, until the next one.
    #[serde(default)]
    pub recording: Option<Recording>,
    /// The chain steers its output toward its loudness target (`ChainConfig::auto_level`).
    #[serde(default)]
    pub auto_level: bool,
    /// Which version plays while an A/B comparison is under way.
    #[serde(default)]
    pub compare: Option<crate::config::AbSide>,
    /// Peak DSP load over the last half second, % of the audio cycle; `None` while
    /// nothing plays through the chain.
    #[serde(default)]
    pub load: Option<f32>,
    pub mute: bool,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub collapsed: bool,
    pub loudness_target: Option<f32>,
    /// Destination `node.name` → send gain in dB, as `ChainConfig::gains`.
    pub gains: BTreeMap<String, f32>,
    /// The sidechain source, as `ChainConfig::key`.
    pub key: Option<String>,
}

/// The `audio.channel` names of send pair `send` (1..) on a chain's playback end; pair 0
/// is FL/FR. Mirrors `SEND_POSITIONS` in `canalis-chain`.
pub fn send_channels(send: usize) -> Option<[&'static str; 2]> {
    const PAIRS: [[&str; 2]; 6] = [
        ["RL", "RR"],
        ["SL", "SR"],
        ["FLC", "FRC"],
        ["RLC", "RRC"],
        ["TFL", "TFR"],
        ["TRL", "TRR"],
    ];
    match send {
        0 => Some(["FL", "FR"]),
        n => PAIRS.get(n - 1).copied(),
    }
}

/// Slot ids at or above this address a slot inside a group's lane, as `canalis-chain`
/// numbers them.
pub const LANE_BASE: usize = 1000;

/// The slot id of slot `sub` in lane `lane` of the group at top-level index `top`.
pub fn lane_slot_id(top: usize, lane: usize, sub: usize) -> usize {
    LANE_BASE + top * 100 + lane * 10 + sub
}

/// The `audio.channel` names of the hardware insert's send pair on the playback end;
/// `INSERT_POSITIONS` in `canalis-chain`.
pub const INSERT_CHANNELS: [&str; 2] = ["TSL", "TSR"];

/// What a chain reports about a built slot (`canalis_chain::SlotInfo` by its JSON).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SlotInfo {
    pub name: String,
    #[serde(default)]
    pub params: Vec<ParamInfo>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub has_gui: bool,
    /// Factory presets the slot can recall, as the chain listed them.
    #[serde(default)]
    pub presets: Vec<PresetInfo>,
    /// The plugin has a sidechain input; the chain's key feeds it.
    #[serde(default)]
    pub sidechain: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetInfo {
    pub uri: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub module: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub value: f64,
    #[serde(default)]
    pub stepped: bool,
}

impl ChainState {
    /// The config entry this state came from, for cycle checks in the GUI.
    pub fn config(&self) -> crate::config::ChainConfig {
        crate::config::ChainConfig {
            name: self.name.clone(),
            label: self.label.clone(),
            kind: self.kind,
            slots: self.slots.clone(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            mute: self.mute,
            color: self.color.clone(),
            icon: self.icon.clone(),
            collapsed: self.collapsed,
            loudness_target: self.loudness_target,
            auto_level: self.auto_level,
            compare: None,
            key: self.key.clone(),
            gains: self.gains.clone(),
        }
    }
}

impl PartialEq for ChainState {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.label == other.label
            && self.kind == other.kind
            && self.slots == other.slots
            && self.inputs == other.inputs
            && self.outputs == other.outputs
            && self.status == other.status
            && self.pid == other.pid
            && self.latency_samples == other.latency_samples
            && self.restarts == other.restarts
            && self.error == other.error
            && self.gains == other.gains
            && self.icon == other.icon
            && self.key == other.key
            && Arc::ptr_eq(&self.meters, &other.meters)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Port {
    pub id: PortId,
    pub node: NodeId,
    pub name: String,
    pub direction: Direction,
    /// `audio.channel`, e.g. `FL`; `None` for MIDI or unlabelled ports.
    pub channel: Option<String>,
    /// `port.monitor`: a sink's copy of what it plays.
    pub monitor: bool,
    /// PipeWire's latency from where the sound comes in (capture side) up to this port.
    #[serde(default)]
    pub upstream: Option<Latency>,
    /// PipeWire's latency from this port to where the sound goes out (playback side).
    #[serde(default)]
    pub downstream: Option<Latency>,
}

/// A port's `Latency` param, its upper bound: whole quanta, samples and nanoseconds,
/// summed by PipeWire over the path in one direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Latency {
    pub quanta: f32,
    pub samples: u32,
    pub ns: u64,
}

impl Latency {
    pub fn in_samples(&self, clock: Clock) -> f64 {
        f64::from(self.quanta) * f64::from(clock.quantum)
            + f64::from(self.samples)
            + self.ns as f64 * f64::from(clock.rate) / 1e9
    }
}

/// The graph's rate and quantum as PipeWire's `settings` metadata has them
/// (`clock.force-*` when set, else `clock.rate` and `clock.quantum`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    pub rate: u32,
    pub quantum: u32,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            rate: 48_000,
            quantum: 1024,
        }
    }
}

impl Clock {
    pub fn ms(&self, samples: f64) -> f64 {
        samples * 1000.0 / f64::from(self.rate.max(1))
    }
}

/// The raw `clock.*` keys of the `settings` metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockSettings {
    rate: Option<u32>,
    quantum: Option<u32>,
    force_rate: Option<u32>,
    force_quantum: Option<u32>,
}

impl ClockSettings {
    /// Applies one `settings` property; `false` for other keys.
    pub fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        let slot = match key {
            "clock.rate" => &mut self.rate,
            "clock.quantum" => &mut self.quantum,
            "clock.force-rate" => &mut self.force_rate,
            "clock.force-quantum" => &mut self.force_quantum,
            _ => return false,
        };
        *slot = value.and_then(|v| v.trim().parse().ok()).filter(|v| *v > 0);
        true
    }

    pub fn clock(&self) -> Clock {
        let default = Clock::default();
        Clock {
            rate: self.force_rate.or(self.rate).unwrap_or(default.rate),
            quantum: self
                .force_quantum
                .or(self.quantum)
                .unwrap_or(default.quantum),
        }
    }
}

impl Port {
    /// Builds a port from its global properties; `None` when `node.id` is missing.
    pub fn from_props<'a>(id: u32, get: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
        let node = NodeId(get("node.id")?.parse().ok()?);
        let direction = match get("port.direction")? {
            "in" => Direction::Input,
            "out" => Direction::Output,
            _ => return None,
        };
        Some(Port {
            id: PortId(id),
            node,
            name: get("port.name").unwrap_or_default().to_owned(),
            direction,
            channel: get("audio.channel").map(str::to_owned),
            monitor: get("port.monitor") == Some("true"),
            upstream: None,
            downstream: None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: u32,
    pub output: PortId,
    pub input: PortId,
    /// Made by Canalis (`canalis.link`); such links linger past the app, so the
    /// reconciler removes the ones the config no longer asks for.
    pub ours: bool,
}

impl Link {
    /// Builds a link from its global properties; `None` when a port id is missing.
    pub fn from_props<'a>(id: u32, get: impl Fn(&str) -> Option<&'a str>) -> Option<Self> {
        Some(Link {
            id,
            output: PortId(get("link.output.port")?.parse().ok()?),
            input: PortId(get("link.input.port")?.parse().ok()?),
            ours: get("canalis.link") == Some("true"),
        })
    }
}

/// The default devices WirePlumber reports, by `node.name`: the ones in use, and the
/// ones chosen (`default.configured.*`), which stay set while their device is away.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defaults {
    pub sink: Option<String>,
    pub source: Option<String>,
    #[serde(default)]
    pub configured_sink: Option<String>,
    #[serde(default)]
    pub configured_source: Option<String>,
}

impl Defaults {
    /// Applies one `default` metadata property; `value` is SPA JSON like `{"name":"x"}`.
    /// Returns `false` for keys that are not an audio default.
    pub fn apply(&mut self, key: &str, value: Option<&str>) -> bool {
        let slot = match key {
            "default.audio.sink" => &mut self.sink,
            "default.audio.source" => &mut self.source,
            "default.configured.audio.sink" => &mut self.configured_sink,
            "default.configured.audio.source" => &mut self.configured_source,
            _ => return false,
        };
        *slot = value.and_then(name_in_json);
        true
    }
}

/// One consistent view of the registry, sorted by id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    /// `false` while the daemon is unreachable; the lists are then empty.
    pub connected: bool,
    pub nodes: Vec<Node>,
    pub ports: Vec<Port>,
    pub links: Vec<Link>,
    pub defaults: Defaults,
    pub chains: Vec<ChainState>,
    /// The CLAP catalogue, sorted by name; `scanning` while a scan runs.
    pub plugins: Vec<crate::config::PluginEntry>,
    pub scanning: bool,
    pub scenes: Vec<crate::config::Scene>,
    /// Device node names and app names the user hid, as `Config::hidden`.
    pub hidden: BTreeSet<String>,
    /// Per-app labels and colours, as `Config::apps`.
    pub apps: BTreeMap<String, crate::config::AppStyle>,
    /// The MIDI source linked into the control plane's input, as `Config::midi_input`.
    pub midi_input: Option<String>,
    /// Learned controller mappings, as `Config::controls`.
    pub controls: Vec<crate::config::ControlMap>,
    /// Plugins last picked, newest first, as `Config::recent_plugins`.
    pub recent_plugins: Vec<crate::config::PluginRef>,
    /// The control plane waits for the next MIDI message to map it to this.
    pub learning: Option<crate::config::ControlTarget>,
    /// Why the config file is not being saved, for the window to say.
    pub config_problem: Option<crate::config::ConfigProblem>,
    /// Why LV2 plugins are not available (lilv not installed), with what to install.
    pub lv2_off: Option<String>,
    /// The recording being played back (a mic check), until it ends.
    #[serde(default)]
    pub playing: Option<String>,
    /// The soundboard, as `Config::soundboard`.
    #[serde(default)]
    pub soundboard: crate::config::Soundboard,
    /// Each pad of `soundboard`, in its order, as the service has it.
    #[serde(default)]
    pub pads: Vec<PadState>,
    /// As `Config::device_rules`.
    #[serde(default)]
    pub device_rules: Vec<crate::config::DeviceRule>,
    /// As `Config::notifications`.
    #[serde(default)]
    pub notifications: crate::config::Notifications,
    #[serde(default)]
    pub clock: Clock,
}

impl Graph {
    pub fn device_rule(&self, device: &str) -> Option<&crate::config::DeviceRule> {
        self.device_rules.iter().find(|r| r.device == device)
    }
}

/// A pad as the service sees it, for the window to draw.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PadState {
    pub name: String,
    /// Seconds, once the file is decoded.
    pub length: Option<f32>,
    /// Why it cannot play (its file is gone or unreadable).
    pub error: Option<String>,
    /// Seconds it had been playing when this snapshot was taken.
    pub playing: Option<f32>,
}

impl Graph {
    pub fn chain(&self, name: &str) -> Option<&ChainState> {
        self.chains.iter().find(|c| c.name == name)
    }

    /// The app's label as the user set it, else its `application.name`.
    pub fn app_label<'a>(&'a self, app: &'a str) -> &'a str {
        self.apps
            .get(app)
            .and_then(|s| s.label.as_deref())
            .unwrap_or(app)
    }

    /// A device by node name or an app by name the user hid.
    pub fn is_hidden(&self, key: &str) -> bool {
        self.hidden.contains(key)
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn port(&self, id: PortId) -> Option<&Port> {
        self.ports.iter().find(|p| p.id == id)
    }

    pub fn ports_of(&self, node: NodeId) -> impl Iterator<Item = &Port> {
        self.ports.iter().filter(move |p| p.node == node)
    }

    pub fn node_by_name(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    pub fn link_between(&self, output: PortId, input: PortId) -> Option<&Link> {
        self.links
            .iter()
            .find(|l| l.output == output && l.input == input)
    }

    /// The nodes a node's output ports are linked into (deduplicated, in link order).
    pub fn downstream_of(&self, node: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for link in &self.links {
            let Some(from) = self.port(link.output) else {
                continue;
            };
            if from.node != node {
                continue;
            }
            if let Some(to) = self.port(link.input)
                && !out.contains(&to.node)
            {
                out.push(to.node);
            }
        }
        out
    }

    /// The nodes feeding a node's input ports (deduplicated, in link order).
    pub fn upstream_of(&self, node: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for link in &self.links {
            let Some(to) = self.port(link.input) else {
                continue;
            };
            if to.node != node {
                continue;
            }
            if let Some(from) = self.port(link.output)
                && !out.contains(&from.node)
            {
                out.push(from.node);
            }
        }
        out
    }
}

/// The `name` field of a SPA JSON object, quoted or bare.
fn name_in_json(json: &str) -> Option<String> {
    let after_key = json
        .find("\"name\"")
        .map(|i| &json[i + "\"name\"".len()..])?;
    let value = after_key
        .trim_start()
        .strip_prefix([':', '='])?
        .trim_start();
    if let Some(quoted) = value.strip_prefix('"') {
        return quoted.split('"').next().map(str::to_owned);
    }
    let bare: String = value
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, ',' | '}'))
        .collect();
    (!bare.is_empty()).then_some(bare)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn no_two_nodes_of_a_chain_share_a_name() {
        for kind in ChainKind::ALL {
            let mut names = vec![
                kind.capture_node_name("c"),
                kind.playback_node_name("c"),
                kind.key_node_name("c"),
                kind.insert_return_node_name("c"),
            ];
            if kind.source_device() {
                names.push(kind.source_node_name("c"));
            }
            let count = names.len();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), count, "{kind:?} reuses a node name");
        }
    }

    fn props<'a>(pairs: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<&'a str> {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |key| map.get(key).copied()
    }

    #[test]
    fn media_class_maps_sinks() {
        assert_eq!(
            MediaClass::from_media_class(Some("Audio/Sink")),
            MediaClass::Sink
        );
    }

    #[test]
    fn media_class_maps_sources_and_virtual_sources() {
        assert_eq!(
            MediaClass::from_media_class(Some("Audio/Source")),
            MediaClass::Source
        );
        assert_eq!(
            MediaClass::from_media_class(Some("Audio/Source/Virtual")),
            MediaClass::Source
        );
    }

    #[test]
    fn media_class_maps_playback_streams() {
        assert_eq!(
            MediaClass::from_media_class(Some("Stream/Output/Audio")),
            MediaClass::PlaybackStream
        );
    }

    #[test]
    fn media_class_maps_capture_streams() {
        assert_eq!(
            MediaClass::from_media_class(Some("Stream/Input/Audio")),
            MediaClass::CaptureStream
        );
    }

    #[test]
    fn media_class_leaves_the_rest_as_other() {
        for class in [
            Some("Audio/Duplex"),
            Some("Midi/Bridge"),
            Some("Video/Source"),
            None,
        ] {
            assert_eq!(MediaClass::from_media_class(class), MediaClass::Other);
        }
        assert_eq!(
            MediaClass::from_media_class(Some("Audio/Sinker")),
            MediaClass::Other
        );
    }

    #[test]
    fn node_label_prefers_description_then_nick_then_name() {
        let get = props(&[
            ("node.name", "alsa_output.x"),
            ("node.nick", "Nick"),
            ("node.description", "Built-in Audio"),
            ("media.class", "Audio/Sink"),
            ("device.id", "52"),
        ]);
        let node = Node::from_props(7, get);
        assert_eq!(node.label, "Built-in Audio");
        assert_eq!(node.short_label, "Nick");
        assert_eq!(node.name, "alsa_output.x");
        assert_eq!(node.class, MediaClass::Sink);
        assert_eq!(node.app, None);
        assert!(node.hardware);

        let node = Node::from_props(7, props(&[("node.name", "n"), ("node.nick", "Nick")]));
        assert_eq!(node.label, "Nick");

        let node = Node::from_props(7, props(&[("node.name", "n")]));
        assert_eq!(node.label, "n");
        assert_eq!(node.short_label, "n");
    }

    #[test]
    fn stream_nodes_carry_the_application_name() {
        let node = Node::from_props(
            9,
            props(&[
                ("node.name", "Firefox"),
                ("media.class", "Stream/Output/Audio"),
                ("application.name", "Firefox"),
            ]),
        );
        assert_eq!(node.class, MediaClass::PlaybackStream);
        assert_eq!(node.app.as_deref(), Some("Firefox"));
        assert!(!node.hardware);
    }

    #[test]
    fn port_reads_node_direction_and_channel() {
        let port = Port::from_props(
            12,
            props(&[
                ("node.id", "7"),
                ("port.name", "playback_FL"),
                ("port.direction", "in"),
                ("audio.channel", "FL"),
            ]),
        );
        assert_eq!(
            port,
            Some(Port {
                id: PortId(12),
                node: NodeId(7),
                name: "playback_FL".into(),
                direction: Direction::Input,
                channel: Some("FL".into()),
                monitor: false,
                upstream: None,
                downstream: None,
            })
        );
        let monitor = Port::from_props(
            13,
            props(&[
                ("node.id", "7"),
                ("port.name", "monitor_FL"),
                ("port.direction", "out"),
                ("port.monitor", "true"),
            ]),
        );
        assert!(monitor.is_some_and(|p| p.monitor));
    }

    #[test]
    fn port_without_a_node_or_direction_is_skipped() {
        assert_eq!(
            Port::from_props(1, props(&[("port.direction", "out")])),
            None
        );
        assert_eq!(Port::from_props(1, props(&[("node.id", "3")])), None);
        assert_eq!(
            Port::from_props(
                1,
                props(&[("node.id", "3"), ("port.direction", "sideways")])
            ),
            None
        );
    }

    #[test]
    fn link_reads_both_port_ids() {
        let link = Link::from_props(
            40,
            props(&[("link.output.port", "12"), ("link.input.port", "25")]),
        );
        assert_eq!(
            link,
            Some(Link {
                id: 40,
                output: PortId(12),
                input: PortId(25),
                ours: false,
            })
        );
        assert_eq!(
            Link::from_props(40, props(&[("link.output.port", "12")])),
            None
        );
    }

    #[test]
    fn chain_ends_are_named_like_the_chain_binary_names_them() {
        assert_eq!(
            ChainKind::Output.capture_node_name("canalis.eq"),
            "canalis.eq"
        );
        assert_eq!(
            ChainKind::Output.playback_node_name("canalis.eq"),
            "canalis.eq.out"
        );
        assert_eq!(
            ChainKind::Mic.capture_node_name("canalis.mic"),
            "canalis.mic.in"
        );
        assert_eq!(
            ChainKind::Mic.playback_node_name("canalis.mic"),
            "canalis.mic.out"
        );
        assert!(ChainKind::Insert.picks_inputs() && ChainKind::Insert.picks_outputs());
        assert!(!ChainKind::Cable.picks_inputs() && !ChainKind::Cable.picks_outputs());
    }

    #[test]
    fn virtual_node_names_are_prefixed_slugs() {
        assert_eq!(
            VirtualKind::Sink.node_name("Desktop EQ"),
            "canalis.desktop-eq"
        );
        assert_eq!(
            VirtualKind::Source.node_name("  Voice (calls) / OBS "),
            "canalis.voice-calls-obs"
        );
    }

    #[test]
    fn volume_slider_is_the_cube_root_of_the_mean_gain() {
        let v = Volume {
            channels: vec![0.125, 0.125],
            mute: false,
        };
        assert!((v.cubic() - 0.5).abs() < 1e-6);
        assert_eq!(
            Volume {
                channels: vec![],
                mute: false
            }
            .cubic(),
            0.0
        );
    }

    #[test]
    fn the_clock_prefers_forced_values_and_falls_back_to_48k_1024() {
        let mut settings = ClockSettings::default();
        assert_eq!(settings.clock(), Clock::default());
        assert!(settings.apply("clock.rate", Some("44100")));
        assert!(settings.apply("clock.quantum", Some("512")));
        assert!(settings.apply("clock.force-quantum", Some("128")));
        assert!(!settings.apply("clock.min-quantum", Some("32")));
        assert_eq!(
            settings.clock(),
            Clock {
                rate: 44_100,
                quantum: 128
            }
        );
        // 0 means "not forced".
        settings.apply("clock.force-quantum", Some("0"));
        assert_eq!(settings.clock().quantum, 512);
        assert_eq!(
            Clock {
                rate: 48_000,
                quantum: 1024
            }
            .ms(480.0),
            10.0
        );
    }

    #[test]
    fn defaults_follow_the_audio_keys_only() {
        let mut defaults = Defaults::default();
        assert!(defaults.apply("default.audio.sink", Some(r#"{ "name": "speakers" }"#)));
        assert!(defaults.apply("default.audio.source", Some(r#"{"name":"mic"}"#)));
        assert!(!defaults.apply("default.video.source", Some(r#"{"name":"cam"}"#)));
        assert!(defaults.apply(
            "default.configured.audio.sink",
            Some(r#"{"name":"headphones"}"#)
        ));
        assert_eq!(defaults.configured_sink.as_deref(), Some("headphones"));
        assert_eq!(defaults.sink.as_deref(), Some("speakers"));
        assert_eq!(defaults.source.as_deref(), Some("mic"));

        assert!(defaults.apply("default.audio.sink", None));
        assert_eq!(defaults.sink, None);
    }

    #[test]
    fn name_in_json_accepts_bare_spa_values() {
        assert_eq!(
            name_in_json("{ \"name\" = speakers }").as_deref(),
            Some("speakers")
        );
        assert_eq!(name_in_json("{ \"name\": }"), None);
        assert_eq!(name_in_json("{}"), None);
    }
}
