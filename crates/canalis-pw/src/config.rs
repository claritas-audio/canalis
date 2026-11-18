//! `~/.config/canalis/config.toml`: what the user asked for, by name only. Ids are never
//! stored; the session reapplies this whenever the named objects are back in the registry.
//! The compatibility rules are ADR 0005.

mod migrate;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::{fs, io};

use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::midi::MidiEvent;
use crate::model::{ChainKind, Graph, MediaClass, VirtualKind};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, rename = "virtual_device")]
    pub virtual_devices: Vec<VirtualDevice>,
    #[serde(default, rename = "link")]
    pub links: Vec<LinkByName>,
    /// `application.name` → `node.name` the app's streams are sent to.
    #[serde(default)]
    pub targets: BTreeMap<String, String>,
    #[serde(default, rename = "chain")]
    pub chains: Vec<ChainConfig>,
    /// `node.name` → the name the user gave it; PipeWire's own name is untouched.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Named routing states, recalled as a diff.
    #[serde(default, rename = "scene")]
    pub scenes: Vec<Scene>,
    /// `node.name`s of devices and `application.name`s of apps kept out of the views.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub hidden: BTreeSet<String>,
    /// `application.name` → how the app is shown; PipeWire's name is untouched.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, AppStyle>,
    /// `node.name` of the MIDI source linked into `canalis.midi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_input: Option<String>,
    /// What a controller moves, learned one at a time.
    #[serde(default, rename = "control", skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlMap>,
    /// Plugins last picked in the plugin picker, newest first.
    #[serde(
        default,
        rename = "recent_plugin",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub recent_plugins: Vec<PluginRef>,
    /// Pads of short sounds played into a chain's outputs.
    #[serde(default, skip_serializing_if = "Soundboard::is_empty")]
    pub soundboard: Soundboard,
    /// What happens when a device connects, and is undone when it goes.
    #[serde(default, rename = "device_rule", skip_serializing_if = "Vec::is_empty")]
    pub device_rules: Vec<DeviceRule>,
    /// Which problems the service tells the desktop about.
    #[serde(default, skip_serializing_if = "Notifications::is_default")]
    pub notifications: Notifications,
}

/// Desktop notifications the service sends, each on its own switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notifications {
    /// A chain crashed, or was stopped after crashing again and again.
    #[serde(default = "yes")]
    pub crashes: bool,
    /// A chain's output sat at full scale for a while.
    #[serde(default)]
    pub clipping: bool,
    /// A device a running chain plays into or listens to went away.
    #[serde(default)]
    pub lost_devices: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            crashes: true,
            clipping: false,
            lost_devices: false,
        }
    }
}

impl Notifications {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// When `device` (a `node.name`) appears: recall `scene`, then make `output` and `input`
/// (the device itself or a chain) WirePlumber's configured defaults. With `restore`, the
/// defaults it replaced come back when the device goes, unless they were changed since.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceRule {
    pub device: String,
    /// The device's name as shown when the rule was made, for while it is away.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
    #[serde(default = "yes")]
    pub restore: bool,
}

fn yes() -> bool {
    true
}

impl DeviceRule {
    pub fn new(device: &str) -> Self {
        Self {
            device: device.into(),
            label: String::new(),
            output: None,
            input: None,
            scene: None,
            restore: true,
        }
    }

    /// Does nothing when the device appears.
    pub fn is_empty(&self) -> bool {
        self.output.is_none() && self.input.is_none() && self.scene.is_none()
    }
}

/// The soundboard: pads mixed into one source node (`SOUNDBOARD_NODE`) that is linked to
/// `outputs` like any device, and to the default output when `monitor` is on. Chains can
/// take it as an input to put effects on the pads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Soundboard {
    /// The Pads tab and the source node exist.
    #[serde(default)]
    pub enabled: bool,
    /// `node.name`s the board plays into.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub monitor: bool,
    /// The whole board's level.
    #[serde(default)]
    pub level_db: f32,
    #[serde(default, rename = "pad", skip_serializing_if = "Vec::is_empty")]
    pub pads: Vec<Pad>,
}

impl Soundboard {
    pub fn is_empty(&self) -> bool {
        *self == Soundboard::default()
    }

    pub fn pad(&self, name: &str) -> Option<&Pad> {
        self.pads.iter().find(|p| p.name == name)
    }

    pub fn at(&self, slot: u32) -> Option<&Pad> {
        self.pads.iter().find(|p| p.slot == slot)
    }
}

/// One sound file, kept by path, at a place on the grid. `name` and `slot` are each
/// unique on the board.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pad {
    pub name: String,
    pub path: std::path::PathBuf,
    /// Bank × `PADS_PER_BANK` + place in the bank, row by row.
    #[serde(default)]
    pub slot: u32,
    #[serde(default)]
    pub gain_db: f32,
    /// `#rrggbb`; the theme's accent when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub mode: canalis_pads::Mode,
    #[serde(default)]
    pub looped: bool,
    /// Pads of the same group (1..) cut each other off; 0 is none.
    #[serde(default)]
    pub choke: u8,
}

/// Pads in one bank of the grid: 8 across, 4 down, as a Launchpad's top half.
pub const PADS_PER_BANK: u32 = 32;
pub const BANKS: u32 = 4;

/// How many plugins the picker's Recent group keeps.
pub const RECENT_PLUGINS: usize = 8;

