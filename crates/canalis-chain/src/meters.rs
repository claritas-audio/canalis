//! Peak meters written by the audio thread, read and reset by whoever reports them.

use std::sync::atomic::{AtomicU32, Ordering};

pub const METER_CHANNELS: usize = 2;

/// Peaks since the last read, as f32 bits (non-negative floats order like their bits).
pub struct Meters {
    input: [AtomicU32; METER_CHANNELS],
    output: [AtomicU32; METER_CHANNELS],
    /// Momentary, short-term, integrated LUFS and true peak dBTP, as f32 bits.
    loudness: [AtomicU32; 4],
    /// `Loudness::balance`, as f32 bits.
    balance: AtomicU32,
    /// The automatic level's gain now, in dB, as f32 bits.
    auto_gain_db: AtomicU32,
}

impl Default for Meters {
    /// Loudness starts as silence: zero bits would report 0.0 LUFS before any audio.
    fn default() -> Self {
        Self {
            input: Default::default(),
            output: Default::default(),
            loudness: std::array::from_fn(|_| AtomicU32::new(crate::loudness::SILENCE.to_bits())),
            balance: AtomicU32::new(0),
            auto_gain_db: AtomicU32::new(0),
        }
    }
}

impl Meters {
    /// Real-time: one atomic max per channel.
    pub(crate) fn measure_input(&self, io: &[&mut [f32]]) {
        measure(&self.input, io);
    }

    pub(crate) fn measure_output(&self, io: &[&mut [f32]]) {
        measure(&self.output, io);
    }

    /// Peaks since the previous call, linear, then reset.
    pub fn take(&self) -> ([f32; METER_CHANNELS], [f32; METER_CHANNELS]) {
        (take(&self.input), take(&self.output))
    }

    /// Real-time: four stores.
    pub(crate) fn set_loudness(&self, values: [f32; 4]) {
        for (slot, value) in self.loudness.iter().zip(values) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    /// Real-time: one store.
    pub(crate) fn set_balance(&self, balance: f32) {
        self.balance.store(balance.to_bits(), Ordering::Relaxed);
    }

    pub fn balance(&self) -> f32 {
        f32::from_bits(self.balance.load(Ordering::Relaxed))
    }

    pub(crate) fn set_auto_gain_db(&self, db: f32) {
        self.auto_gain_db.store(db.to_bits(), Ordering::Relaxed);
    }

    pub fn auto_gain_db(&self) -> f32 {
        f32::from_bits(self.auto_gain_db.load(Ordering::Relaxed))
    }

    pub fn loudness(&self) -> [f32; 4] {
        std::array::from_fn(|i| f32::from_bits(self.loudness[i].load(Ordering::Relaxed)))
    }
}

fn measure(peaks: &[AtomicU32; METER_CHANNELS], io: &[&mut [f32]]) {
    for (peak, channel) in peaks.iter().zip(io) {
        let max = channel.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        peak.fetch_max(max.to_bits(), Ordering::Relaxed);
    }
}

fn take(peaks: &[AtomicU32; METER_CHANNELS]) -> [f32; METER_CHANNELS] {
    std::array::from_fn(|c| f32::from_bits(peaks[c].swap(0, Ordering::Relaxed)))
}
