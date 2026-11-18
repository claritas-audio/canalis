//! Sound files into memory: any format symphonia reads (WAV, FLAC, MP3, Ogg Vorbis), as
//! stereo at `RATE`. Runs on a thread of its own, never in the callback.

use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use crate::RATE;
use crate::resample::resample;

/// Pads are for short sounds and beds; a longer file would sit whole in memory.
pub const MAX_SECONDS: usize = 300;

/// A decoded sound: stereo frames at `RATE`.
#[derive(Debug, PartialEq)]
pub struct Sound {
    pub frames: Vec<[f32; 2]>,
}

impl Sound {
    pub fn seconds(&self) -> f32 {
        self.frames.len() as f32 / RATE as f32
    }
}

/// The whole file: mono is doubled, more channels keep their first two.
pub fn decode(path: &Path) -> Result<Sound, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let source = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|_| "not an audio file Canalis can read".to_owned())?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio in the file")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio in the file")?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    let mut block = Vec::new();
    let mut rate = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged packet is skipped, as players do.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        rate = decoded.spec().rate();
        let channels = decoded.spec().channels().count().max(1);
        decoded.copy_to_vec_interleaved(&mut block);
        frames.extend(
            block
                .chunks_exact(channels)
                .map(|f| [f[0], f.get(1).copied().unwrap_or(f[0])]),
        );
        if frames.len() > MAX_SECONDS * rate as usize {
            return Err(format!(
                "longer than {} minutes; pads are for short sounds",
                MAX_SECONDS / 60
            ));
        }
    }
    if frames.is_empty() || rate == 0 {
        return Err("no audio in the file".into());
    }
    Ok(Sound {
        frames: resample(&frames, rate, RATE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit PCM WAV, as most sound files on disk are.
    fn wav16(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn a_mono_wav_decodes_to_stereo_at_the_engine_rate() {
        let dir = std::env::temp_dir().join(format!("canalis-pads-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ding.wav");
        std::fs::write(&path, wav16(&[16384, -16384, 0, 0], 1, RATE)).unwrap();
        let sound = decode(&path).expect("decodes");
        assert_eq!(
            sound.frames,
            [[0.5, 0.5], [-0.5, -0.5], [0.0, 0.0], [0.0, 0.0]]
        );
        std::fs::write(&path, wav16(&[0; 22_050], 1, 22_050)).unwrap();
        assert_eq!(decode(&path).expect("decodes").frames.len(), 48_000);
        std::fs::write(&path, b"not audio").unwrap();
        assert!(decode(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
