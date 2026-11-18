//! The real-time side of one slot: a processor plus bypass and dry/wet. Built off the
//! audio thread (it allocates), then moved into the chain through the message ring.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::MAX_QUANTUM;
use crate::chain::{INSERT_CHANNELS, Insert};
use crate::clap::ClapProcessor;
use crate::crash::ACTIVE_SLOT;
use crate::lv2::Lv2Processor;
use crate::slot::{SlotConfig, lane_slot_id};

/// Samples a lane can be delayed to line up with the slowest one: 1.4 s at 48 kHz.
const DELAY_CAPACITY: usize = 1 << 16;
/// A lane's gain slides over this many samples (10 ms at 48 kHz).
const LANE_GAIN_STEP: f32 = 1.0 / 480.0;

/// A click that never comes back within this many samples is given up on.
const MEASURE_TIMEOUT: u32 = 480_000;
/// The return has to be quiet this long before the click goes out, so nothing still in
/// the loop is taken for it.
const MEASURE_QUIET: u32 = 4_800;
/// The click is full scale; anything above this on the return is it.
const CLICK_THRESHOLD: f32 = 0.25;

/// `Hardware::measured` values: `0` nothing new, `MAX` no return, else samples + 1.
pub const MEASURE_NONE: u32 = u32::MAX;

/// The audio thread's view of a slot.
pub struct SlotRunner {
    processor: Processor,
    pub(crate) bypass: bool,
    pub(crate) mix: f32,
    /// One `MAX_QUANTUM` block per channel, the input kept for the dry/wet blend.
    dry: Vec<f32>,
    latency: u32,
}

enum Processor {
    Clap(Box<ClapProcessor>),
    Lv2(Box<Lv2Processor>),
    Hardware(Hardware),
    Group(Box<Group>),
    /// A plugin that failed to load keeps its slot and passes audio through.
    Passthrough,
    /// Tests only: spins this long per call, like a plugin too heavy for the cycle.
    #[cfg(test)]
    Busy(std::time::Duration),
}

impl SlotRunner {
    /// A slot that passes audio through: one whose plugin is loading, failed or crashed.
    /// Allocates everything the slot will ever need; never called on the audio thread.
    pub fn passthrough(config: &SlotConfig, channels: usize) -> Self {
        Self::with(Processor::Passthrough, config, channels, 0)
    }

    #[cfg(test)]
    pub(crate) fn busy(
        config: &SlotConfig,
        channels: usize,
        per_call: std::time::Duration,
    ) -> Self {
        Self::with(Processor::Busy(per_call), config, channels, 0)
    }

    /// A parallel group over `lanes`, each already built; `top` is the group's own index,
    /// which names its lane slots for the crash report.
    pub fn group(config: &SlotConfig, top: usize, lanes: Vec<LaneRunner>, channels: usize) -> Self {
        let trim = lane_trim(&lanes);
        let group = Group {
            lanes,
            top,
            latency: 0,
            trim,
        };
        Self::with(Processor::Group(Box::new(group)), config, channels, 0)
    }

    /// The slots of the group's lanes, for the loop thread to retire; `Err` gives the
    /// runner back when it is not a group.
    pub fn into_lane_slots(self) -> Result<Vec<Box<SlotRunner>>, Box<Self>> {
        match self.processor {
            Processor::Group(group) => Ok(group.lanes.into_iter().flat_map(|l| l.slots).collect()),
            processor => Err(Box::new(SlotRunner { processor, ..self })),
        }
    }

    /// A slot inside this group's lane. Real-time.
    pub(crate) fn lane_slot_mut(&mut self, lane: usize, sub: usize) -> Option<&mut SlotRunner> {
        match &mut self.processor {
            Processor::Group(group) => group
                .lanes
                .get_mut(lane)?
                .slots
                .get_mut(sub)
                .map(|s| &mut **s),
            _ => None,
        }
    }

