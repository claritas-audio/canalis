//! Recording a chain's output to a WAV file. The callback's half, `Tap`, only copies the
//! main pair into a lock-free ring; a writer thread drains it into a 32-bit float WAV and
//! patches the header's sizes when told to stop (or when the tap is dropped). The writer
//! then keeps its end of the ring until the callback has dropped the tap, so the ring is
//! always freed on the writer's thread, never in the callback.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

/// Seconds of audio the ring holds while the writer is late.
const RING_SECONDS: usize = 2;
const CHANNELS: u16 = 2;
/// How long the writer sleeps when the ring is empty.
const WRITER_NAP: Duration = Duration::from_millis(20);
/// How long a quitting chain waits for its recording to be completed.
const FINISH_WAIT: Duration = Duration::from_secs(5);

/// The callback's end: interleaved stereo samples into the ring.
pub struct Tap {
    ring: Producer<f32>,
    dropped: Arc<AtomicU64>,
}

impl Tap {
    /// Real-time: copies `left` and `right` (at most what fits), counts frames that did not.
    pub fn push(&mut self, left: &[f32], right: &[f32]) {
        let frames = left.len().min(right.len());
        let fits = (self.ring.slots() / CHANNELS as usize).min(frames);
        if let Ok(chunk) = self.ring.write_chunk_uninit(fits * CHANNELS as usize) {
            chunk.fill_from_iter(
                left[..fits]
                    .iter()
                    .zip(&right[..fits])
                    .flat_map(|(l, r)| [*l, *r]),
            );
        }
        if fits < frames {
            self.dropped
                .fetch_add((frames - fits) as u64, Ordering::Relaxed);
        }
    }
}

/// The control side's end: how far the recording got, and how it ended.
pub struct Recorder {
    pub path: PathBuf,
    rate: u32,
    frames: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    /// The writer drains what is there and completes the file, tap or not.
    stop: Arc<AtomicBool>,
    /// How the file ended, once it is complete.
    done: mpsc::Receiver<io::Result<()>>,
}

impl Recorder {
    /// Opens `path` and starts the writer; the tap goes to the callback.
    pub fn start(path: PathBuf, rate: u32) -> io::Result<(Recorder, Tap)> {
        let file = BufWriter::new(File::create(&path)?);
        let (producer, consumer) =
            RingBuffer::new(rate as usize * RING_SECONDS * CHANNELS as usize);
        let frames = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (written, stopping) = (Arc::clone(&frames), Arc::clone(&stop));
        let (done_tx, done) = mpsc::channel();
        std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || {
                let mut ring = consumer;
                let _ = done_tx.send(write(file, &mut ring, rate, &written, &stopping));
                // The callback may still hold the tap; keep the ring alive until it lets
                // go, so the last reference (and the free) is this thread's.
                while !ring.is_abandoned() {
                    std::thread::sleep(WRITER_NAP);
                }
            })?;
        Ok((
            Recorder {
                path,
                rate,
                frames,
                dropped: Arc::clone(&dropped),
                stop,
                done,
            },
            Tap {
                ring: producer,
                dropped,
            },
        ))
    }

    pub fn seconds(&self) -> f32 {
        self.frames.load(Ordering::Relaxed) as f32 / self.rate.max(1) as f32
    }

    /// Frames lost because the writer fell two seconds behind.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Asks the writer to complete the file with what has arrived; `finished` reports it.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Completes the file and waits for it (the chain is quitting).
    pub fn finish(self) -> io::Result<()> {
        self.stop();
        self.done
            .recv_timeout(FINISH_WAIT)
            .unwrap_or_else(|_| Err(io::Error::other("the recorder did not finish")))
    }

    /// The file is complete: how it went; `None` while recording.
    pub fn finished(&self) -> Option<io::Result<()>> {
        self.done.try_recv().ok()
    }
}

/// Drains `ring` into `file` until the tap is gone and the ring is empty, then writes the
/// final sizes into the header.
fn write(
    mut file: BufWriter<File>,
    ring: &mut Consumer<f32>,
    rate: u32,
    frames: &AtomicU64,
    stop: &AtomicBool,
) -> io::Result<()> {
    file.write_all(&header(rate, 0))?;
    let mut samples: u64 = 0;
    loop {
        let ready = ring.slots();
        if ready == 0 {
            if ring.is_abandoned() || stop.load(Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(WRITER_NAP);
            continue;
        }
        let chunk = ring.read_chunk(ready).map_err(io::Error::other)?;
        let (a, b) = chunk.as_slices();
        for sample in a.iter().chain(b) {
            file.write_all(&sample.to_le_bytes())?;
        }
        chunk.commit_all();
        samples += ready as u64;
        frames.store(samples / u64::from(CHANNELS), Ordering::Relaxed);
    }
    let data_bytes = u32::try_from(samples * 4).unwrap_or(u32::MAX);
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(rate, data_bytes))?;
    file.flush()
}

/// A 44-byte WAV header for stereo 32-bit float at `rate`, `data_bytes` of samples.
fn header(rate: u32, data_bytes: u32) -> [u8; 44] {
    let block = CHANNELS * 4;
    let mut h = [0u8; 44];
    let mut at = 0;
    let mut put = |bytes: &[u8]| {
        h[at..at + bytes.len()].copy_from_slice(bytes);
        at += bytes.len();
    };
    put(b"RIFF");
    put(&(36 + data_bytes).to_le_bytes());
    put(b"WAVEfmt ");
    put(&16u32.to_le_bytes());
    put(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    put(&CHANNELS.to_le_bytes());
    put(&rate.to_le_bytes());
    put(&(rate * u32::from(block)).to_le_bytes());
    put(&block.to_le_bytes());
    put(&32u16.to_le_bytes());
    put(b"data");
    put(&data_bytes.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_is_a_float_wav_with_its_sizes_patched() {
        let path = std::env::temp_dir().join(format!("canalis-rec-{}.wav", std::process::id()));
        let (recorder, mut tap) = Recorder::start(path.clone(), 48_000).expect("starts");
        let left = vec![0.5f32; 480];
        let right = vec![-0.25f32; 480];
        tap.push(&left, &right);
        tap.push(&left, &right);
        recorder.stop();
        let result = loop {
            if let Some(result) = recorder.finished() {
                break result;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        result.expect("written");
        drop(tap);
        assert_eq!(recorder.seconds(), 960.0 / 48_000.0);
        assert_eq!(recorder.dropped(), 0);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 960 * 2 * 4);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[20..22], &3u16.to_le_bytes(), "IEEE float");
        assert_eq!(
            &bytes[40..44],
            &(960u32 * 8).to_le_bytes(),
            "data size patched"
        );
        assert_eq!(&bytes[44..48], &0.5f32.to_le_bytes(), "left first");
        assert_eq!(&bytes[48..52], &(-0.25f32).to_le_bytes());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_full_ring_counts_what_it_could_not_take() {
        let (producer, _consumer) = RingBuffer::new(8);
        let mut tap = Tap {
            ring: producer,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        tap.push(&[0.0; 6], &[0.0; 6]);
        assert_eq!(
            tap.dropped.load(Ordering::Relaxed),
            2,
            "4 frames fit in 8 slots"
        );
    }
}