impl Config {
    /// Puts `plugin` first among the recent ones, once, dropping the oldest past the cap.
    pub fn remember_plugin(&mut self, plugin: PluginRef) {
        self.recent_plugins.retain(|p| *p != plugin);
        self.recent_plugins.insert(0, plugin);
        self.recent_plugins.truncate(RECENT_PLUGINS);
    }
}

/// A catalogue plugin by what identifies it: format and CLAP id or LV2 URI. A bundle
/// moved or rebuilt keeps its place in Recent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRef {
    pub format: String,
    pub id: String,
}

impl PluginRef {
    pub fn of(entry: &PluginEntry) -> PluginRef {
        PluginRef {
            format: entry.format.clone(),
            id: entry.id.clone(),
        }
    }

    pub fn matches(&self, entry: &PluginEntry) -> bool {
        self.format == entry.format && self.id == entry.id
    }
}

/// A learned mapping: when `trigger` arrives on the MIDI input, `target` moves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlMap {
    pub trigger: ControlTrigger,
    pub target: ControlTarget,
}

/// A MIDI message on a named port; a mapping whose port is not the current MIDI input
/// stays listed and inert.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ControlTrigger {
    Cc { port: String, channel: u8, cc: u8 },
    Note { port: String, channel: u8, note: u8 },
}

impl ControlTrigger {
    pub fn port(&self) -> &str {
        match self {
            ControlTrigger::Cc { port, .. } | ControlTrigger::Note { port, .. } => port,
        }
    }

    /// The trigger a learn takes from `event`; a note off teaches nothing.
    pub fn from_event(port: &str, event: MidiEvent) -> Option<Self> {
        match event {
            MidiEvent::Cc { channel, cc, .. } => Some(ControlTrigger::Cc {
                port: port.to_owned(),
                channel,
                cc,
            }),
            MidiEvent::NoteOn { channel, note, .. } => Some(ControlTrigger::Note {
                port: port.to_owned(),
                channel,
                note,
            }),
            MidiEvent::NoteOff { .. } => None,
        }
    }

    /// Whether `event` is this trigger, and the value it carries: a CC's 0..=127, a
    /// note on's velocity, `None` for a note off.
    fn matches(&self, event: MidiEvent) -> Option<Option<u8>> {
        match (self, event) {
            (
                ControlTrigger::Cc { channel, cc, .. },
                MidiEvent::Cc {
                    channel: c,
                    cc: n,
                    value,
                },
            ) if *channel == c && *cc == n => Some(Some(value)),
            (
                ControlTrigger::Note { channel, note, .. },
                MidiEvent::NoteOn {
                    channel: c,
                    note: n,
                    velocity,
                },
            ) if *channel == c && *note == n => Some(Some(velocity)),
            (
                ControlTrigger::Note { channel, note, .. },
                MidiEvent::NoteOff {
                    channel: c,
                    note: n,
                },
            ) if *channel == c && *note == n => Some(None),
            _ => None,
        }
    }
}

/// What a control moves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ControlTarget {
    /// A chain's mute: a note toggles it, a CC above 63 mutes.
    Mute { chain: String },
    /// A slot's bypass (by slot id), the same way.
    Bypass { chain: String, slot: usize },
    /// A chain's send gain to `target`: a CC's 0..=127 spans −60..+12 dB.
    Gain { chain: String, target: String },
    /// Recall a scene on a note or a CC above 63.
    Scene { name: String },
    /// A soundboard pad: a note or a CC above 63 presses it, a note off or a CC at 63 or
    /// below releases it (which only a Hold pad hears).
    Pad { name: String },
    /// A plugin parameter (by slot id and the plugin's parameter id): a CC's 0..=127 spans
    /// `min..=max` (whole steps when `stepped`), a note flips between the ends. The range
    /// and name are the plugin's, kept from when it was learned.
    Param {
        chain: String,
        slot: usize,
        id: u32,
        name: String,
        min: f64,
        max: f64,
        #[serde(default)]
        stepped: bool,
    },
}

impl ControlTarget {
    pub fn label(&self) -> String {
        match self {
            ControlTarget::Mute { chain } => format!("mute {chain}"),
            ControlTarget::Bypass { chain, slot } => format!("bypass {chain} slot {}", slot + 1),
            ControlTarget::Gain { chain, target } => format!("{chain} → {target} level"),
            ControlTarget::Scene { name } => format!("scene {name}"),
            ControlTarget::Pad { name } => format!("pad {name}"),
            ControlTarget::Param {
                chain, slot, name, ..
            } => format!("{chain} slot {} · {name}", slot + 1),
        }
    }
}

