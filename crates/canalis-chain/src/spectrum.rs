//! A live spectrum of what goes into a chain and what comes out, while a window shows it.
//! The callback's half, `SpectrumTap`, copies a mono mix of the input and of the output
//! into two lock-free rings; the analyzer thread windows the latest `FFT_SIZE` samples of
//! each, transforms them and folds the bins into `BANDS` log-spaced bands from 20 Hz to
//! 20 kHz. Like the recorder, the analyzer keeps its ends of the rings until the callback
//! drops the tap, so nothing is freed on the audio thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use realfft::RealFftPlanner;
use rtrb::{Consumer, Producer, RingBuffer};

/// Bands across 20 Hz–20 kHz.
pub const BANDS: usize = 64;
/// Quietest level a band reports, in dBFS.
pub const FLOOR_DB: f32 = -100.0;
const FFT_SIZE: usize = 4096;
const LOW_HZ: f32 = 20.0;
const HIGH_HZ: f32 = 20_000.0;
/// How often the analyzer looks at the rings and computes a new spectrum: as often as the
/// chain reports, so the window gets a fresh one every time.
const EVERY: Duration = Duration::from_millis(16);

/// The callback's end.
pub struct SpectrumTap {
    input: Producer<f32>,
    output: Producer<f32>,
}

impl SpectrumTap {
    /// Real-time: the input's mono mix, before the slots.
    pub fn push_input(&mut self, io: &[&mut [f32]]) {
        push_mono(&mut self.input, io);
    }

    /// Real-time: the output's mono mix, after the slots.
    pub fn push_output(&mut self, io: &[&mut [f32]]) {
        push_mono(&mut self.output, io);
    }
}

/// The mean of the first two channels, as much as fits; the rest is dropped (the analyzer
/// only wants the latest samples anyway).
fn push_mono(ring: &mut Producer<f32>, io: &[&mut [f32]]) {
    let (Some(left), right) = (io.first(), io.get(1)) else {
        return;
    };
    let right: &[f32] = right.map_or(left, |r| r);
    let frames = left.len().min(right.len()).min(ring.slots());
    if let Ok(chunk) = ring.write_chunk_uninit(frames) {
        chunk.fill_from_iter(
            left[..frames]
                .iter()
                .zip(&right[..frames])
                .map(|(l, r)| (l + r) * 0.5),
        );
    }
}

/// The latest spectra, band levels in dBFS as f32 bits: input then output.
pub type Bands = [[f32; BANDS]; 2];

/// The analyzer thread and what it found.
pub struct Analyzer {
    latest: Arc<[AtomicU32; 2 * BANDS]>,
    stop: Arc<AtomicBool>,
}

impl Analyzer {
    /// Starts the analyzer for a stream at `rate`; the tap goes to the callback.
    pub fn start(rate: u32) -> std::io::Result<(Analyzer, SpectrumTap)> {
        let capacity = (rate as usize / 2).max(FFT_SIZE * 2);
        let (input_tx, input_rx) = RingBuffer::new(capacity);
        let (output_tx, output_rx) = RingBuffer::new(capacity);
        let latest: Arc<[AtomicU32; 2 * BANDS]> =
            Arc::new(std::array::from_fn(|_| AtomicU32::new(FLOOR_DB.to_bits())));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopping) = (Arc::clone(&latest), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("spectrum".into())
            .spawn(move || analyze(rate, [input_rx, output_rx], &shared, &stopping))?;
        Ok((
            Analyzer { latest, stop },
            SpectrumTap {
                input: input_tx,
                output: output_tx,
            },
        ))
    }

    pub fn bands(&self) -> Bands {
        std::array::from_fn(|side| {
            std::array::from_fn(|band| {
                f32::from_bits(self.latest[side * BANDS + band].load(Ordering::Relaxed))
            })
        })
    }
}

