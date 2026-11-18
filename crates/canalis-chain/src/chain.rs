//! The processing core: slots run in order over planar channel buffers, inside the
//! callback, with nothing allocated after `activate`. Slots arrive and leave through
//! `Message`s, built and freed off the audio thread.

use std::sync::Arc;

use std::sync::atomic::Ordering;

use crate::MAX_QUANTUM;
use crate::autolevel::AutoLevel;
use crate::crash::ACTIVE_SLOT;
use crate::loudness::Loudness;
use crate::meters::Meters;
use crate::overload::{RATE, Watchdog};
use crate::recorder::Tap;
use crate::runner::SlotRunner;
use crate::slot::{MAX_SLOTS, SlotPath};
use crate::spectrum::SpectrumTap;

/// What the control side sends into the callback. Every variant is built off-thread.
pub enum Message {
    /// A new runner for `index`, or `None` to empty the slot. The old runner comes back as garbage.
    Replace {
        index: usize,
        slot: Option<Box<SlotRunner>>,
    },
    Bypass {
        index: usize,
        bypass: bool,
    },
    Mix {
        index: usize,
        mix: f32,
    },
    Param {
        index: usize,
        id: u32,
        value: f64,
    },
    Mute {
        mute: bool,
    },
    /// Start (`Some`) or stop feeding the spectrum analyzer; dropping the tap frees
    /// nothing here, the analyzer still holds its rings.
    Spectrum(Option<SpectrumTap>),
    /// Steer the output toward this loudness (LUFS), or `None` to stop.
    AutoLevel {
        target: Option<f32>,
    },
    /// Start (`Some`) or stop recording the output. Dropping a tap frees nothing here:
    /// the recorder's writer still holds its ring.
    Tap(Option<Tap>),
    /// The stream's sample rate, for the loudness filters.
    Rate {
        rate: u32,
    },
    ResetLoudness,
    /// Linear gain per destination pair; the first is the main pair.
    Sends {
        gains: [f32; MAX_SENDS],
    },
    /// The hardware insert in this slot sends its click.
    Measure {
        index: usize,
    },
    /// A slot inside a group's lane; the old one comes back through the garbage ring.
    ReplaceLane {
        top: usize,
        lane: usize,
        sub: usize,
        slot: Option<Box<SlotRunner>>,
    },
    /// Linear gain of one lane of a group slot.
    LaneGain {
        index: usize,
        lane: usize,
        gain: f32,
    },
    LaneMute {
        index: usize,
        lane: usize,
        mute: bool,
    },
    LanePan {
        index: usize,
        lane: usize,
        pan: f32,
    },
}

/// Destinations a chain can feed at once; the playback end carries a stereo pair each,
/// and keeps one more pair for the hardware insert's send.
pub const MAX_SENDS: usize = 7;

/// Channels of the insert send and return: the chain's own width.
pub const INSERT_CHANNELS: usize = 2;

/// What the callback hands a hardware insert: where its send goes (planar, one
/// `MAX_QUANTUM` block per channel) and what came back from the gear.
pub struct Insert<'a> {
    pub send: &'a mut [f32],
    pub ret: &'a [&'a [f32]],
    /// Set by the slot that wrote `send` this cycle.
    pub written: bool,
}

/// Per-sample step of the mute ramp: about 10 ms at 48 kHz, click-free at any rate.
const MUTE_STEP: f32 = 1.0 / 480.0;

pub struct Chain {
    slots: [Option<Box<SlotRunner>>; MAX_SLOTS],
    meters: Arc<Meters>,
    /// The hardware insert's send, filled by its slot and copied out by the callback.
    insert_send: Vec<f32>,
    insert_written: bool,
    /// Where the output gain is heading: 0 muted, 1 open.
    gain_target: f32,
    gain: f32,
    loudness: Loudness,
    send_targets: [f32; MAX_SENDS],
    sends: [f32; MAX_SENDS],
    /// Each slot's time this cycle, in ns, for `watchdog`.
    slot_ns: [u64; MAX_SLOTS],
    watchdog: Watchdog,
    /// The output goes here too while it is being recorded.
    tap: Option<Tap>,
    auto: AutoLevel,
    /// Input and output go to the analyzer too while a window shows the spectrum.
    spectrum: Option<SpectrumTap>,
    /// The automatic level's gain applied last cycle, linear; ramped to the next.
    auto_gain: f32,
}

