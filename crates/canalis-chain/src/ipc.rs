//! What crosses the pipe between the control plane and a chain process: one JSON object
//! per line, control → chain on the child's stdin, chain → control on its stdout.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::clap::SlotInfo;
use crate::meters::METER_CHANNELS;
use crate::slot::SlotConfig;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToChain {
    /// The whole slot list; replaces every runner.
    Slots {
        slots: Vec<SlotConfig>,
    },
    Bypass {
        slot: usize,
        bypass: bool,
    },
    Mix {
        slot: usize,
        mix: f32,
    },
    /// One CLAP parameter, by the plugin's param id.
    Param {
        slot: usize,
        id: u32,
        value: f64,
    },
    /// Open or close the plugin's own window.
    Gui {
        slot: usize,
        open: bool,
    },
    /// Recall a factory preset: the slot is rebuilt from it, dropping its state and params.
    Preset {
        slot: usize,
        uri: String,
    },
    /// Whether the chain's key input (`<name>.key`) exists; it feeds every slot whose
    /// plugin has a sidechain port.
    Key {
        enabled: bool,
    },
    /// Send a click through the hardware insert in this slot and time its return.
    Measure {
        slot: usize,
    },
    /// One lane's gain of a parallel group slot, in dB (≤ −60 is off).
    LaneGain {
        slot: usize,
        lane: usize,
        gain_db: f32,
    },
    LaneMute {
        slot: usize,
        lane: usize,
        mute: bool,
    },
    /// One lane's balance, −1 (left) to 1 (right).
    LanePan {
        slot: usize,
        lane: usize,
        pan: f32,
    },
    /// The chain's output fades to silence (or back) over about 10 ms.
    Mute {
        mute: bool,
    },
    /// Forget the integrated loudness and the true-peak hold.
    ResetLoudness,
    /// Analyze the input and output spectrum (while a window shows it), or stop.
    Spectrum {
        on: bool,
    },
    /// Steer the output toward `target` LUFS, or stop with `None`.
    AutoLevel {
        target: Option<f32>,
    },
    /// Record the output to a WAV file at `path`, or stop with `None`.
    Record {
        path: Option<String>,
    },
    /// How many destination pairs the playback end carries and each one's gain in dB;
    /// the first is the main pair. A count change reconnects the playback stream.
    Sends {
        count: usize,
        gains_db: Vec<f32>,
    },
    Quit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromChain {
    Up {
        node_name: String,
    },
    Format {
        rate: u32,
        channels: u32,
    },
    /// Linear peaks since the previous report, and the output loudness: momentary,
    /// short-term and integrated LUFS plus true peak dBTP (−200 for silence).
    Meters {
        input: [f32; METER_CHANNELS],
        output: [f32; METER_CHANNELS],
        #[serde(default = "silent")]
        loudness: [f32; 4],
        /// Output balance over 400 ms, −1 (left) to 1 (right).
        #[serde(default)]
        balance: f32,
        /// What the automatic level adds, in dB; 0 when it is off.
        #[serde(default)]
        auto_gain_db: f32,
        /// While the spectrum is on: `spectrum::BANDS` input levels then as many output
        /// levels, dBFS, low to high.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spectrum: Option<Vec<f32>>,
    },
    Latency {
        samples: u32,
        /// Each top-level slot's share of `samples`, in order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        slots: Vec<u32>,
    },
    /// What a slot turned out to be once built: the plugin's name and params, or why it failed.
    SlotInfo {
        slot: usize,
        #[serde(flatten)]
        info: SlotInfo,
    },
    /// The plugin window's state changed, by our hand or the window manager's.
    Gui {
        slot: usize,
        open: bool,
    },
    /// The plugin's state to persist, base64, after its changes settled.
    State {
        slot: usize,
        state: String,
    },
    /// Control values a plugin's own UI changed, by id, to persist.
    Params {
        slot: usize,
        params: BTreeMap<String, f64>,
    },
    /// The hardware insert's round trip in samples, or `None` when the click never came back.
    Measured {
        slot: usize,
        samples: Option<u32>,
    },
    /// The callback bypassed this slot: the chain kept overrunning its cycle and the slot
    /// took the most of it. Times in µs.
    Overload {
        slot: usize,
        slot_us: u64,
        cycle_us: u64,
    },
    /// The chain's peak DSP load over the last half second, % of the audio cycle.
    Load {
        percent: f32,
    },
    /// A recording's progress, about twice a second, and once more when it is `done`
    /// (with `error` if the file could not be written).
    Recording {
        path: String,
        seconds: f32,
        dropped: u64,
        done: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Written by the fatal-signal handler (`crash`), never by `report`: the slot the
    /// callback was in when the process died, none when it died outside the slots.
    Crash {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<usize>,
    },
}

fn silent() -> [f32; 4] {
    [crate::loudness::SILENCE; 4]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_tagged_json_lines() {
        let text = serde_json::to_string(&ToChain::Bypass {
            slot: 2,
            bypass: true,
        })
        .expect("serializes");
        assert_eq!(text, r#"{"type":"bypass","slot":2,"bypass":true}"#);
        let parsed: FromChain =
            serde_json::from_str(r#"{"type":"latency","samples":64}"#).expect("parses");
        assert_eq!(
            parsed,
            FromChain::Latency {
                samples: 64,
                slots: vec![]
            }
        );
    }

    #[test]
    fn slot_info_flattens_into_the_report() {
        let report = FromChain::SlotInfo {
            slot: 1,
            info: SlotInfo {
                name: "Claritas EQ".into(),
                params: vec![],
                error: Some("cannot load".into()),
                has_gui: false,
                presets: Vec::new(),
                sidechain: false,
            },
        };
        let text = serde_json::to_string(&report).expect("serializes");
        assert_eq!(
            text,
            r#"{"type":"slot_info","slot":1,"name":"Claritas EQ","params":[],"error":"cannot load","has_gui":false,"sidechain":false}"#
        );
        let param: ToChain =
            serde_json::from_str(r#"{"type":"param","slot":0,"id":42,"value":0.5}"#)
                .expect("parses");
        assert_eq!(
            param,
            ToChain::Param {
                slot: 0,
                id: 42,
                value: 0.5
            }
        );
    }
}