impl ControlMap {
    /// The command `event` asks for, given the state in `config`; `None` when the event
    /// is not this trigger or changes nothing.
    pub fn fire(&self, event: MidiEvent, config: &Config) -> Option<crate::session::Command> {
        use crate::session::Command;
        let value = self.trigger.matches(event)?;
        let switch = |now: bool| -> Option<bool> {
            match (&self.trigger, value) {
                (ControlTrigger::Note { .. }, Some(_)) => Some(!now),
                (ControlTrigger::Note { .. }, None) => None,
                (ControlTrigger::Cc { .. }, Some(v)) => Some(v > 63).filter(|on| *on != now),
                (ControlTrigger::Cc { .. }, None) => None,
            }
        };
        match &self.target {
            ControlTarget::Mute { chain } => {
                let now = config.chains.iter().find(|c| c.name == *chain)?.mute;
                Some(Command::SetChainMute {
                    name: chain.clone(),
                    mute: switch(now)?,
                })
            }
            ControlTarget::Bypass { chain, slot } => {
                let chain_config = config.chains.iter().find(|c| c.name == *chain)?;
                let now = chain_config
                    .slots
                    .get(*slot)
                    .and_then(|s| s.get("bypass"))
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                let bypass = switch(now)?;
                Some(Command::ChainMessage {
                    name: chain.clone(),
                    message: toml::Value::Table(
                        [
                            ("type".to_owned(), toml::Value::String("bypass".into())),
                            ("slot".to_owned(), toml::Value::Integer(*slot as i64)),
                            ("bypass".to_owned(), toml::Value::Boolean(bypass)),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                })
            }
            ControlTarget::Gain { chain, target } => {
                let v = value?;
                Some(Command::SetChainGain {
                    name: chain.clone(),
                    target: target.clone(),
                    gain_db: GAIN_OFF_DB + (12.0 - GAIN_OFF_DB) * f32::from(v) / 127.0,
                })
            }
            ControlTarget::Scene { name } => {
                switch(false)?;
                let scene = config.scenes.iter().find(|s| s.name == *name)?;
                Some(Command::ApplyScene {
                    scene: scene.clone(),
                })
            }
            ControlTarget::Pad { name } => {
                config.soundboard.pad(name)?;
                let press = match (&self.trigger, value) {
                    (ControlTrigger::Note { .. }, on) => on.is_some(),
                    (ControlTrigger::Cc { .. }, v) => v? > 63,
                };
                let name = name.clone();
                Some(if press {
                    Command::PressPad { name }
                } else {
                    Command::ReleasePad { name }
                })
            }
            ControlTarget::Param {
                chain,
                slot,
                id,
                min,
                max,
                stepped,
                ..
            } => {
                let value = match (&self.trigger, value?) {
                    (ControlTrigger::Cc { .. }, v) => {
                        let value = min + (max - min) * f64::from(v) / 127.0;
                        if *stepped { value.round() } else { value }
                    }
                    (ControlTrigger::Note { .. }, _) => {
                        let now = config
                            .chains
                            .iter()
                            .find(|c| c.name == *chain)
                            .and_then(|c| c.slots.get(*slot))
                            .and_then(|s| s.get("spec")?.get("params")?.get(id.to_string()))
                            .and_then(toml::Value::as_float)
                            .unwrap_or(*min);
                        if now > (min + max) / 2.0 { *min } else { *max }
                    }
                };
                Some(Command::ChainMessage {
                    name: chain.clone(),
                    message: toml::Value::Table(
                        [
                            ("type".to_owned(), toml::Value::String("param".into())),
                            ("slot".to_owned(), toml::Value::Integer(*slot as i64)),
                            ("id".to_owned(), toml::Value::Integer(i64::from(*id))),
                            ("value".to_owned(), toml::Value::Float(value)),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                })
            }
        }
    }
}

/// The user's label and colour for an app, both optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `#rrggbb` for the avatar and graph box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// A whole routing state: defaults, app targets, chain ends, mutes and slot bypasses.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `application.name` → `node.name`, as `Config::targets`.
    #[serde(default)]
    pub targets: BTreeMap<String, String>,
    #[serde(default, rename = "chain")]
    pub chains: Vec<SceneChain>,
    /// Recall the scene by itself when this appears; `None` means only by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<SceneTrigger>,
}

/// An A/B comparison on a chain: `slots` is the version playing, `other` the one kept
/// aside, swapped by a switch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Compare {
    pub playing: AbSide,
    pub other: Vec<toml::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AbSide {
    A,
    B,
}

impl AbSide {
    pub fn other(self) -> AbSide {
        match self {
            AbSide::A => AbSide::B,
            AbSide::B => AbSide::A,
        }
    }
}

/// What recalls a scene by itself: a device (by `node.name`) or an app (by
/// `application.name`) appearing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SceneTrigger {
    Device { name: String },
    App { name: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneChain {
    pub name: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub mute: bool,
    /// One flag per slot, in order.
    #[serde(default)]
    pub bypass: Vec<bool>,
    /// Destination `node.name` → send gain in dB, as `ChainConfig::gains`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gains: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl Scene {
    /// The state the snapshot shows right now, under `name`.
    pub fn capture(name: &str, graph: &Graph) -> Scene {
        let mut targets = BTreeMap::new();
        for node in &graph.nodes {
            if let (true, Some(app), Some(target)) = (
                matches!(
                    node.class,
                    MediaClass::PlaybackStream | MediaClass::CaptureStream
                ),
                &node.app,
                &node.target,
            ) {
                targets.insert(app.clone(), target.clone());
            }
        }
        Scene {
            name: name.to_owned(),
            sink: graph.defaults.sink.clone(),
            source: graph.defaults.source.clone(),
            targets,
            chains: graph
                .chains
                .iter()
                .map(|c| SceneChain {
                    name: c.name.clone(),
                    inputs: c.inputs.clone(),
                    outputs: c.outputs.clone(),
                    mute: c.mute,
                    gains: c.gains.clone(),
                    key: c.key.clone(),
                    bypass: c
                        .slots
                        .iter()
                        .map(|s| s.get("bypass").and_then(|b| b.as_bool()) == Some(true))
                        .collect(),
                })
                .collect(),
            when: None,
        }
    }
}

/// One plugin of one `.clap` bundle, with the bundle's modification time at scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    pub path: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub modified: u64,
    /// `clap` (the default) or `lv2`; `id` is the CLAP id or the LV2 URI.
    #[serde(default = "clap_format")]
    pub format: String,
}

fn clap_format() -> String {
    "clap".to_owned()
}

impl PluginEntry {
    pub fn is_lv2(&self) -> bool {
        self.format == "lv2"
    }
}

/// One effect chain: a `canalis-chain` process plus where its stream ends are linked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChainConfig {
    pub name: String,
    pub label: String,
    pub kind: ChainKind,
    /// `canalis_chain::SlotConfig`s as the chain process reads them; opaque here.
    #[serde(default)]
    pub slots: Vec<toml::Value>,
    /// `node.name`s feeding the capture end (mic and insert chains).
    #[serde(default)]
    pub inputs: Vec<String>,
    /// `node.name`s the playback end feeds (output and insert chains).
    #[serde(default)]
    pub outputs: Vec<String>,
    /// The output is faded to silence; kept across restarts like a console's mute.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mute: bool,
    /// `#rrggbb` for the strip edge, tags and graph links; the kind's colour when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// One of the GUI's built-in icons, shown before the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The strip shows only its header; nothing to the audio.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
    /// The integrated loudness the strip aims at, in LUFS; −16 when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness_target: Option<f32>,
    /// The chain steers its output toward the loudness target by itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_level: bool,
    /// An A/B comparison under way: which version plays and the other's slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<Compare>,
    /// `node.name` of the sidechain the key input (`<name>.key`) hears; every slot with
    /// a sidechain port gets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Destination `node.name` → send gain in dB; unity when absent, silence below
    /// `GAIN_OFF_DB`. The virtual source's name keys a mic or cable chain's own send.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gains: BTreeMap<String, f32>,
}

/// Send gains at or below this are silence; the floor of the GUI's gain slider too.
pub const GAIN_OFF_DB: f32 = -60.0;

impl ChainConfig {
    /// One step of an A/B comparison. Start keeps a copy of the slots as B (A plays);
    /// Switch swaps the versions and returns the slots to load; Keep ends it on the one
    /// playing. Without a comparison, Switch and Keep change nothing.
    pub fn compare_step(&mut self, step: crate::session::CompareStep) -> Option<Vec<toml::Value>> {
        use crate::session::CompareStep;
        match (step, self.compare.take()) {
            (CompareStep::Start, _) => {
                self.compare = Some(Compare {
                    playing: AbSide::A,
                    other: self.slots.clone(),
                });
                None
            }
            (CompareStep::Switch, Some(compare)) => {
                self.compare = Some(Compare {
                    playing: compare.playing.other(),
                    other: self.slots.clone(),
                });
                Some(compare.other)
            }
            (CompareStep::Switch | CompareStep::Keep, _) => None,
        }
    }

    /// Node names this chain feeds, through its outputs list.
    pub fn feeds(&self) -> impl Iterator<Item = &str> {
        self.outputs.iter().map(String::as_str)
    }

    /// Everything the playback end feeds, in send order: the virtual source first for a
    /// mic or cable chain, then `outputs`. Send `i` leaves on the channel pair `i`.
    pub fn destinations(&self) -> Vec<String> {
        let own = self
            .kind
            .source_device()
            .then(|| self.kind.source_node_name(&self.name));
        own.into_iter()
            .chain(self.outputs.iter().cloned())
            .collect()
    }

    pub fn gain_db(&self, destination: &str) -> f32 {
        self.gains.get(destination).copied().unwrap_or(0.0)
    }

    /// The hardware insert's send and return devices, when a slot is one (the first).
    pub fn hardware_insert(&self) -> Option<(String, String)> {
        self.slots.iter().find_map(|slot| {
            let spec = slot.get("spec")?;
            if spec.get("kind").and_then(|k| k.as_str()) != Some("hardware") {
                return None;
            }
            Some((
                spec.get("send")?.as_str()?.to_owned(),
                spec.get("ret")?.as_str()?.to_owned(),
            ))
        })
    }
}

/// The chain that owns a node name, if it is one of a chain's two ends.
pub fn chain_of_node<'a>(chains: &'a [ChainConfig], node: &str) -> Option<&'a ChainConfig> {
    chains.iter().find(|c| {
        c.kind.capture_node_name(&c.name) == node
            || c.kind.playback_node_name(&c.name) == node
            || c.kind.key_node_name(&c.name) == node
            || c.kind.insert_return_node_name(&c.name) == node
            || (c.kind.source_device() && c.kind.source_node_name(&c.name) == node)
    })
}