impl Chain {
    pub fn new(meters: Arc<Meters>) -> Self {
        Chain {
            slots: std::array::from_fn(|_| None),
            meters,
            insert_send: vec![0.0; INSERT_CHANNELS * MAX_QUANTUM],
            insert_written: false,
            gain_target: 1.0,
            gain: 1.0,
            loudness: Loudness::default(),
            send_targets: [1.0; MAX_SENDS],
            sends: [1.0; MAX_SENDS],
            slot_ns: [0; MAX_SLOTS],
            watchdog: Watchdog::default(),
            tap: None,
            auto: AutoLevel::default(),
            spectrum: None,
            auto_gain: 1.0,
        }
    }

    /// Applies one message; returns the runner it displaced, to be dropped off-thread.
    /// Real-time: moves only.
    pub fn apply(&mut self, message: Message) -> Option<Box<SlotRunner>> {
        match message {
            Message::Replace { index, slot } => {
                let old = self.slots.get_mut(index)?.take();
                if let Some(target) = self.slots.get_mut(index) {
                    *target = slot;
                }
                old
            }
            Message::ReplaceLane {
                top,
                lane,
                sub,
                slot,
            } => match self.slots.get_mut(top).and_then(Option::as_mut) {
                Some(group) => group.replace_lane_slot(lane, sub, slot),
                None => slot,
            },
            Message::LaneGain { index, lane, gain } => {
                if let Some(slot) = self.slots.get_mut(index).and_then(Option::as_mut) {
                    slot.set_lane_gain(lane, gain);
                }
                None
            }
            Message::LaneMute { index, lane, mute } => {
                if let Some(slot) = self.slots.get_mut(index).and_then(Option::as_mut) {
                    slot.set_lane_mute(lane, mute);
                }
                None
            }
            Message::LanePan { index, lane, pan } => {
                if let Some(slot) = self.slots.get_mut(index).and_then(Option::as_mut) {
                    slot.set_lane_pan(lane, pan);
                }
                None
            }
            Message::Bypass { index, bypass } => {
                if let Some(slot) = self.slot_mut(index) {
                    slot.bypass = bypass;
                }
                None
            }
            Message::Mix { index, mix } => {
                if let Some(slot) = self.slot_mut(index) {
                    slot.mix = mix.clamp(0.0, 1.0);
                }
                None
            }
            Message::Param { index, id, value } => {
                if let Some(slot) = self.slot_mut(index) {
                    slot.set_param(id, value);
                }
                None
            }
            Message::Mute { mute } => {
                self.gain_target = if mute { 0.0 } else { 1.0 };
                None
            }
            Message::Rate { rate } => {
                self.loudness.set_rate(rate);
                None
            }
            Message::Tap(tap) => {
                self.tap = tap;
                None
            }
            Message::Spectrum(tap) => {
                self.spectrum = tap;
                None
            }
            Message::AutoLevel { target } => {
                self.auto.set_target(target);
                None
            }
            Message::ResetLoudness => {
                self.loudness.reset();
                None
            }
            Message::Sends { gains } => {
                self.send_targets = gains;
                None
            }
            Message::Measure { index } => {
                if let Some(slot) = self.slot_mut(index) {
                    slot.start_measure();
                }
                None
            }
        }
    }

    /// The runner a slot id addresses, inside a group's lane when it says so. Real-time.
    fn slot_mut(&mut self, id: usize) -> Option<&mut SlotRunner> {
        match SlotPath::from_id(id) {
            SlotPath::Top(top) => self.slots.get_mut(top)?.as_deref_mut(),
            SlotPath::Lane { top, lane, sub } => self
                .slots
                .get_mut(top)?
                .as_deref_mut()?
                .lane_slot_mut(lane, sub),
        }
    }