    /// Swaps a slot of a lane; the old one goes back for the loop thread. Real-time.
    pub(crate) fn replace_lane_slot(
        &mut self,
        lane: usize,
        sub: usize,
        slot: Option<Box<SlotRunner>>,
    ) -> Option<Box<SlotRunner>> {
        let Processor::Group(group) = &mut self.processor else {
            return slot;
        };
        let Some(lane) = group.lanes.get_mut(lane) else {
            return slot;
        };
        match slot {
            Some(new) => match lane.slots.get_mut(sub) {
                Some(at) => Some(std::mem::replace(at, new)),
                None => Some(new),
            },
            // Only the last slot of a lane can go away in place; shorter lists are rebuilt.
            None if sub + 1 == lane.slots.len() => lane.slots.pop(),
            None => None,
        }
    }

    pub(crate) fn set_lane_gain(&mut self, lane: usize, gain: f32) {
        if let Processor::Group(group) = &mut self.processor
            && let Some(lane) = group.lanes.get_mut(lane)
        {
            lane.gain_target = gain;
            lane.gain_set = gain;
        }
    }

    pub(crate) fn set_lane_pan(&mut self, lane: usize, pan: f32) {
        if let Processor::Group(group) = &mut self.processor
            && let Some(lane) = group.lanes.get_mut(lane)
        {
            lane.pan_target = pan.clamp(-1.0, 1.0);
        }
    }

    pub(crate) fn set_lane_mute(&mut self, lane: usize, mute: bool) {
        if let Processor::Group(group) = &mut self.processor
            && let Some(lane) = group.lanes.get_mut(lane)
        {
            lane.mute = mute;
            lane.gain_target = if mute { 0.0 } else { lane.gain_set };
        }
    }

    /// A hardware insert: its round trip is `latency` until a measurement, which lands
    /// in `measured` for the loop thread.
    pub fn hardware(config: &SlotConfig, latency: u32, measured: Arc<AtomicU32>) -> Self {
        let hardware = Hardware {
            measuring: None,
            measured,
        };
        Self::with(
            Processor::Hardware(hardware),
            config,
            crate::chain::INSERT_CHANNELS,
            latency,
        )
    }

    /// The hardware insert in this slot sends its click now. Real-time.
    pub(crate) fn start_measure(&mut self) {
        if let Processor::Hardware(hardware) = &mut self.processor {
            hardware.measuring = Some(Measure::Quiet { quiet: 0, total: 0 });
        }
    }

    pub fn lv2(config: &SlotConfig, processor: Lv2Processor, channels: usize) -> Self {
        Self::with(Processor::Lv2(Box::new(processor)), config, channels, 0)
    }

    /// The LV2 processor when this slot hosts one, else the runner back.
    pub fn into_lv2_or_back(self) -> Result<Lv2Processor, Box<Self>> {
        match self.processor {
            Processor::Lv2(processor) => Ok(*processor),
            processor => Err(Box::new(SlotRunner { processor, ..self })),
        }
    }

    /// A slot around a plugin the loop thread has already loaded and activated.
    pub fn clap(
        config: &SlotConfig,
        processor: ClapProcessor,
        channels: usize,
        latency: u32,
    ) -> Self {
        Self::with(
            Processor::Clap(Box::new(processor)),
            config,
            channels,
            latency,
        )
    }

    fn with(processor: Processor, config: &SlotConfig, channels: usize, latency: u32) -> Self {
        SlotRunner {
            processor,
            bypass: config.bypass,
            mix: config.mix.clamp(0.0, 1.0),
            dry: vec![0.0; MAX_QUANTUM * channels.max(1)],
            latency,
        }
    }

    /// Samples of delay this slot adds; summed by the chain and reported to PipeWire.
    pub fn latency_samples(&self) -> u32 {
        match &self.processor {
            Processor::Group(group) => group.latency,
            Processor::Lv2(processor) => processor.latency_samples(),
            _ => self.latency,
        }
    }

