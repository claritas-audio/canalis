//! The soundboard engine, independent of Canalis: sound files decoded once into memory at
//! `RATE`, and an `Engine` that mixes pads into one stereo output in a real-time callback,
//! driven by a `Controller` on another thread through lock-free queues. It knows nothing
//! about PipeWire, chains or config files; the host owns the stream and the routing.

#![forbid(unsafe_code)]

mod decode;
mod engine;
mod resample;

pub use decode::{MAX_SECONDS, Sound, decode};
pub use engine::{Command, Controller, Engine, FADE_FRAMES, MAX_VOICES, Mode, Pad, pair};

/// The engine's sample rate; sounds are resampled to it when decoded and the host's stream
/// runs at it (PipeWire converts to the graph's rate if that differs).
pub const RATE: u32 = 48_000;
