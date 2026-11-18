//! The audio plane. A chain is a capture stream and a playback stream handled in one
//! process callback, the way PipeWire's own `module-filter-chain` does it. The side
//! that faces apps carries the virtual device's `media.class`, so one chain *is* the
//! virtual mic or output. Slots run in order inside the callback.
//!
//! Everything reachable from the process callback follows the real-time rules in
//! `CLAUDE.md`: no allocation, locks, syscalls, logging or panics. Buffers are sized
//! for the largest quantum PipeWire can ask for (8192) in `activate()`.

mod autolevel;
mod chain;
pub mod clap;
pub mod crash;
pub mod ipc;
mod kind;
pub mod lilv;
mod loudness;
pub mod lv2;
pub mod lv2state;
pub mod lv2ui;
mod meters;
pub mod overload;
pub mod recorder;
mod runner;
mod slot;
pub mod spectrum;
pub mod x11;

pub use chain::{Chain, INSERT_CHANNELS, Insert, MAX_SENDS, Message};
pub use clap::{ParamInfo, PluginDescriptor, PresetInfo, SlotInfo};
pub use kind::ChainKind;
pub use lv2::Lv2Descriptor;
pub use meters::{METER_CHANNELS, Meters};
pub use runner::{LaneRunner, MEASURE_NONE, SlotRunner};
pub use slot::{
    LANE_BASE, Lane, MAX_LANES, MAX_SLOTS, SlotConfig, SlotPath, SlotSpec, lane_slot_id,
    slot_config, slot_config_mut, slot_ids,
};

/// The largest `clock.quantum` PipeWire hands out with default settings.
pub const MAX_QUANTUM: usize = 8192;

/// Tests that load plugins take this: a chain process loads from one thread only, and
/// two bundles instantiating at once from two test threads has crashed the test binary.
#[cfg(test)]
pub(crate) static PLUGIN_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn plugin_test_guard() -> std::sync::MutexGuard<'static, ()> {
    PLUGIN_TESTS.lock().unwrap_or_else(|e| e.into_inner())
}