    /// The plugin processor, when this slot hosts one, for the loop thread to deactivate it.
    pub fn into_clap(self) -> Option<ClapProcessor> {
        match self.processor {
            Processor::Clap(processor) => Some(*processor),
            _ => None,
        }
    }

    pub(crate) fn set_param(&mut self, id: u32, value: f64) {
        match &mut self.processor {
            Processor::Clap(processor) => processor.set_param(id, value),
            Processor::Lv2(processor) => processor.set_param(id, value),
            _ => {}
        }
    }

    /// Real-time: no allocation, no panics; `io` holds at most `MAX_QUANTUM` frames per
    /// channel, `key` the chain's sidechain for plugins that take one, `insert` the
    /// hardware send and return for a hardware slot.
    pub(crate) fn process(&mut self, io: &mut [&mut [f32]], key: &[&[f32]], insert: &mut Insert) {
        if self.bypass || io.is_empty() {
            return;
        }
        let frames = io[0].len().min(MAX_QUANTUM);
        let blend = self.mix < 1.0;
        if blend {
            for (c, channel) in io.iter().enumerate() {
                let Some(dry) = self.dry.get_mut(c * MAX_QUANTUM..c * MAX_QUANTUM + frames) else {
                    break;
                };
                dry.copy_from_slice(&channel[..frames]);
            }
        }
        match &mut self.processor {
            Processor::Clap(processor) => processor.process(&mut io[..], key),
            Processor::Lv2(processor) => processor.process(&mut io[..], key),
            Processor::Hardware(hardware) => {
                if let Some(latency) = hardware.process(io, insert, frames) {
                    self.latency = latency;
                }
            }
            Processor::Group(group) => group.process(io, key, insert, frames),
            Processor::Passthrough => {}
            #[cfg(test)]
            Processor::Busy(per_call) => {
                let end = std::time::Instant::now() + *per_call;
                while std::time::Instant::now() < end {}
            }
        }
        if blend {
            let wet = self.mix;
            for (c, channel) in io.iter_mut().enumerate() {
                let Some(dry) = self.dry.get(c * MAX_QUANTUM..c * MAX_QUANTUM + frames) else {
                    break;
                };
                for (out, dry) in channel[..frames].iter_mut().zip(dry) {
                    *out = *out * wet + *dry * (1.0 - wet);
                }
            }
        }
    }
}

/// One lane of a parallel group on the audio thread.
pub struct LaneRunner {
    // Boxed as the garbage ring carries them: a swap moves a pointer, not a runner.
    #[allow(clippy::vec_box)]
    slots: Vec<Box<SlotRunner>>,
    /// Planar scratch, one `MAX_QUANTUM` block per channel.
    buffer: Vec<f32>,
    /// A ring per channel that lines the lane up with the slowest one.
    delay: Vec<f32>,
    write: usize,
    gain: f32,
    gain_target: f32,
    /// The gain the user set, which mute leaves and unmute returns to.
    gain_set: f32,
    mute: bool,
    pan: f32,
    pan_target: f32,
}

impl LaneRunner {
    /// Allocates the lane; never on the audio thread.
    pub fn new(slots: Vec<Box<SlotRunner>>, gain: f32, mute: bool, channels: usize) -> Self {
        let channels = channels.clamp(1, INSERT_CHANNELS);
        LaneRunner {
            slots,
            buffer: vec![0.0; channels * MAX_QUANTUM],
            delay: vec![0.0; channels * DELAY_CAPACITY],
            write: 0,
            gain: if mute { 0.0 } else { gain },
            gain_target: if mute { 0.0 } else { gain },
            gain_set: gain,
            mute,
            pan: 0.0,
            pan_target: 0.0,
        }
    }

    /// The lane starting at balance `pan`, −1 to 1.
    pub fn panned(mut self, pan: f32) -> Self {
        self.pan = pan.clamp(-1.0, 1.0);
        self.pan_target = self.pan;
        self
    }