    /// Runs every slot in place on `io`, one slice per channel, all of the same length
    /// (at most `MAX_QUANTUM` frames); `key` is the sidechain the keyed slots hear,
    /// `ret` what came back from the hardware insert, both empty when the chain has
    /// none. Real-time.
    pub fn process(&mut self, io: &mut [&mut [f32]], key: &[&[f32]], ret: &[&[f32]]) {
        debug_assert!(io.iter().all(|c| c.len() <= MAX_QUANTUM));
        self.meters.measure_input(io);
        if let Some(spectrum) = &mut self.spectrum {
            spectrum.push_input(io);
        }
        let mut insert = Insert {
            send: &mut self.insert_send,
            ret,
            written: false,
        };
        // The monotonic clock is read through the vDSO: no syscall on the audio thread.
        let started = std::time::Instant::now();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            self.slot_ns[index] = 0;
            if let Some(slot) = slot.as_mut().filter(|s| !s.bypass) {
                ACTIVE_SLOT.store(index + 1, Ordering::Relaxed);
                let slot_started = std::time::Instant::now();
                slot.process(io, key, &mut insert);
                self.slot_ns[index] = slot_started.elapsed().as_nanos() as u64;
            }
        }
        self.insert_written = insert.written;
        ACTIVE_SLOT.store(0, Ordering::Relaxed);
        let frames = io.first().map_or(0, |c| c.len()) as u64;
        let budget_ns = (frames * 1_000_000_000)
            .checked_div(u64::from(RATE.load(Ordering::Relaxed)))
            .unwrap_or(0);
        let total_ns = started.elapsed().as_nanos() as u64;
        let overrun = self.watchdog.check(&self.slot_ns, total_ns, budget_ns);
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if overrun & (1 << index) != 0
                && let Some(slot) = slot
            {
                slot.bypass = true;
            }
        }
        self.apply_auto_level(io);
        self.apply_gain(io);
        self.meters.measure_output(io);
        if let Some(spectrum) = &mut self.spectrum {
            spectrum.push_output(io);
        }
        if let (Some(tap), [left, right, ..]) = (&mut self.tap, &*io) {
            tap.push(left, right);
        }
        self.loudness.feed(io);
        let values = self.loudness.values();
        self.meters.set_loudness(values);
        self.meters.set_balance(self.loudness.balance());
        let rate = RATE.load(Ordering::Relaxed);
        if rate > 0 {
            let frames = io.first().map_or(0, |c| c.len());
            self.auto
                .update(values[1], values[3], frames as f32 / rate as f32);
        }
        self.meters.set_auto_gain_db(self.auto.gain_db());
    }

    /// The automatic level's gain, ramped across the cycle from last cycle's so a change
    /// never clicks. Real-time.
    fn apply_auto_level(&mut self, io: &mut [&mut [f32]]) {
        let target = 10f32.powf(self.auto.gain_db() / 20.0);
        if target == 1.0 && self.auto_gain == 1.0 {
            return;
        }
        let frames = io.first().map_or(0, |c| c.len());
        let step = (target - self.auto_gain) / frames.max(1) as f32;
        for channel in io.iter_mut() {
            for (i, sample) in channel.iter_mut().take(frames).enumerate() {
                *sample *= self.auto_gain + step * (i + 1) as f32;
            }
        }
        self.auto_gain = target;
    }

    /// What the hardware insert sent this cycle, planar, or `None` when no slot did.
    pub fn insert_send(&self) -> Option<&[f32]> {
        self.insert_written.then_some(self.insert_send.as_slice())
    }

    /// Copies the processed main pair into every other destination pair with its gain,
    /// then applies the main pair's own gain; each gain slides per sample. Real-time.
    pub fn fan_out(&mut self, main: &mut [&mut [f32]], extra: &mut [&mut [f32]]) {
        let frames = main.first().map_or(0, |c| c.len());
        for (pair, chunk) in extra.chunks_mut(2).enumerate() {
            let send = pair + 1;
            if send >= MAX_SENDS {
                break;
            }
            let (start, target) = (self.sends[send], self.send_targets[send]);
            for (c, channel) in chunk.iter_mut().enumerate() {
                let Some(source) = main.get(c) else {
                    continue;
                };
                let mut gain = start;
                let n = frames.min(channel.len()).min(source.len());
                for i in 0..n {
                    gain += (target - gain).clamp(-MUTE_STEP, MUTE_STEP);
                    channel[i] = source[i] * gain;
                }
                channel[n..].fill(0.0);
            }
            let mut gain = start;
            for _ in 0..frames {
                gain += (target - gain).clamp(-MUTE_STEP, MUTE_STEP);
            }
            self.sends[send] = gain;
        }
        let (start, target) = (self.sends[0], self.send_targets[0]);
        if start == target && start == 1.0 {
            return;
        }
        for channel in main.iter_mut() {
            let mut gain = start;
            for sample in channel.iter_mut().take(frames) {
                gain += (target - gain).clamp(-MUTE_STEP, MUTE_STEP);
                *sample *= gain;
            }
        }
        let mut gain = start;
        for _ in 0..frames {
            gain += (target - gain).clamp(-MUTE_STEP, MUTE_STEP);
        }
        self.sends[0] = gain;
    }

    /// The mute ramp: a per-sample slide towards the target, the same on every channel.
    fn apply_gain(&mut self, io: &mut [&mut [f32]]) {
        if self.gain == self.gain_target && self.gain == 1.0 {
            return;
        }
        let frames = io.first().map_or(0, |c| c.len());
        let start = self.gain;
        for channel in io.iter_mut() {
            let mut gain = start;
            for sample in channel.iter_mut().take(frames) {
                gain += (self.gain_target - gain).clamp(-MUTE_STEP, MUTE_STEP);
                *sample *= gain;
            }
        }
        let mut gain = start;
        for _ in 0..frames {
            gain += (self.gain_target - gain).clamp(-MUTE_STEP, MUTE_STEP);
        }
        self.gain = gain;
    }

    /// Sum of the slots' latencies, in samples.
    pub fn latency_samples(&self) -> u32 {
        self.slots
            .iter()
            .flatten()
            .map(|s| s.latency_samples())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::{SlotConfig, SlotSpec};
    use std::sync::atomic::AtomicU32;

    const RATE: u32 = 48_000;

    #[test]
    fn a_slot_that_overruns_the_cycle_is_bypassed_before_the_kernel_kills_us() {
        let _serial = crate::plugin_test_guard();
        crate::overload::RATE.store(48_000, Ordering::Relaxed);
        let mut chain = Chain::new(Arc::default());
        let config = SlotConfig::new(SlotSpec::Hardware {
            send: "o".into(),
            ret: "i".into(),
            latency: 0,
        });
        let heavy = SlotRunner::busy(&config, 2, std::time::Duration::from_millis(3));
        chain.apply(Message::Replace {
            index: 1,
            slot: Some(Box::new(heavy)),
        });
        // 64 frames at 48 kHz is a 1.33 ms cycle; the slot takes 3 ms every call.
        for _ in 0..3 {
            let (mut left, mut right) = (vec![0.0; 64], vec![0.0; 64]);
            chain.process(&mut [&mut left, &mut right], &[], &[]);
        }
        assert!(chain.slots[1].as_ref().is_some_and(|s| s.bypass));
        let reports = crate::overload::take_reports();
        assert_eq!(reports.len(), 1);
        let (slot, slot_us, cycle_us) = reports[0];
        assert_eq!((slot, cycle_us), (1, 1333));
        assert!(slot_us >= 3000);
        crate::overload::RATE.store(0, Ordering::Relaxed);
    }

    fn sine(freq: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / RATE as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn run(chain: &mut Chain, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.chunks_mut(1024).zip(right.chunks_mut(1024)) {
            chain.process(&mut [l, r], &[], &[]);
        }
    }

    /// A hardware insert with nothing on its return: the wet signal is silence.
    fn silencer() -> Box<SlotRunner> {
        let config = SlotConfig::new(SlotSpec::Hardware {
            send: "o".into(),
            ret: "i".into(),
            latency: 0,
        });
        Box::new(SlotRunner::hardware(
            &config,
            0,
            Arc::new(AtomicU32::new(0)),
        ))
    }

    #[test]
    fn a_slot_shapes_the_output_and_bypass_restores_it() {
        let meters = Arc::new(Meters::default());
        let mut chain = Chain::new(Arc::clone(&meters));
        let garbage = chain.apply(Message::Replace {
            index: 0,
            slot: Some(silencer()),
        });
        assert!(garbage.is_none());

        let mut left = sine(2000.0, RATE as usize);
        let mut right = left.clone();
        let input_rms = rms(&left);
        run(&mut chain, &mut left, &mut right);
        let tail = &left[RATE as usize / 2..];
        let drop_db = 20.0 * (rms(tail) / input_rms).log10();
        assert!(drop_db < -40.0, "the slot attenuated only {drop_db:.1} dB");
        let (input, output) = meters.take();
        assert!((input[0] - 1.0).abs() < 0.01, "input peak {input:?}");
        assert!(output[0] < 0.1, "output peak {output:?}");

        chain.apply(Message::Bypass {
            index: 0,
            bypass: true,
        });
        let mut left = sine(2000.0, RATE as usize);
        let mut right = left.clone();
        run(&mut chain, &mut left, &mut right);
        assert!((rms(&left) - input_rms).abs() < 1e-3);
    }

    #[test]
    fn half_mix_blends_dry_and_wet_and_replace_hands_back_the_old_runner() {
        let mut chain = Chain::new(Arc::new(Meters::default()));
        chain.apply(Message::Replace {
            index: 1,
            slot: Some(silencer()),
        });
        chain.apply(Message::Mix { index: 1, mix: 0.5 });
        let mut left = sine(5000.0, RATE as usize);
        let mut right = left.clone();
        let input_rms = rms(&left);
        run(&mut chain, &mut left, &mut right);
        let ratio = rms(&left[RATE as usize / 2..]) / input_rms;
        assert!((ratio - 0.5).abs() < 0.02, "half mix gave {ratio:.3}");

        let old = chain.apply(Message::Replace {
            index: 1,
            slot: None,
        });
        assert!(old.is_some());
        assert_eq!(chain.latency_samples(), 0);
    }

    #[test]
    fn a_waiting_slot_passes_audio_through() {
        let mut chain = Chain::new(Arc::new(Meters::default()));
        let config = SlotConfig::new(SlotSpec::Group { lanes: Vec::new() });
        chain.apply(Message::Replace {
            index: 0,
            slot: Some(Box::new(SlotRunner::passthrough(&config, 2))),
        });
        let original = sine(440.0, 4096);
        let mut left = original.clone();
        let mut right = original.clone();
        run(&mut chain, &mut left, &mut right);
        assert_eq!(left, original);
    }

    #[test]
    fn mute_fades_to_silence_and_back() {
        let meters = Arc::new(Meters::default());
        let mut chain = Chain::new(Arc::clone(&meters));
        chain.apply(Message::Mute { mute: true });
        let mut left = vec![0.5f32; 2048];
        let mut right = left.clone();
        run(&mut chain, &mut left, &mut right);
        assert!(
            left[0] < 0.5 && left[0] > 0.49,
            "the ramp starts gently: {}",
            left[0]
        );
        assert_eq!(left[2047], 0.0, "silent after the ramp");
        chain.apply(Message::Mute { mute: false });
        let mut left = vec![0.5f32; 2048];
        let mut right = left.clone();
        run(&mut chain, &mut left, &mut right);
        assert!(
            (left[2047] - 0.5).abs() < 1e-6,
            "back to unity: {}",
            left[2047]
        );
    }
}
