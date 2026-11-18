//! PipeWire control plane. Owns the `pipewire` main loop on its own thread and
//! mirrors the registry (nodes, ports, links, metadata) into plain values the
//! GUI can read; commands flow back over a channel. No audio passes through here.

pub mod claritas;
mod config;
mod midi;
mod mirror;
mod model;
mod pod;
pub mod remote;
mod session;

pub use canalis_pads::Mode as PadMode;
pub use config::{
    AbSide, AppStyle, BANKS, CONFIG_VERSION, ChainConfig, Compare, Config, ConfigProblem,
    ControlMap, ControlTarget, ControlTrigger, DeviceRule, GAIN_OFF_DB, LinkByName, Notifications,
    PADS_PER_BANK, Pad, PluginEntry, PluginRef, PortRef, Scene, SceneChain, SceneTrigger,
    Soundboard, VirtualDevice, would_cycle,
};
pub use midi::MidiEvent;

/// `node.name` of the soundboard's source.
pub const SOUNDBOARD_NODE: &str = "canalis.soundboard";

/// `node.name` of the control plane's MIDI input.
pub const MIDI_INPUT_NAME: &str = "canalis.midi";
pub use model::{
    ChainKind, ChainMeters, ChainState, ChainStatus, Clock, Defaults, Direction, Graph,
    INSERT_CHANNELS, LANE_BASE, LOUDNESS_SILENCE, LOUDNESS_TARGET, Latency, Link, MediaClass,
    MeterValues, Node, NodeId, PadState, ParamInfo, Port, PortId, PresetInfo, SlotInfo,
    VIRTUAL_PREFIX, VirtualKind, Volume, lane_slot_id, send_channels, virtual_node_name,
};
pub use session::{Command, CompareStep, HOLD_TAIL, Handle, HoldTarget, spawn, spawn_with_config};
