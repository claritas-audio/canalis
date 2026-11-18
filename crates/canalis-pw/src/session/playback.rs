//! Playing a recording back to the default output (the mic check): the WAV the chain
//! recorder writes (32-bit float, interleaved) is read whole and fed to a playback stream
//! that WirePlumber links like any app's. It runs on the loop thread; it is not the audio
//! plane and needs no real-time care beyond not blocking.

use std::cell::Cell;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use pipewire::core::CoreRc;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::sys;
use pipewire::spa::utils::Direction;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};

/// A WAV's samples: interleaved float, its channel count and rate.
#[derive(Debug, PartialEq)]
pub(super) struct Sound {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub rate: u32,
}

/// Reads a 32-bit float WAV (format 3), as `canalis_chain::recorder` writes it.
pub(super) fn read_wav(bytes: &[u8]) -> Result<Sound, String> {
    let u16_at = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..16) != Some(b"WAVEfmt ") {
        return Err("not a WAV file".into());
    }
    let (Some(format), Some(channels), Some(rate), Some(bits)) =
        (u16_at(20), u16_at(22), u32_at(24), u16_at(34))
    else {
        return Err("a WAV header too short".into());
    };
    if format != 3 || bits != 32 || channels == 0 {
        return Err(format!(
            "not 32-bit float audio (format {format}, {bits} bits)"
        ));
    }
    let data = bytes.get(44..).ok_or("no audio in the file")?;
    let samples = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    Ok(Sound {
        samples,
        channels,
        rate,
    })
}

/// A recording playing; `done` once its last sample has gone out.
pub(super) struct Playback {
    pub path: PathBuf,
    /// Delete the file once it has played (a mic check's take).
    pub delete_after: bool,
    done: Rc<Cell<bool>>,
    _listener: StreamListener<Feed>,
    _stream: StreamRc,
}

impl Playback {
    pub fn done(&self) -> bool {
        self.done.get()
    }
}

/// What the stream's callback reads from.
struct Feed {
    sound: Sound,
    at: usize,
    done: Rc<Cell<bool>>,
}

/// Starts `path` playing to the default output.
pub(super) fn play(core: &CoreRc, path: &Path, delete_after: bool) -> Result<Playback, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sound = read_wav(&bytes)?;
    let props = pipewire::properties::properties! {
        "node.name" => "canalis-playback",
        "node.description" => "Canalis mic check",
        "media.type" => "Audio",
        "media.category" => "Playback",
        "media.role" => "Notification",
    };
    let stream =
        StreamRc::new(core.clone(), "canalis-playback", props).map_err(|e| e.to_string())?;
    let done = Rc::new(Cell::new(false));
    let format = format_pod(sound.channels, sound.rate);
    let feed = Feed {
        sound,
        at: 0,
        done: Rc::clone(&done),
    };
    let listener = stream
        .add_local_listener_with_user_data(feed)
        .process(|stream, feed| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let channels = usize::from(feed.sound.channels);
            let requested = buffer.requested() as usize;
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let stride = channels * size_of::<f32>();
            let room = data.as_raw().maxsize as usize / stride;
            let frames = if requested == 0 {
                room
            } else {
                requested.min(room)
            };
            let left = (feed.sound.samples.len() - feed.at) / channels;
            let n = frames.min(left);
            if let Some(bytes) = data.data() {
                let out = &feed.sound.samples[feed.at..feed.at + n * channels];
                for (dst, sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(out) {
                    *dst = sample.to_le_bytes();
                }
            }
            feed.at += n * channels;
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (n * stride) as u32;
            if n == 0 {
                feed.done.set(true);
            }
        })
        .register()
        .map_err(|e| e.to_string())?;
    let mut params = [Pod::from_bytes(&format).ok_or("a bad format pod")?];
    stream
        .connect(
            Direction::Output,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| e.to_string())?;
    Ok(Playback {
        path: path.to_path_buf(),
        delete_after,
        done,
        _listener: listener,
        _stream: stream,
    })
}

/// Interleaved float at the file's rate, front left and right (mono as mono).
pub(super) fn format_pod(channels: u16, rate: u32) -> Vec<u8> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(u32::from(channels));
    let mut position = [0; MAX_CHANNELS];
    if channels >= 2 {
        position[0] = sys::SPA_AUDIO_CHANNEL_FL;
        position[1] = sys::SPA_AUDIO_CHANNEL_FR;
    } else {
        position[0] = sys::SPA_AUDIO_CHANNEL_MONO;
    }
    info.set_position(position);
    let object = Object {
        type_: sys::SPA_TYPE_OBJECT_Format,
        id: sys::SPA_PARAM_EnumFormat,
        properties: info.into(),
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .expect("a format pod serializes")
        .0
        .into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: &[f32], channels: u16, rate: u32, format: u16) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&format.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * u32::from(channels) * 4).to_le_bytes());
        b.extend_from_slice(&(channels * 4).to_le_bytes());
        b.extend_from_slice(&32u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn a_float_wav_reads_back_its_samples() {
        let sound = read_wav(&wav(&[0.5, -0.25, 0.125, 0.0], 2, 48_000, 3)).expect("reads");
        assert_eq!(sound.channels, 2);
        assert_eq!(sound.rate, 48_000);
        assert_eq!(sound.samples, [0.5, -0.25, 0.125, 0.0]);
        assert!(read_wav(&wav(&[0.0], 1, 48_000, 1)).is_err(), "integer PCM");
        assert!(read_wav(b"not a wav").is_err());
    }
}