    fn latency(&self) -> u32 {
        self.slots.iter().map(|s| s.latency_samples()).sum()
    }
}

/// Parallel lanes summed back into the slot. Real-time.
struct Group {
    lanes: Vec<LaneRunner>,
    top: usize,
    latency: u32,
    /// The ramped 1/n over the unmuted lanes: identical lanes sum back to unity.
    trim: f32,
}

/// A balance control's gain on channel `c`: the far side turns down, the near side
/// stays at unity, so centre changes nothing and no side ever gains. Mono ignores it.
fn balance(pan: f32, c: usize, channels: usize) -> f32 {
    match (channels, c) {
        (2, 0) => (1.0 - pan).min(1.0),
        (2, 1) => (1.0 + pan).min(1.0),
        _ => 1.0,
    }
}

/// 1/n for n unmuted lanes, so n copies of the signal come back at 0 dB.
fn lane_trim(lanes: &[LaneRunner]) -> f32 {
    let open = lanes.iter().filter(|l| !l.mute).count();
    1.0 / open.max(1) as f32
}

impl Group {
    fn process(
        &mut self,
        io: &mut [&mut [f32]],
        key: &[&[f32]],
        insert: &mut Insert,
        frames: usize,
    ) {
        // No lanes yet: the slot is empty and lets the signal through, like a bypass.
        if self.lanes.is_empty() {
            self.latency = 0;
            return;
        }
        let channels = io.len().min(INSERT_CHANNELS);
        for (lane_index, lane) in self.lanes.iter_mut().enumerate() {
            let (left, right) = lane.buffer.split_at_mut(MAX_QUANTUM);
            let mut lane_io: [&mut [f32]; INSERT_CHANNELS] =
                [&mut left[..frames], &mut right[..frames]];
            for (c, channel) in lane_io.iter_mut().enumerate().take(channels) {
                channel.copy_from_slice(&io[c][..frames]);
            }
            for (sub, slot) in lane.slots.iter_mut().enumerate() {
                ACTIVE_SLOT.store(
                    lane_slot_id(self.top, lane_index, sub) + 1,
                    Ordering::Relaxed,
                );
                slot.process(&mut lane_io[..channels], key, insert);
            }
        }
        ACTIVE_SLOT.store(self.top + 1, Ordering::Relaxed);
        let latency = self
            .lanes
            .iter()
            .map(LaneRunner::latency)
            .max()
            .unwrap_or(0);
        self.latency = latency;
        for channel in io.iter_mut() {
            channel[..frames].fill(0.0);
        }
        for lane in &mut self.lanes {
            let delay = ((latency - lane.latency()) as usize).min(DELAY_CAPACITY - 1);
            let mut gain = lane.gain;
            let mut pan = lane.pan;
            for i in 0..frames {
                gain += (lane.gain_target - gain).clamp(-LANE_GAIN_STEP, LANE_GAIN_STEP);
                pan += (lane.pan_target - pan).clamp(-LANE_GAIN_STEP, LANE_GAIN_STEP);
                let write = (lane.write + i) % DELAY_CAPACITY;
                let read = (write + DELAY_CAPACITY - delay) % DELAY_CAPACITY;
                for (c, channel) in io.iter_mut().enumerate().take(channels) {
                    let ring = &mut lane.delay[c * DELAY_CAPACITY..(c + 1) * DELAY_CAPACITY];
                    ring[write] = lane.buffer[c * MAX_QUANTUM + i];
                    channel[i] += ring[read] * gain * balance(pan, c, channels);
                }
            }
            lane.write = (lane.write + frames) % DELAY_CAPACITY;
            lane.gain = gain;
            lane.pan = pan;
        }
        // Ramped like the lane gains, so a mute or unmute changes the trim without a click.
        let target = lane_trim(&self.lanes);
        let mut trim = self.trim;
        for i in 0..frames {
            trim += (target - trim).clamp(-LANE_GAIN_STEP, LANE_GAIN_STEP);
            for channel in io.iter_mut().take(channels) {
                channel[i] *= trim;
            }
        }
        self.trim = trim;
    }
}

