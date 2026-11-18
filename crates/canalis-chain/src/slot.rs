//! What a slot holds, as the user configured it: serializable, sent over IPC and saved
//! in the config.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Slots per chain; the runner keeps a fixed array of this size.
pub const MAX_SLOTS: usize = 8;

/// Lanes a parallel group slot can hold.
pub const MAX_LANES: usize = 4;

/// Slot ids at or above this address a slot inside a group's lane:
/// `LANE_BASE + top * 100 + lane * 10 + sub`. Below it the id is the top-level index,
/// so every message keeps its one `slot` field.
pub const LANE_BASE: usize = 1000;

/// Where a slot id points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotPath {
    Top(usize),
    Lane { top: usize, lane: usize, sub: usize },
}

impl SlotPath {
    pub fn from_id(id: usize) -> Self {
        if id < LANE_BASE {
            SlotPath::Top(id)
        } else {
            let rest = id - LANE_BASE;
            SlotPath::Lane {
                top: rest / 100,
                lane: (rest % 100) / 10,
                sub: rest % 10,
            }
        }
    }

    pub fn id(self) -> usize {
        match self {
            SlotPath::Top(top) => top,
            SlotPath::Lane { top, lane, sub } => LANE_BASE + top * 100 + lane * 10 + sub,
        }
    }

    /// The top-level index the id lives under.
    pub fn top(self) -> usize {
        match self {
            SlotPath::Top(top) | SlotPath::Lane { top, .. } => top,
        }
    }
}

pub fn lane_slot_id(top: usize, lane: usize, sub: usize) -> usize {
    SlotPath::Lane { top, lane, sub }.id()
}

/// The slot an id points to, through a group's lanes when it is inside one.
pub fn slot_config(slots: &[SlotConfig], id: usize) -> Option<&SlotConfig> {
    match SlotPath::from_id(id) {
        SlotPath::Top(top) => slots.get(top),
        SlotPath::Lane { top, lane, sub } => match &slots.get(top)?.spec {
            SlotSpec::Group { lanes } => lanes.get(lane)?.slots.get(sub),
            _ => None,
        },
    }
}

pub fn slot_config_mut(slots: &mut [SlotConfig], id: usize) -> Option<&mut SlotConfig> {
    match SlotPath::from_id(id) {
        SlotPath::Top(top) => slots.get_mut(top),
        SlotPath::Lane { top, lane, sub } => match &mut slots.get_mut(top)?.spec {
            SlotSpec::Group { lanes } => lanes.get_mut(lane)?.slots.get_mut(sub),
            _ => None,
        },
    }
}

/// Every slot id in `slots`, lanes included, in processing order.
pub fn slot_ids(slots: &[SlotConfig]) -> Vec<usize> {
    let mut ids = Vec::new();
    for (top, slot) in slots.iter().enumerate() {
        ids.push(top);
        if let SlotSpec::Group { lanes } = &slot.spec {
            for (lane, l) in lanes.iter().enumerate() {
                ids.extend((0..l.slots.len()).map(|sub| lane_slot_id(top, lane, sub)));
            }
        }
    }
    ids
}

/// One lane of a parallel group: its own slots, summed back with a gain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub slots: Vec<SlotConfig>,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mute: bool,
    /// Balance, −1 (left) to 1 (right); 0 leaves both channels at unity.
    #[serde(default, skip_serializing_if = "is_centre")]
    pub pan: f32,
}

fn is_centre(pan: &f32) -> bool {
    *pan == 0.0
}

