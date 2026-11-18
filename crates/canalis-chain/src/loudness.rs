//! EBU R128 / ITU-R BS.1770 loudness on the chain output, inside the callback with
//! nothing allocated after construction: K-weighting (two biquads per channel), 100 ms
//! block powers in a ring for the momentary (400 ms) and short-term (3 s) values, a
//! histogram of gated 400 ms windows for the integrated value, and a 4x oversampled
//! true peak. Values leave through `Meters` as f32 bits.

use std::f32::consts::PI;

use crate::meters::METER_CHANNELS;

/// Blocks of 100 ms; four make a momentary window, thirty a short-term one.
const BLOCK_MS: u32 = 100;
const RING_BLOCKS: usize = 30;
const MOMENTARY_BLOCKS: usize = 4;
/// Histogram of window loudness from −70 LUFS up, 0.1 LU per bin.
const HIST_MIN: f32 = -70.0;
const HIST_BINS: usize = 751;
const ABSOLUTE_GATE: f32 = -70.0;
const RELATIVE_GATE: f32 = -10.0;
/// What silence reads as, so a JSON line never carries −inf.
pub const SILENCE: f32 = -200.0;
const OVERSAMPLE: usize = 4;
const TAPS: usize = 12;

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// The two K-weighting stages for `rate`, as libebur128 designs them.
fn k_weighting(rate: f32) -> (Biquad, Biquad) {
    let (f0, g, q) = (1681.9745_f32, 3.9998439_f32, 0.70717524_f32);
    let k = (PI * f0 / rate).tan();
    let vh = 10.0f32.powf(g / 20.0);
    let vb = vh.powf(0.49966677);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
        ..Biquad::default()
    };
    let (f0, q) = (38.13547_f32, 0.50032704_f32);
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
        ..Biquad::default()
    };
    (shelf, high_pass)
}

/// The windowed-sinc phases of the 4x interpolator, built once.
fn interpolator() -> [[f32; TAPS]; OVERSAMPLE] {
    let mut phases = [[0.0f32; TAPS]; OVERSAMPLE];
    let centre = (TAPS as f32 - 1.0) / 2.0;
    for (p, phase) in phases.iter_mut().enumerate() {
        for (t, tap) in phase.iter_mut().enumerate() {
            let x = t as f32 - centre - p as f32 / OVERSAMPLE as f32;
            let sinc = if x.abs() < 1e-6 {
                1.0
            } else {
                (PI * x).sin() / (PI * x)
            };
            let window = 0.5 + 0.5 * (2.0 * PI * (t as f32 + 0.5) / TAPS as f32 - PI).cos();
            *tap = sinc * window;
        }
    }
    phases
}

pub struct Loudness {
    rate: u32,
    filters: [(Biquad, Biquad); METER_CHANNELS],
    block_len: usize,
    /// Sum of weighted squares in the block under way, per channel.
    block_sum: [f64; METER_CHANNELS],
    block_fill: usize,
    /// Mean squares of the last blocks, summed over channels; oldest first in ring order.
    ring: [f64; RING_BLOCKS],
    ring_at: usize,
    ring_filled: usize,
    /// Each channel's mean square in the last `MOMENTARY_BLOCKS` blocks, for the balance.
    sides: [[f64; METER_CHANNELS]; MOMENTARY_BLOCKS],
    histogram: [u32; HIST_BINS],
    interpolator: [[f32; TAPS]; OVERSAMPLE],
    history: [[f32; TAPS]; METER_CHANNELS],
    /// Largest interpolated sample magnitude since the last reset.
    true_peak: f32,
    momentary: f32,
    short_term: f32,
    integrated: f32,
}

impl Default for Loudness {
    fn default() -> Self {
        Self::new(48_000)
    }
}

impl Loudness {
    pub fn new(rate: u32) -> Self {
        let mut loudness = Loudness {
            rate: 0,
            filters: [(Biquad::default(), Biquad::default()); METER_CHANNELS],
            block_len: 0,
            block_sum: [0.0; METER_CHANNELS],
            block_fill: 0,
            ring: [0.0; RING_BLOCKS],
            ring_at: 0,
            ring_filled: 0,
            sides: [[0.0; METER_CHANNELS]; MOMENTARY_BLOCKS],
            histogram: [0; HIST_BINS],
            interpolator: interpolator(),
            history: [[0.0; TAPS]; METER_CHANNELS],
            true_peak: 0.0,
            momentary: SILENCE,
            short_term: SILENCE,
            integrated: SILENCE,
        };
        loudness.set_rate(rate);
        loudness
    }

