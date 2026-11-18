//! Offline sample-rate conversion for decoded sounds: a windowed-sinc interpolator with its
//! cutoff lowered when downsampling, so nothing above the new Nyquist folds back.

use std::f64::consts::PI;

/// Taps on each side of the interpolation point.
const HALF_TAPS: i64 = 24;

/// `frames` of stereo at `from` Hz as stereo at `to` Hz.
pub fn resample(frames: &[[f32; 2]], from: u32, to: u32) -> Vec<[f32; 2]> {
    if from == to || frames.is_empty() {
        return frames.to_vec();
    }
    let step = f64::from(from) / f64::from(to);
    let cutoff = (f64::from(to) / f64::from(from)).min(1.0);
    let out_len = (frames.len() as f64 / step).ceil() as usize;
    let last = frames.len() as i64 - 1;
    (0..out_len)
        .map(|n| {
            let t = n as f64 * step;
            let center = t.floor() as i64;
            let mut acc = [0.0f64; 2];
            for k in center - HALF_TAPS + 1..=center + HALF_TAPS {
                if k < 0 || k > last {
                    continue;
                }
                let w = kernel(t - k as f64, cutoff);
                let frame = frames[k as usize];
                acc[0] += w * f64::from(frame[0]);
                acc[1] += w * f64::from(frame[1]);
            }
            [acc[0] as f32, acc[1] as f32]
        })
        .collect()
}

/// A Blackman-windowed sinc at `cutoff` (fraction of the input Nyquist), unity at DC.
fn kernel(x: f64, cutoff: f64) -> f64 {
    let span = HALF_TAPS as f64;
    if x.abs() >= span {
        return 0.0;
    }
    let sinc = if x == 0.0 {
        1.0
    } else {
        (PI * cutoff * x).sin() / (PI * cutoff * x)
    };
    let phase = PI * (x / span + 1.0);
    let window = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
    cutoff * sinc * window
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, frames: usize) -> Vec<[f32; 2]> {
        (0..frames)
            .map(|i| {
                let v = (2.0 * PI * freq * i as f64 / f64::from(rate)).sin() as f32 * 0.5;
                [v, v]
            })
            .collect()
    }

    fn rms(frames: &[[f32; 2]]) -> f64 {
        let sum: f64 = frames.iter().map(|f| f64::from(f[0]).powi(2)).sum();
        (sum / frames.len() as f64).sqrt()
    }

    #[test]
    fn a_tone_keeps_its_length_pitch_and_level() {
        let input = sine(1000.0, 44_100, 44_100);
        let output = resample(&input, 44_100, 48_000);
        assert_eq!(output.len(), 48_000);
        let middle = &output[4_000..44_000];
        let level = rms(middle) / rms(&input[4_000..40_000]);
        assert!((level - 1.0).abs() < 0.01, "level ratio {level}");
        let crossings = middle
            .windows(2)
            .filter(|w| w[0][0] < 0.0 && w[1][0] >= 0.0)
            .count();
        // 40 000 frames at 48 kHz is 0.833 s of a 1 kHz tone.
        assert!((832..=834).contains(&crossings), "{crossings} cycles");
    }

    #[test]
    fn downsampling_drops_what_the_new_rate_cannot_hold() {
        let input = sine(30_000.0, 96_000, 96_000);
        let output = resample(&input, 96_000, 48_000);
        assert!(
            rms(&output[1_000..47_000]) < 0.01,
            "30 kHz must not fold back"
        );
    }
}