/// A link from a chain's output back into the same chain, directly or through others:
/// a feedback loop at full gain.
pub fn feeds_itself(chains: &[ChainConfig], link: &LinkByName) -> bool {
    chain_of_node(chains, &link.output.node).is_some_and(|from| {
        chain_of_node(chains, &link.input.node).is_some_and(|to| to.name == from.name)
            || would_cycle(chains, from, &link.input.node)
    })
}

/// Whether sending `from`'s playback end into `to` (a node name) would let audio come back
/// to `from` through chains' configured inputs and outputs.
pub fn would_cycle(chains: &[ChainConfig], from: &ChainConfig, to: &str) -> bool {
    let Some(start) = chain_of_node(chains, to) else {
        return false;
    };
    let mut stack = vec![start.name.as_str()];
    let mut seen = vec![];
    while let Some(name) = stack.pop() {
        if name == from.name {
            return true;
        }
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        let Some(chain) = chains.iter().find(|c| c.name == name) else {
            continue;
        };
        for downstream in chain.feeds().filter_map(|n| chain_of_node(chains, n)) {
            stack.push(downstream.name.as_str());
        }
        for other in chains {
            let fed_by_chain = other
                .inputs
                .iter()
                .any(|i| chain_of_node(chains, i).is_some_and(|c| c.name == name));
            if fed_by_chain {
                stack.push(other.name.as_str());
            }
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VirtualDevice {
    pub name: String,
    pub label: String,
    pub kind: VirtualKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRef {
    pub node: String,
    pub port: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkByName {
    pub output: PortRef,
    pub input: PortRef,
}

/// Why the loaded config must not be written back: saving would destroy the user's file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ConfigProblem {
    /// The file did not parse; it stays as it is and a copy went to `backup`.
    Unreadable {
        error: String,
        backup: Option<PathBuf>,
    },
    /// A newer Canalis wrote it; read as far as this build understands.
    Newer { version: i64 },
}

impl Config {
    /// `~/.config/canalis/config.toml`, or `None` without a config directory.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("canalis").join("config.toml"))
    }

    /// The saved config brought up to `CONFIG_VERSION`; empty when the file is missing.
    /// With a problem the file is left alone and the caller must not save over it.
    pub fn load(path: &Path) -> (Config, Option<ConfigProblem>) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Config::default(), None),
            Err(e) => return (Config::default(), Some(unreadable(path, e.to_string()))),
        };
        match Config::parse(&text) {
            Ok((config, None)) => {
                info!("config loaded from {}", path.display());
                (config, None)
            }
            Ok((config, Some(version))) => {
                warn!(
                    "{} is format {version}, newer than {CONFIG_VERSION}; not saving over it",
                    path.display()
                );
                (config, Some(ConfigProblem::Newer { version }))
            }
            Err(e) => (Config::default(), Some(unreadable(path, e))),
        }
    }

    /// A config file of any version; older ones are migrated, a newer one comes back with
    /// its version and whatever this build can read of it.
    pub fn parse(text: &str) -> Result<(Config, Option<i64>), String> {
        let mut table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let newer = migrate::migrate(&mut table)?;
        match toml::Value::Table(table).try_into::<Config>() {
            Ok(config) => Ok((config, newer)),
            Err(_) if newer.is_some() => Ok((Config::default(), newer)),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Writes the file, `version` first, through a sibling temp file so a crash never
    /// leaves it half-written.
    pub fn save(&self, path: &Path) {
        if let Err(e) = self.write(path) {
            warn!("cannot save {}: {e}", path.display());
        }
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, &self.to_text()?)
    }

    pub fn to_text(&self) -> io::Result<String> {
        let body = toml::to_string_pretty(self).map_err(io::Error::other)?;
        Ok(format!("version = {CONFIG_VERSION}\n{body}"))
    }
}

/// The format this build writes; `migrate` brings every older file up to it.
pub const CONFIG_VERSION: i64 = migrate::VERSION;

fn unreadable(path: &Path, error: String) -> ConfigProblem {
    warn!(
        "cannot read {}: {error}; keeping it untouched",
        path.display()
    );
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let backup = back_up(path, &format!("broken-{stamp}"))
        .inspect_err(|e| warn!("no backup of {}: {e}", path.display()))
        .ok();
    ConfigProblem::Unreadable { error, backup }
}

/// Copies `path` to `<path>.<suffix>` beside it.
pub(crate) fn back_up(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    let backup = path.with_file_name(name);
    fs::copy(path, &backup)?;
    info!("copied {} to {}", path.display(), backup.display());
    Ok(backup)
}

pub(crate) fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_plugins_put_the_newest_first_once_and_keep_eight() {
        let plugin = |id: &str| PluginRef {
            format: "clap".into(),
            id: id.into(),
        };
        let mut config = Config::default();
        for i in 0..10 {
            config.remember_plugin(plugin(&i.to_string()));
        }
        config.remember_plugin(plugin("5"));
        let ids: Vec<&str> = config
            .recent_plugins
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(ids, ["5", "9", "8", "7", "6", "4", "3", "2"]);
        let text = toml::to_string(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            back.recent_plugins, config.recent_plugins,
            "survives a save"
        );
    }

    #[test]
    fn a_soundboard_round_trips_and_an_empty_one_is_not_written() {
        let mut config = Config::default();
        assert!(!config.to_text().unwrap().contains("soundboard"));
        config.soundboard = Soundboard {
            enabled: true,
            outputs: vec!["canalis.voice".into()],
            monitor: true,
            level_db: -6.0,
            pads: vec![Pad {
                name: "airhorn".into(),
                path: "/home/me/airhorn.mp3".into(),
                slot: 9,
                gain_db: -3.0,
                color: Some("#e04a4a".into()),
                mode: canalis_pads::Mode::Toggle,
                looped: true,
                choke: 2,
            }],
        };
        let text = config.to_text().unwrap();
        assert!(text.contains("[[soundboard.pad]]"), "{text}");
        let (back, _) = Config::parse(&text).unwrap();
        assert_eq!(back.soundboard, config.soundboard);
    }

    #[test]
    fn a_device_rule_round_trips_and_restores_unless_told_not_to() {
        let mut config = Config::default();
        assert!(!config.to_text().unwrap().contains("device_rule"));
        config.device_rules = vec![DeviceRule {
            output: Some("canalis.headphones-eq".into()),
            scene: Some("Music".into()),
            ..DeviceRule::new("bluez_output.00_11.1")
        }];
        let text = config.to_text().unwrap();
        assert!(text.contains("[[device_rule]]"), "{text}");
        let (back, _) = Config::parse(&text).unwrap();
        assert_eq!(back.device_rules, config.device_rules);

        let (old, _) =
            Config::parse("[[device_rule]]\ndevice = \"usb\"\ninput = \"usb\"\n").unwrap();
        assert!(old.device_rules[0].restore);
    }

    #[test]
    fn config_round_trips_through_toml() {
        let mut config = Config::default();
        config.virtual_devices.push(VirtualDevice {
            name: "canalis.desktop-eq".into(),
            label: "Desktop EQ".into(),
            kind: VirtualKind::Sink,
        });
        config.links.push(LinkByName {
            output: PortRef {
                node: "canalis.desktop-eq".into(),
                port: "monitor_FL".into(),
            },
            input: PortRef {
                node: "alsa_output.x".into(),
                port: "playback_FL".into(),
            },
        });
        config
            .targets
            .insert("Firefox".into(), "canalis.desktop-eq".into());
        config.hidden.insert("alsa_output.hdmi".into());
        config.apps.insert(
            "Firefox".into(),
            AppStyle {
                label: Some("Browser".into()),
                color: Some("#4fc3f7".into()),
            },
        );
        let mut eq = chain(
            "canalis.desktop-eq",
            ChainKind::Output,
            &[],
            &["alsa_output.x"],
        );
        eq.icon = Some("🎧".into());
        config.chains.push(eq);
        let text = toml::to_string_pretty(&config).expect("serializes");
        assert!(text.contains("[[virtual_device]]"));
        assert!(text.contains("kind = \"sink\""));
        assert!(text.contains("hidden = [\"alsa_output.hdmi\"]"));
        assert!(text.contains("[apps.Firefox]"));
        assert!(text.contains("icon = \"🎧\""));
        assert_eq!(toml::from_str::<Config>(&text).expect("parses"), config);
    }

    fn chain(name: &str, kind: ChainKind, inputs: &[&str], outputs: &[&str]) -> ChainConfig {
        ChainConfig {
            gains: BTreeMap::new(),
            icon: None,
            collapsed: false,
            loudness_target: None,
            auto_level: false,
            compare: None,
            key: None,
            name: name.into(),
            label: name.into(),
            kind,
            slots: vec![],
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            mute: false,
            color: None,
        }
    }

    #[test]
    fn a_link_from_a_chain_output_into_its_own_input_feeds_itself() {
        let test = chain("canalis.test", ChainKind::Output, &[], &["alsa_output.x"]);
        let chains = vec![test];
        let link = |out: &str, inp: &str| LinkByName {
            output: PortRef {
                node: out.into(),
                port: "output_FL".into(),
            },
            input: PortRef {
                node: inp.into(),
                port: "playback_FL".into(),
            },
        };
        assert!(feeds_itself(
            &chains,
            &link("canalis.test.out", "canalis.test")
        ));
        assert!(!feeds_itself(
            &chains,
            &link("canalis.test.out", "alsa_output.x")
        ));
        assert!(!feeds_itself(
            &chains,
            &link("Google Chrome", "canalis.test")
        ));
    }

    #[test]
    fn a_chain_feeding_itself_through_another_chain_is_a_cycle() {
        let eq = chain("canalis.eq", ChainKind::Output, &[], &["canalis.bus.in"]);
        let bus = chain("canalis.bus", ChainKind::Insert, &[], &["alsa_output.x"]);
        let chains = vec![eq.clone(), bus.clone()];
        assert!(!would_cycle(&chains, &eq, "alsa_output.x"));
        assert!(!would_cycle(&chains, &eq, "canalis.bus.in"));
        assert!(would_cycle(&chains, &bus, "canalis.eq"));
        let mic = chain("canalis.mic", ChainKind::Mic, &["canalis.bus.out"], &[]);
        let chains = vec![eq, bus.clone(), mic];
        assert!(would_cycle(&chains, &bus, "canalis.eq"));
    }

    #[test]
    fn a_b_keeps_the_other_version_and_swaps_on_switch() {
        use crate::session::CompareStep;
        let mut eq = chain("canalis.eq", ChainKind::Output, &[], &[]);
        let a: toml::Value =
            toml::from_str("bypass = false\n[spec]\nkind = \"clap\"\npath = \"a\"").unwrap();
        eq.slots.push(a.clone());
        assert_eq!(eq.compare_step(CompareStep::Start), None);
        let b: toml::Value =
            toml::from_str("bypass = true\n[spec]\nkind = \"clap\"\npath = \"a\"").unwrap();
        eq.slots = vec![b.clone()];
        assert_eq!(
            eq.compare_step(CompareStep::Switch),
            Some(vec![a.clone()]),
            "back to the copy"
        );
        eq.slots = vec![a.clone()];
        assert_eq!(eq.compare.as_ref().map(|c| c.playing), Some(AbSide::B));
        assert_eq!(
            eq.compare_step(CompareStep::Switch),
            Some(vec![b]),
            "the edit survived"
        );
        eq.compare_step(CompareStep::Keep);
        assert_eq!(eq.compare, None);
        assert_eq!(
            eq.compare_step(CompareStep::Switch),
            None,
            "nothing to switch to"
        );
    }

    #[test]
    fn a_learned_parameter_follows_a_cc_and_flips_on_a_note() {
        let mut config = Config::default();
        let mut eq = chain("canalis.eq", ChainKind::Output, &[], &["alsa_output.x"]);
        eq.slots.push(
            toml::from_str(
                "[spec]\nkind = \"clap\"\npath = \"\"\nid = \"x\"\n[spec.params]\n\"42\" = 1.0",
            )
            .unwrap(),
        );
        config.chains.push(eq);
        let target = ControlTarget::Param {
            chain: "canalis.eq".into(),
            slot: 0,
            id: 42,
            name: "Drive".into(),
            min: 0.0,
            max: 10.0,
            stepped: true,
        };
        let value_of = |command: Option<crate::session::Command>| match command {
            Some(crate::session::Command::ChainMessage { message, .. }) => {
                assert_eq!(message["type"].as_str(), Some("param"));
                assert_eq!(message["id"].as_integer(), Some(42));
                message["value"].as_float()
            }
            other => panic!("a param message, not {other:?}"),
        };
        let cc = ControlMap {
            trigger: ControlTrigger::Cc {
                port: "knobs".into(),
                channel: 0,
                cc: 21,
            },
            target: target.clone(),
        };
        let turn = |value| MidiEvent::Cc {
            channel: 0,
            cc: 21,
            value,
        };
        assert_eq!(value_of(cc.fire(turn(127), &config)), Some(10.0));
        assert_eq!(
            value_of(cc.fire(turn(70), &config)),
            Some(6.0),
            "stepped: 5.51 rounds"
        );
        let note = ControlMap {
            trigger: ControlTrigger::Note {
                port: "pads".into(),
                channel: 0,
                note: 36,
            },
            target,
        };
        let press = MidiEvent::NoteOn {
            channel: 0,
            note: 36,
            velocity: 90,
        };
        assert_eq!(
            value_of(note.fire(press, &config)),
            Some(10.0),
            "low end flips to max"
        );
        assert_eq!(
            ControlTarget::Param {
                chain: "canalis.eq".into(),
                slot: 0,
                id: 42,
                name: "Drive".into(),
                min: 0.0,
                max: 10.0,
                stepped: true,
            }
            .label(),
            "canalis.eq slot 1 · Drive"
        );
    }

    #[test]
    fn a_learned_pad_is_pressed_and_released_and_silent_once_removed() {
        let mut config = Config::default();
        config.soundboard.pads.push(Pad {
            name: "airhorn".into(),
            path: "/x.wav".into(),
            slot: 0,
            gain_db: 0.0,
            color: None,
            mode: canalis_pads::Mode::Hold,
            looped: false,
            choke: 0,
        });
        let map = ControlMap {
            trigger: ControlTrigger::Note {
                port: "pad".into(),
                channel: 9,
                note: 36,
            },
            target: ControlTarget::Pad {
                name: "airhorn".into(),
            },
        };
        let hit = MidiEvent::NoteOn {
            channel: 9,
            note: 36,
            velocity: 90,
        };
        assert_eq!(
            map.fire(hit, &config),
            Some(crate::session::Command::PressPad {
                name: "airhorn".into()
            })
        );
        let lift = MidiEvent::NoteOff {
            channel: 9,
            note: 36,
        };
        assert_eq!(
            map.fire(lift, &config),
            Some(crate::session::Command::ReleasePad {
                name: "airhorn".into()
            })
        );
        config.soundboard.pads.clear();
        assert_eq!(map.fire(hit, &config), None);
    }

    #[test]
    fn a_learned_control_fires_the_right_command() {
        let mut config = Config::default();
        let mut eq = chain("canalis.eq", ChainKind::Output, &[], &["alsa_output.x"]);
        eq.slots
            .push(toml::from_str("bypass = false\n[spec]\nkind = \"comp\"").unwrap());
        config.chains.push(eq);
        let note = ControlMap {
            trigger: ControlTrigger::Note {
                port: "pad".into(),
                channel: 0,
                note: 36,
            },
            target: ControlTarget::Mute {
                chain: "canalis.eq".into(),
            },
        };
        let on = MidiEvent::NoteOn {
            channel: 0,
            note: 36,
            velocity: 100,
        };
        assert_eq!(
            note.fire(on, &config),
            Some(crate::session::Command::SetChainMute {
                name: "canalis.eq".into(),
                mute: true
            })
        );
        assert_eq!(
            note.fire(
                MidiEvent::NoteOff {
                    channel: 0,
                    note: 36
                },
                &config
            ),
            None,
            "a note off toggles nothing"
        );
        let cc = ControlMap {
            trigger: ControlTrigger::Cc {
                port: "pad".into(),
                channel: 0,
                cc: 7,
            },
            target: ControlTarget::Gain {
                chain: "canalis.eq".into(),
                target: "alsa_output.x".into(),
            },
        };
        let Some(crate::session::Command::SetChainGain { gain_db, .. }) = cc.fire(
            MidiEvent::Cc {
                channel: 0,
                cc: 7,
                value: 127,
            },
            &config,
        ) else {
            panic!("a CC sets the gain");
        };
        assert!((gain_db - 12.0).abs() < 1e-4);
        assert_eq!(
            cc.fire(
                MidiEvent::Cc {
                    channel: 1,
                    cc: 7,
                    value: 127
                },
                &config
            ),
            None,
            "another channel is another control"
        );
        let bypass = ControlMap {
            trigger: ControlTrigger::Cc {
                port: "pad".into(),
                channel: 0,
                cc: 20,
            },
            target: ControlTarget::Bypass {
                chain: "canalis.eq".into(),
                slot: 0,
            },
        };
        let Some(crate::session::Command::ChainMessage { message, .. }) = bypass.fire(
            MidiEvent::Cc {
                channel: 0,
                cc: 20,
                value: 100,
            },
            &config,
        ) else {
            panic!("a CC above 63 bypasses");
        };
        assert_eq!(message["bypass"].as_bool(), Some(true));
        assert_eq!(
            bypass.fire(
                MidiEvent::Cc {
                    channel: 0,
                    cc: 20,
                    value: 10
                },
                &config
            ),
            None,
            "already not bypassed"
        );
    }

    #[test]
    fn a_hardware_slot_names_its_send_and_return() {
        let mut config = chain("canalis.rack", ChainKind::Insert, &[], &[]);
        assert_eq!(config.hardware_insert(), None);
        config
            .slots
            .push(toml::from_str("[spec]\nkind = \"comp\"").expect("a slot table"));
        config.slots.push(
            toml::from_str(
                "[spec]\nkind = \"hardware\"\nsend = \"alsa_output.x\"\nret = \"alsa_input.x\"\nlatency = 1024",
            )
            .expect("a slot table"),
        );
        assert_eq!(
            config.hardware_insert(),
            Some(("alsa_output.x".to_owned(), "alsa_input.x".to_owned()))
        );
    }

    #[test]
    fn chain_configs_persist_slots_as_tables() {
        let mut config = Config::default();
        let slot: toml::Value = toml::from_str(
            "[spec]\nkind = \"eq\"\n[[spec.bands]]\nshape = \"bell\"\nfreq_hz = 1000.0\n",
        )
        .expect("a slot table");
        config.chains.push(ChainConfig {
            gains: BTreeMap::new(),
            icon: None,
            collapsed: false,
            loudness_target: None,
            auto_level: false,
            compare: None,
            key: None,
            name: "canalis.desktop-eq".into(),
            label: "Desktop EQ".into(),
            kind: ChainKind::Output,
            slots: vec![slot],
            inputs: vec![],
            outputs: vec!["alsa_output.x".into()],
            mute: false,
            color: None,
        });
        let text = toml::to_string_pretty(&config).expect("serializes");
        assert!(text.contains("[[chain]]"));
        assert!(text.contains("kind = \"output\""));
        assert_eq!(toml::from_str::<Config>(&text).expect("parses"), config);
    }

    #[test]
    fn missing_sections_default_to_empty() {
        let config: Config = toml::from_str("").expect("parses");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn a_scene_captures_defaults_targets_and_chain_state() {
        use crate::model::{ChainMeters, ChainState, ChainStatus, Node, NodeId};
        let mut graph = Graph::default();
        graph.defaults.sink = Some("canalis.desktop-eq".into());
        graph.nodes.push(Node::from_props(7, |key| match key {
            "node.name" => Some("firefox.playback"),
            "media.class" => Some("Stream/Output/Audio"),
            "application.name" => Some("Firefox"),
            _ => None,
        }));
        graph.nodes[0].target = Some("canalis.obs-feed".into());
        graph.chains.push(ChainState {
            name: "canalis.desktop-eq".into(),
            label: "Desktop EQ".into(),
            kind: ChainKind::Output,
            slots: vec![toml::from_str("bypass = true\n[spec]\nkind = \"comp\"").unwrap()],
            inputs: vec![],
            outputs: vec!["alsa_output.x".into()],
            status: ChainStatus::Running,
            pid: None,
            latency_samples: 0,
            slot_latencies: vec![],
            restarts: 0,
            error: None,
            meters: std::sync::Arc::new(ChainMeters::default()),
            slot_infos: Default::default(),
            gui_open: Default::default(),
            crashed_slot: None,
            restarting: false,
            overloads: Default::default(),
            recording: None,
            load: None,
            auto_level: false,
            compare: None,
            mute: true,
            color: None,
            icon: None,
            collapsed: false,
            loudness_target: None,
            key: Some("alsa_input.mic".into()),
            gains: [("alsa_output.x".to_owned(), -6.0)].into_iter().collect(),
        });
        let _ = NodeId(7);
        let scene = Scene::capture("Calls", &graph);
        assert_eq!(scene.sink.as_deref(), Some("canalis.desktop-eq"));
        assert_eq!(
            scene.targets.get("Firefox").map(String::as_str),
            Some("canalis.obs-feed")
        );
        assert_eq!(scene.chains[0].bypass, vec![true]);
        assert!(scene.chains[0].mute);
        assert_eq!(scene.chains[0].gains.get("alsa_output.x"), Some(&-6.0));
        assert_eq!(scene.chains[0].key.as_deref(), Some("alsa_input.mic"));
        let text = toml::to_string_pretty(&scene).expect("serializes");
        let back: Scene = toml::from_str(&text).expect("parses");
        assert_eq!(back, scene);
    }
}