    /// A new sample rate: filters redesigned, everything measured so far dropped.
    pub fn set_rate(&mut self, rate: u32) {
        let rate = rate.max(1);
        self.rate = rate;
        let (shelf, high_pass) = k_weighting(rate as f32);
        self.filters = [(shelf, high_pass); METER_CHANNELS];
        self.block_len = (rate * BLOCK_MS / 1000) as usize;
        self.reset();
    }

    /// Forgets the integrated measurement and the peak hold.
    pub fn reset(&mut self) {
        self.block_sum = [0.0; METER_CHANNELS];
        self.block_fill = 0;
        self.ring = [0.0; RING_BLOCKS];
        self.ring_at = 0;
        self.ring_filled = 0;
        self.sides = [[0.0; METER_CHANNELS]; MOMENTARY_BLOCKS];
        self.histogram = [0; HIST_BINS];
        self.history = [[0.0; TAPS]; METER_CHANNELS];
        self.true_peak = 0.0;
        self.momentary = SILENCE;
        self.short_term = SILENCE;
        self.integrated = SILENCE;
    }

    /// `[momentary, short_term, integrated]` in LUFS and the true peak in dBTP.
    pub fn values(&self) -> [f32; 4] {
        [
            self.momentary,
            self.short_term,
            self.integrated,
            to_db(f64::from(self.true_peak * self.true_peak)),
        ]
    }

    /// Where the last 400 ms sit between the channels, by K-weighted power: −1 all left,
    /// 0 even, 1 all right; 0 in silence or with one channel.
    pub fn balance(&self) -> f32 {
        let (left, right) = self
            .sides
            .iter()
            .fold((0.0, 0.0), |(l, r), block| (l + block[0], r + block[1]));
        let total = left + right;
        if total <= from_lufs(ABSOLUTE_GATE) * MOMENTARY_BLOCKS as f64 {
            return 0.0;
        }
        ((right - left) / total) as f32
    }

    /// Real-time: filters, squares and the interpolator, one pass over `io`.
    pub fn feed(&mut self, io: &[&mut [f32]]) {
        if self.block_len == 0 {
            return;
        }
        let frames = io.first().map_or(0, |c| c.len());
        for i in 0..frames {
            for (c, channel) in io.iter().enumerate().take(METER_CHANNELS) {
                let x = channel[i];
                let (shelf, high_pass) = &mut self.filters[c];
                let w = high_pass.run(shelf.run(x));
                self.block_sum[c] += f64::from(w * w);
                self.true_peak = self.true_peak.max(self.interpolated_peak(c, x));
            }
            self.block_fill += 1;
            if self.block_fill >= self.block_len {
                self.close_block();
            }
        }
    }

    #[inline]
    fn interpolated_peak(&mut self, c: usize, x: f32) -> f32 {
        let history = &mut self.history[c];
        history.copy_within(1.., 0);
        history[TAPS - 1] = x;
        let mut peak = x.abs();
        for phase in &self.interpolator {
            let y: f32 = phase.iter().zip(history.iter()).map(|(t, h)| t * h).sum();
            peak = peak.max(y.abs());
        }
        peak
    }

    fn close_block(&mut self) {
        self.sides[self.ring_at % MOMENTARY_BLOCKS] =
            std::array::from_fn(|c| self.block_sum[c] / self.block_len as f64);
        let power: f64 = self
            .block_sum
            .iter()
            .map(|s| s / self.block_len as f64)
            .sum();
        self.block_sum = [0.0; METER_CHANNELS];
        self.block_fill = 0;
        self.ring[self.ring_at] = power;
        self.ring_at = (self.ring_at + 1) % RING_BLOCKS;
        self.ring_filled = (self.ring_filled + 1).min(RING_BLOCKS);

        let mean = |blocks: usize| -> f64 {
            let n = blocks.min(self.ring_filled);
            (1..=n)
                .map(|back| self.ring[(self.ring_at + RING_BLOCKS - back) % RING_BLOCKS])
                .sum::<f64>()
                / n.max(1) as f64
        };
        let momentary_power = mean(MOMENTARY_BLOCKS);
        self.momentary = to_lufs(momentary_power);
        self.short_term = to_lufs(mean(RING_BLOCKS));

        if self.ring_filled >= MOMENTARY_BLOCKS && self.momentary >= ABSOLUTE_GATE {
            let bin = ((self.momentary - HIST_MIN) * 10.0).round() as usize;
            self.histogram[bin.min(HIST_BINS - 1)] += 1;
        }
        self.integrated = self.integrate();
    }