impl Lane {
    pub fn new(label: &str) -> Self {
        Lane {
            label: label.to_owned(),
            slots: Vec::new(),
            gain_db: 0.0,
            mute: false,
            pan: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlotConfig {
    pub spec: SlotSpec,
    #[serde(default)]
    pub bypass: bool,
    /// Dry/wet, 0..=1.
    #[serde(default = "full_wet")]
    pub mix: f32,
    /// The strip folds consecutive slots with the same folder name under one header;
    /// nothing to the audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The user's name for the slot, shown instead of the plugin's; nothing to the audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Not loaded at all: the control plane sets it on a slot the chain crashed in, so a
    /// plugin that crashes while loading cannot crash the chain again. Never saved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

fn full_wet() -> f32 {
    1.0
}

impl SlotConfig {
    pub fn new(spec: SlotSpec) -> Self {
        SlotConfig {
            spec,
            bypass: false,
            mix: 1.0,
            folder: None,
            label: None,
            disabled: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SlotSpec {
    /// A plugin from a `.clap` bundle; an empty `id` means the bundle's first plugin.
    /// `params` holds the values the user set, by CLAP param id, as text keys for TOML.
    Clap {
        path: String,
        #[serde(default)]
        id: String,
        #[serde(default)]
        params: BTreeMap<String, f64>,
        /// The plugin's own state (CLAP state extension), base64; wins over `params`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
    },
    /// External gear: the signal leaves on the chain's insert pair to the `send` device
    /// and comes back from the `ret` device; `latency` is the measured round trip.
    Hardware {
        send: String,
        ret: String,
        #[serde(default)]
        latency: u32,
    },
    /// Parallel lanes, each a slot list on a copy of the input, summed back with a
    /// gain and mute each; lanes are delayed to the longest one's latency.
    Group {
        #[serde(default)]
        lanes: Vec<Lane>,
    },
    /// An LV2 plugin by URI; `params` holds control port values by port index.
    Lv2 {
        uri: String,
        #[serde(default)]
        params: BTreeMap<String, f64>,
        /// The plugin's own state (`lv2state::Blob`), base64; `params` go on top.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        /// A factory preset restored at load, before `state` and `params`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
    },
}

impl SlotSpec {
    /// What to call the slot before the chain reports the plugin's own name.
    pub fn name(&self) -> String {
        match self {
            SlotSpec::Clap { path, id, .. } if id.is_empty() => Path::new(path)
                .file_stem()
                .map_or_else(|| path.clone(), |s| s.to_string_lossy().into_owned()),
            SlotSpec::Clap { id, .. } => id.clone(),
            SlotSpec::Lv2 { uri, .. } => uri.rsplit('/').next().unwrap_or(uri).to_owned(),
            SlotSpec::Hardware { .. } => "Hardware insert".into(),
            SlotSpec::Group { lanes } => format!("Parallel · {} lanes", lanes.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_configs_round_trip_through_json() {
        let slot = SlotConfig::new(SlotSpec::Clap {
            path: String::new(),
            id: "audio.claritas.eq".into(),
            params: BTreeMap::from([("101".to_owned(), 0.5)]),
            state: None,
        });
        let text = serde_json::to_string(&slot).expect("serializes");
        assert!(text.contains("\"kind\":\"clap\""));
        assert_eq!(
            serde_json::from_str::<SlotConfig>(&text).expect("parses"),
            slot
        );
        assert_eq!(slot.mix, 1.0);
        assert!(
            serde_json::from_str::<SlotConfig>(r#"{"spec":{"kind":"comp"}}"#).is_err(),
            "the built-in kinds are gone; the config migration rewrites them"
        );
    }

    #[test]
    fn a_clap_slot_is_named_after_its_id_or_bundle() {
        let slot: SlotConfig = serde_json::from_str(
            r#"{"spec":{"kind":"clap","path":"/home/me/.clap/Claritas EQ.clap"}}"#,
        )
        .expect("parses");
        assert_eq!(slot.spec.name(), "Claritas EQ");
        let SlotSpec::Clap { id, params, .. } = &slot.spec else {
            panic!("not a clap slot");
        };
        assert!(id.is_empty() && params.is_empty());
        let named = SlotSpec::Clap {
            path: "x.clap".into(),
            id: "audio.claritas.eq".into(),
            params: BTreeMap::from([("7".to_owned(), 0.25)]),
            state: None,
        };
        assert_eq!(named.name(), "audio.claritas.eq");
        let text = serde_json::to_string(&named).expect("serializes");
        assert!(text.contains(r#""params":{"7":0.25}"#), "{text}");
    }
}