/// External gear in a slot: what enters leaves on the insert send, what came back
/// takes its place; a click measures the round trip.
struct Hardware {
    /// A measurement under way; the send carries silence (then the click) meanwhile.
    measuring: Option<Measure>,
    measured: Arc<AtomicU32>,
}

#[derive(Clone, Copy)]
enum Measure {
    /// Waiting for the return to be quiet for `MEASURE_QUIET` samples.
    Quiet { quiet: u32, total: u32 },
    /// The click is out; `elapsed` samples since.
    Flight { elapsed: u32, total: u32 },
}

impl Hardware {
    /// Real-time. Returns the new round trip when the click just came back.
    fn process(
        &mut self,
        io: &mut [&mut [f32]],
        insert: &mut Insert,
        frames: usize,
    ) -> Option<u32> {
        let mut found = None;
        let hit = insert.ret.first().and_then(|r| {
            r[..frames.min(r.len())]
                .iter()
                .position(|s| s.abs() > CLICK_THRESHOLD)
        });
        let click = match self.measuring {
            None => false,
            Some(Measure::Quiet { quiet, total }) => {
                let quiet = if hit.is_some() {
                    0
                } else {
                    quiet + frames as u32
                };
                let total = total + frames as u32;
                if total > MEASURE_TIMEOUT {
                    self.measured.store(MEASURE_NONE, Ordering::Relaxed);
                    self.measuring = None;
                    false
                } else if quiet >= MEASURE_QUIET && frames > 0 {
                    // The click's own cycle counts: its return shows up a cycle later at
                    // the earliest, and that cycle is latency the chain adds.
                    self.measuring = Some(Measure::Flight {
                        elapsed: frames as u32,
                        total,
                    });
                    true
                } else {
                    self.measuring = Some(Measure::Quiet { quiet, total });
                    false
                }
            }
            Some(Measure::Flight { elapsed, total }) => {
                match hit {
                    Some(at) => {
                        let samples = elapsed + at as u32;
                        self.measured.store(samples + 1, Ordering::Relaxed);
                        self.measuring = None;
                        found = Some(samples);
                    }
                    None if total > MEASURE_TIMEOUT => {
                        self.measured.store(MEASURE_NONE, Ordering::Relaxed);
                        self.measuring = None;
                    }
                    None => {
                        self.measuring = Some(Measure::Flight {
                            elapsed: elapsed + frames as u32,
                            total: total + frames as u32,
                        });
                    }
                }
                false
            }
        };
        for (c, channel) in io.iter().enumerate() {
            let Some(send) = insert
                .send
                .get_mut(c * MAX_QUANTUM..c * MAX_QUANTUM + frames)
            else {
                break;
            };
            if self.measuring.is_some() || click {
                // Program audio stays out of the loop until the click is back.
                send.fill(0.0);
                if click {
                    send[0] = 1.0;
                }
            } else {
                send.copy_from_slice(&channel[..frames]);
            }
        }
        insert.written = true;
        for (c, channel) in io.iter_mut().enumerate() {
            match insert.ret.get(c).or(insert.ret.last()) {
                Some(ret) => {
                    let n = frames.min(ret.len());
                    channel[..n].copy_from_slice(&ret[..n]);
                    channel[n..frames].fill(0.0);
                }
                None => channel[..frames].fill(0.0),
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::SlotSpec;

    /// A loopback that hands the send back `delay` samples later.
    struct Loop {
        line: Vec<f32>,
        delay: usize,
    }

    impl Loop {
        fn turn(&mut self, send: &[f32], frames: usize) -> Vec<f32> {
            let mut out = Vec::with_capacity(frames);
            for &sample in &send[..frames] {
                self.line.push(sample);
                let at = self.line.len() - 1;
                out.push(if at >= self.delay {
                    self.line[at - self.delay]
                } else {
                    0.0
                });
            }
            out
        }
    }

    #[test]
    fn a_group_sums_its_lanes_at_unity_lined_up_to_the_slowest() {
        let passthrough = || {
            Box::new(SlotRunner::passthrough(
                &SlotConfig::new(SlotSpec::Group { lanes: Vec::new() }),
                2,
            ))
        };
        // Lane B is a hardware insert with a known 100-sample trip and no return: it adds
        // silence, but lane A has to wait for it.
        let slow = Box::new(SlotRunner::hardware(
            &SlotConfig::new(SlotSpec::Hardware {
                send: "o".into(),
                ret: "i".into(),
                latency: 100,
            }),
            100,
            Arc::new(AtomicU32::new(0)),
        ));
        let lanes = vec![
            LaneRunner::new(vec![passthrough()], 0.5, false, 2),
            LaneRunner::new(vec![slow], 1.0, false, 2),
        ];
        let config = SlotConfig::new(SlotSpec::Group { lanes: Vec::new() });
        let mut group = SlotRunner::group(&config, 0, lanes, 2);
        let mut send = vec![0.0f32; 2 * MAX_QUANTUM];
        let mut left = vec![0.0f32; 256];
        left[0] = 1.0;
        let mut right = left.clone();
        let mut insert = Insert {
            send: &mut send,
            ret: &[],
            written: false,
        };
        group.process(&mut [&mut left, &mut right], &[], &mut insert);
        assert_eq!(group.latency_samples(), 100);
        assert_eq!(left[0], 0.0, "the fast lane waits for the slow one");
        assert!(
            (left[100] - 0.25).abs() < 1e-6,
            "lane A at -6 dB, halved by the two-lane trim, 100 samples late"
        );
        assert!(left.iter().filter(|s| **s != 0.0).count() == 1);

        // Both lanes passing through at unity come back at unity, not twice the input.
        let lanes = vec![
            LaneRunner::new(vec![passthrough()], 1.0, false, 2),
            LaneRunner::new(vec![passthrough()], 1.0, false, 2),
        ];
        let mut group = SlotRunner::group(&config, 0, lanes, 2);
        let mut left = vec![0.25f32; 256];
        let mut right = left.clone();
        let mut insert = Insert {
            send: &mut send,
            ret: &[],
            written: false,
        };
        group.process(&mut [&mut left, &mut right], &[], &mut insert);
        assert!(left.iter().all(|s| (s - 0.25).abs() < 1e-6));
        group.set_lane_mute(1, true);
        for _ in 0..4 {
            let mut insert = Insert {
                send: &mut send,
                ret: &[],
                written: false,
            };
            left.fill(0.25);
            right.fill(0.25);
            group.process(&mut [&mut left, &mut right], &[], &mut insert);
        }
        assert!(
            (left[255] - 0.25).abs() < 1e-6,
            "a muted lane fades out and the trim goes back to 1: {}",
            left[255]
        );
    }

    #[test]
    fn a_panned_lane_turns_the_far_side_down_and_glides_when_moved() {
        let passthrough = || {
            Box::new(SlotRunner::passthrough(
                &SlotConfig::new(SlotSpec::Group { lanes: Vec::new() }),
                2,
            ))
        };
        let lanes = vec![LaneRunner::new(vec![passthrough()], 1.0, false, 2).panned(-1.0)];
        let config = SlotConfig::new(SlotSpec::Group { lanes: Vec::new() });
        let mut group = SlotRunner::group(&config, 0, lanes, 2);
        let mut send = vec![0.0f32; 2 * MAX_QUANTUM];
        let mut run = |group: &mut SlotRunner| {
            let mut left = vec![0.5f32; 256];
            let mut right = left.clone();
            let mut insert = Insert {
                send: &mut send,
                ret: &[],
                written: false,
            };
            group.process(&mut [&mut left, &mut right], &[], &mut insert);
            (left, right)
        };
        let (left, right) = run(&mut group);
        assert!(
            left.iter().all(|s| (s - 0.5).abs() < 1e-6),
            "the near side stays at unity"
        );
        assert!(
            right.iter().all(|s| *s == 0.0),
            "hard left silences the right"
        );

        group.set_lane_pan(0, 0.0);
        let (_, right) = run(&mut group);
        assert!(right[0] < right[255], "the move glides, no jump");
        for _ in 0..4 {
            run(&mut group);
        }
        let (left, right) = run(&mut group);
        assert!((left[255] - 0.5).abs() < 1e-6 && (right[255] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_group_without_lanes_passes_the_signal_through() {
        let config = SlotConfig::new(SlotSpec::Group { lanes: Vec::new() });
        let mut group = SlotRunner::group(&config, 0, Vec::new(), 2);
        let mut send = vec![0.0f32; 2 * MAX_QUANTUM];
        let mut left = vec![0.25f32; 256];
        let mut right = left.clone();
        let mut insert = Insert {
            send: &mut send,
            ret: &[],
            written: false,
        };
        group.process(&mut [&mut left, &mut right], &[], &mut insert);
        assert!(left.iter().chain(&right).all(|s| *s == 0.25));
        assert_eq!(group.latency_samples(), 0);
    }

    #[test]
    fn a_hardware_insert_measures_its_round_trip_and_passes_the_return() {
        let config = SlotConfig::new(SlotSpec::Hardware {
            send: "out".into(),
            ret: "in".into(),
            latency: 0,
        });
        let measured = Arc::new(AtomicU32::new(0));
        let mut runner = SlotRunner::hardware(&config, 0, Arc::clone(&measured));
        let mut send = vec![0.0f32; 2 * MAX_QUANTUM];
        let (mut left_loop, mut right_loop) = (
            Loop {
                line: Vec::new(),
                delay: 700,
            },
            Loop {
                line: Vec::new(),
                delay: 700,
            },
        );
        let (mut ret_l, mut ret_r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        runner.start_measure();
        let mut rounds = 0;
        while measured.load(Ordering::Relaxed) == 0 && rounds < 60 {
            let mut left: Vec<f32> = (0..256).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
            let mut right = left.clone();
            let mut insert = Insert {
                send: &mut send,
                ret: &[ret_l.as_slice(), ret_r.as_slice()],
                written: false,
            };
            runner.process(&mut [&mut left, &mut right], &[], &mut insert);
            assert!(insert.written);
            ret_l = left_loop.turn(&send[..256], 256);
            ret_r = right_loop.turn(&send[MAX_QUANTUM..MAX_QUANTUM + 256], 256);
            rounds += 1;
        }
        // The harness hands back each cycle's send with the next cycle's return, so the
        // trip is the line's delay plus one period.
        assert_eq!(measured.load(Ordering::Relaxed), 700 + 256 + 1);
        assert_eq!(runner.latency_samples(), 700 + 256);

        // Once the click is through, what comes back is what went out, one trip later.
        let mut last_left = Vec::new();
        for _ in 0..8 {
            let mut left: Vec<f32> = vec![0.5; 256];
            let mut right = left.clone();
            let mut insert = Insert {
                send: &mut send,
                ret: &[ret_l.as_slice(), ret_r.as_slice()],
                written: false,
            };
            runner.process(&mut [&mut left, &mut right], &[], &mut insert);
            ret_l = left_loop.turn(&send[..256], 256);
            ret_r = right_loop.turn(&send[MAX_QUANTUM..MAX_QUANTUM + 256], 256);
            last_left = left;
        }
        assert!(last_left.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }
}
