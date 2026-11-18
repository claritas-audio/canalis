//! The Claritas CLAPs Canalis ships (ADR 0007): their ids, and an EQ curve written as
//! Claritas EQ parameters for slots saved before the built-in DSP was dropped (config
//! migration 1 → 2). Claritas is a nice-plug plugin: a parameter's CLAP id is a
//! hash of its string id (`freq_3` for band 3's frequency), and its CLAP value is the
//! normalized value (step index for switches and choices).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const EQ_ID: &str = "audio.claritas.eq";
pub const COMP_ID: &str = "audio.claritas.comp";
/// Bands Claritas EQ has, `claritas_core::eq::MAX_BANDS`.
pub const EQ_BANDS: usize = 24;

/// One EQ band, as the old built-in EQ saved it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub shape: EqShape,
    pub freq_hz: f32,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default = "default_q")]
    pub q: f32,
    /// Cut slope in dB/oct: 6, 12, 18, 24, 30, 36, 48, 72 or 96; others read as 12.
    #[serde(default = "default_slope")]
    pub slope_db: u16,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn default_q() -> f32 {
    0.707
}

fn default_slope() -> u16 {
    12
}

fn enabled() -> bool {
    true
}

/// In Claritas EQ's `Shape` order, which is its step index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EqShape {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
    TiltShelf,
}

impl EqBand {
    pub fn bell(freq_hz: f32, gain_db: f32, q: f32) -> Self {
        EqBand {
            shape: EqShape::Bell,
            freq_hz,
            gain_db,
            q,
            slope_db: 12,
            enabled: true,
        }
    }

    pub fn cut(shape: EqShape, freq_hz: f32, slope_db: u16) -> Self {
        EqBand {
            shape,
            freq_hz,
            gain_db: 0.0,
            q: 0.707,
            slope_db,
            enabled: true,
        }
    }
}

/// Claritas EQ's slope choices, in step order.
const SLOPES: [u16; 9] = [6, 12, 18, 24, 30, 36, 48, 72, 96];

/// nice-plug's CLAP parameter id for a string id.
pub fn param_id(id: &str) -> u32 {
    let hash = id.bytes().fold(0u32, |hash, b| {
        hash.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    hash & !(1 << 31)
}

/// nice-plug's `FloatRange::Skewed` normalization with Claritas's factor, 2^-2.
fn skewed(value: f32, min: f32, max: f32) -> f64 {
    f64::from(((value.clamp(min, max) - min) / (max - min)).powf(0.25))
}

/// Claritas EQ's output gain range, ±36 dB (nice-plug's `FloatRange::gain_range`).
const OUTPUT_DB: f32 = 36.0;

/// `db` as Claritas EQ's output gain value: nice-plug's gain range is skewed so that the
/// middle of the dB range sits at 0.5.
fn output_gain(db: f32) -> f64 {
    let gain = |db: f32| 10f32.powf(db / 20.0);
    let (min, max) = (gain(-OUTPUT_DB), gain(OUTPUT_DB));
    let factor = 0.5f32.log((gain(0.0) - min) / (max - min));
    f64::from(((gain(db.clamp(-OUTPUT_DB, OUTPUT_DB)) - min) / (max - min)).powf(factor))
}

/// A curve with its output gain (an AutoEQ preamp), as the slot's `params`.
pub fn curve_params(bands: &[EqBand], output_db: f32) -> BTreeMap<String, f64> {
    let mut params = eq_params(bands);
    params.insert(param_id("output_gain").to_string(), output_gain(output_db));
    params
}

/// `bands` as Claritas EQ parameter values by CLAP id, the slot's `params`; bands past
/// the plugin's 24 are dropped, the rest stay at the plugin's defaults (off).
pub fn eq_params(bands: &[EqBand]) -> BTreeMap<String, f64> {
    let mut params = BTreeMap::new();
    for (index, band) in bands.iter().take(EQ_BANDS).enumerate() {
        let n = index + 1;
        let slope = SLOPES.iter().position(|s| *s == band.slope_db).unwrap_or(1);
        let values = [
            ("enabled", if band.enabled { 1.0 } else { 0.0 }),
            ("kind", band.shape as u8 as f64),
            ("freq", skewed(band.freq_hz, 10.0, 30_000.0)),
            (
                "gain",
                f64::from((band.gain_db.clamp(-30.0, 30.0) + 30.0) / 60.0),
            ),
            ("q", skewed(band.q, 0.025, 40.0)),
            ("slope", slope as f64),
            ("place", 0.0),
        ];
        for (name, value) in values {
            params.insert(param_id(&format!("{name}_{n}")).to_string(), value);
        }
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_hash_as_nice_plug_does() {
        // h = h * 31 + byte over "q_1", top bit cleared.
        let expected = (((u32::from(b'q') * 31) + u32::from(b'_')) * 31) + u32::from(b'1');
        assert_eq!(param_id("q_1"), expected);
        assert!(param_id("output_gain") < 1 << 31);
    }

    #[test]
    fn a_band_becomes_its_seven_parameters() {
        let params = eq_params(&[EqBand::cut(EqShape::LowCut, 80.0, 24)]);
        assert_eq!(params.len(), 7);
        let get = |name: &str| params[&param_id(name).to_string()];
        assert_eq!(get("enabled_1"), 1.0);
        assert_eq!(get("kind_1"), 3.0, "low cut is the fourth shape");
        assert_eq!(get("slope_1"), 3.0, "24 dB/oct is the fourth slope");
        assert_eq!(get("gain_1"), 0.5, "0 dB is the middle of ±30");
        let freq = get("freq_1");
        let back = 10.0 + freq.powf(4.0) * (30_000.0 - 10.0);
        assert!((back - 80.0).abs() < 0.01, "80 Hz came back as {back}");
    }

    #[test]
    fn output_gain_is_centred_on_0_db() {
        assert!(
            (output_gain(0.0) - 0.5).abs() < 1e-4,
            "{}",
            output_gain(0.0)
        );
        assert_eq!(output_gain(-36.0), 0.0);
        assert!((output_gain(36.0) - 1.0).abs() < 1e-6);
        assert!(output_gain(-6.3) < 0.5);
        let params = curve_params(&[], -6.3);
        assert_eq!(params.len(), 1, "only the gain without bands");
    }

    #[test]
    fn only_the_plugins_bands_are_written() {
        let bands = vec![EqBand::bell(1000.0, 0.0, 1.0); 30];
        assert_eq!(eq_params(&bands).len(), 7 * EQ_BANDS);
    }
}