impl Drop for Analyzer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn analyze(
    rate: u32,
    mut rings: [Consumer<f32>; 2],
    latest: &[AtomicU32; 2 * BANDS],
    stop: &AtomicBool,
) {
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos())
        .collect();
    let edges = band_edges(rate);
    // A circular buffer per side: `at` is where the next sample goes, so the oldest.
    let mut history = [vec![0.0f32; FFT_SIZE], vec![0.0f32; FFT_SIZE]];
    let mut at = [0usize; 2];
    let mut scratch = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    while !stop.load(Ordering::Relaxed) {
        for (side, ring) in rings.iter_mut().enumerate() {
            let ready = ring.slots();
            if let Ok(chunk) = ring.read_chunk(ready) {
                let (a, b) = chunk.as_slices();
                for sample in a.iter().chain(b) {
                    history[side][at[side]] = *sample;
                    at[side] = (at[side] + 1) % FFT_SIZE;
                }
                chunk.commit_all();
            }
            let (newer, older) = history[side].split_at(at[side]);
            for ((x, h), w) in scratch
                .iter_mut()
                .zip(older.iter().chain(newer))
                .zip(&window)
            {
                *x = h * w;
            }
            if fft.process(&mut scratch, &mut spectrum).is_ok() {
                for (band, level) in fold(&spectrum, &edges).iter().enumerate() {
                    latest[side * BANDS + band].store(level.to_bits(), Ordering::Relaxed);
                }
            }
        }
        std::thread::sleep(EVERY);
    }
    // The callback may still hold the tap: keep the rings until it lets go.
    while !rings.iter().all(Consumer::is_abandoned) {
        std::thread::sleep(EVERY);
    }
}

/// The FFT bin where each band starts, `BANDS + 1` edges spaced evenly in log frequency.
fn band_edges(rate: u32) -> Vec<usize> {
    let hz_per_bin = rate as f32 / FFT_SIZE as f32;
    let ratio = (HIGH_HZ / LOW_HZ).powf(1.0 / BANDS as f32);
    (0..=BANDS)
        .map(|i| ((LOW_HZ * ratio.powi(i as i32)) / hz_per_bin).round() as usize)
        .map(|bin| bin.clamp(1, FFT_SIZE / 2))
        .collect()
}

/// Each band's level in dBFS: the strongest bin in it (a band narrower than a bin takes
/// the bin it falls in), scaled so a full-scale sine reads 0 dB.
fn fold(spectrum: &[realfft::num_complex::Complex<f32>], edges: &[usize]) -> [f32; BANDS] {
    // Hann's coherent gain is 0.5; a real sine's energy splits between two halves.
    let scale = 4.0 / FFT_SIZE as f32;
    std::array::from_fn(|band| {
        let (start, end) = (edges[band], edges[band + 1].max(edges[band] + 1));
        let peak = spectrum[start.min(spectrum.len() - 1)..end.min(spectrum.len())]
            .iter()
            .map(|c| c.norm())
            .fold(0.0f32, f32::max);
        (20.0 * (peak * scale).max(1e-9).log10()).max(FLOOR_DB)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_lights_the_band_it_is_in_at_its_level() {
        let rate = 48_000;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let mut input: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let hann = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos();
                0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / rate as f32).sin() * hann
            })
            .collect();
        let mut spectrum = fft.make_output_vec();
        fft.process(&mut input, &mut spectrum).unwrap();
        let edges = band_edges(rate);
        let bands = fold(&spectrum, &edges);
        let (loudest, level) = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        let ratio = (HIGH_HZ / LOW_HZ).powf(1.0 / BANDS as f32);
        let low = LOW_HZ * ratio.powi(loudest as i32);
        assert!(
            low <= 1000.0 && 1000.0 < low * ratio * 1.1,
            "1 kHz in band {loudest} from {low} Hz"
        );
        assert!(
            (level - (-6.0)).abs() < 1.5,
            "a half-scale sine reads about -6 dB: {level}"
        );
        assert!(bands[0] < -60.0, "20 Hz is quiet: {}", bands[0]);
    }

    #[test]
    fn the_edges_climb_from_20_hz_to_20_khz() {
        let edges = band_edges(48_000);
        assert_eq!(edges.len(), BANDS + 1);
        assert!(edges.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(
            *edges.last().unwrap(),
            (20_000.0 / (48_000.0 / FFT_SIZE as f32)).round() as usize
        );
    }
}