    /// Mean of the windows above the relative gate, from the histogram.
    fn integrate(&self) -> f32 {
        let (mut count, mut sum) = (0u64, 0.0f64);
        for (bin, n) in self.histogram.iter().enumerate() {
            if *n > 0 {
                count += u64::from(*n);
                sum += f64::from(*n) * from_lufs(bin_lufs(bin));
            }
        }
        if count == 0 {
            return SILENCE;
        }
        let gate = to_lufs(sum / count as f64) + RELATIVE_GATE;
        let (mut count, mut sum) = (0u64, 0.0f64);
        for (bin, n) in self.histogram.iter().enumerate() {
            if *n > 0 && bin_lufs(bin) >= gate {
                count += u64::from(*n);
                sum += f64::from(*n) * from_lufs(bin_lufs(bin));
            }
        }
        if count == 0 {
            SILENCE
        } else {
            to_lufs(sum / count as f64)
        }
    }
}

fn bin_lufs(bin: usize) -> f32 {
    HIST_MIN + bin as f32 / 10.0
}

fn to_lufs(power: f64) -> f32 {
    if power <= 0.0 {
        SILENCE
    } else {
        (-0.691 + 10.0 * power.log10()).max(SILENCE as f64) as f32
    }
}

fn from_lufs(lufs: f32) -> f64 {
    10f64.powf((f64::from(lufs) + 0.691) / 10.0)
}

fn to_db(power: f64) -> f32 {
    if power <= 0.0 {
        SILENCE
    } else {
        (10.0 * power.log10()).max(SILENCE as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amplitude: f32, rate: u32, seconds: f32) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .map(|i| amplitude * (2.0 * PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn a_minus_20_dbfs_sine_reads_minus_20_lufs() {
        let mut loudness = Loudness::new(48_000);
        let mut left = sine(1000.0, 0.1, 48_000, 5.0);
        let mut right = left.clone();
        for (l, r) in left.chunks_mut(512).zip(right.chunks_mut(512)) {
            loudness.feed(&[l, r]);
        }
        let [m, s, i, tp] = loudness.values();
        assert!((m + 20.0).abs() < 0.3, "momentary {m}");
        assert!((s + 20.0).abs() < 0.3, "short-term {s}");
        assert!((i + 20.0).abs() < 0.3, "integrated {i}");
        assert!((tp + 20.0).abs() < 0.3, "true peak {tp}");
    }

    #[test]
    fn the_balance_follows_where_the_power_sits() {
        let mut loudness = Loudness::new(48_000);
        assert_eq!(loudness.balance(), 0.0, "silence reads centred");
        let mut left = sine(1000.0, 0.1, 48_000, 1.0);
        let mut right = left.clone();
        loudness.feed(&[&mut left, &mut right]);
        assert!(
            loudness.balance().abs() < 1e-3,
            "even: {}",
            loudness.balance()
        );

        let mut left = sine(1000.0, 0.1, 48_000, 1.0);
        let mut right = vec![0.0; left.len()];
        loudness.feed(&[&mut left, &mut right]);
        assert!(
            loudness.balance() < -0.99,
            "left only: {}",
            loudness.balance()
        );

        // Right at half the amplitude of the left: a quarter of its power.
        let mut left = sine(1000.0, 0.2, 48_000, 1.0);
        let mut right: Vec<f32> = left.iter().map(|s| s * 0.5).collect();
        loudness.feed(&[&mut left, &mut right]);
        assert!(
            (loudness.balance() + 0.6).abs() < 0.01,
            "{}",
            loudness.balance()
        );
    }

    #[test]
    fn silence_is_gated_out_of_the_integrated_value() {
        let mut loudness = Loudness::new(48_000);
        let mut left = sine(1000.0, 0.1, 48_000, 2.0);
        let mut right = left.clone();
        loudness.feed(&[&mut left, &mut right]);
        let before = loudness.values()[2];
        let mut quiet = vec![0.0f32; 48_000 * 3];
        let mut quiet2 = quiet.clone();
        loudness.feed(&[&mut quiet, &mut quiet2]);
        let after = loudness.values()[2];
        // The windows straddling the cut are above the gate and pull I down a little;
        // pure silence adds nothing.
        assert!(
            (before - after).abs() < 0.5,
            "silence changed I: {before} -> {after}"
        );
        assert!(loudness.values()[0] <= -70.0, "momentary not silent");
        loudness.reset();
        assert_eq!(loudness.values()[2], SILENCE);
    }
}
