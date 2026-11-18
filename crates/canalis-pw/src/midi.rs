//! The control plane's MIDI input: one `application/control` stream, `canalis.midi`,
//! that the user's controller is linked into. Its buffers are `spa_pod_sequence`s of
//! UMP packets (PipeWire ≥ 1.4) or raw MIDI bytes; both become `MidiEvent`s here.
//! The stream's process callback runs on the loop thread, so no real-time rules apply.

use std::io::Cursor;

use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Property, PropertyFlags, Value};
use pipewire::spa::sys;

/// `SPA_CONTROL_UMP`, spelled out: the headers of PipeWire before 1.2 (Ubuntu 24.04 has
/// 1.0.5) do not name it, and a daemon that old never sends it.
const CONTROL_UMP: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiEvent {
    Cc { channel: u8, cc: u8, value: u8 },
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
}

/// The `EnumFormat` pod of a control stream.
pub(crate) fn control_format_pod() -> Vec<u8> {
    let object = Object {
        type_: sys::SPA_TYPE_OBJECT_Format,
        id: sys::SPA_PARAM_EnumFormat,
        properties: vec![
            Property {
                key: sys::SPA_FORMAT_mediaType,
                flags: PropertyFlags::empty(),
                value: Value::Id(pipewire::spa::utils::Id(sys::SPA_MEDIA_TYPE_application)),
            },
            Property {
                key: sys::SPA_FORMAT_mediaSubtype,
                flags: PropertyFlags::empty(),
                value: Value::Id(pipewire::spa::utils::Id(sys::SPA_MEDIA_SUBTYPE_control)),
            },
        ],
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .expect("a format pod serializes")
        .0
        .into_inner()
}

/// The events in one buffer's `spa_pod_sequence`, in order.
pub(crate) fn parse_sequence(bytes: &[u8]) -> Vec<MidiEvent> {
    let mut events = Vec::new();
    let u32_at = |at: usize| -> Option<u32> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (Some(size), Some(type_)) = (u32_at(0), u32_at(4)) else {
        return events;
    };
    if type_ != sys::SPA_TYPE_Sequence {
        return events;
    }
    let end = (8 + size as usize).min(bytes.len());
    // After the sequence header come the controls: offset, type, then a pod.
    let mut at = 16;
    while at + 16 <= end {
        let (Some(control_type), Some(value_size), Some(value_type)) =
            (u32_at(at + 4), u32_at(at + 8), u32_at(at + 12))
        else {
            break;
        };
        let value_size = value_size as usize;
        let body = &bytes[at + 16..(at + 16 + value_size).min(end)];
        if value_type == sys::SPA_TYPE_Bytes {
            match control_type {
                CONTROL_UMP => events.extend(ump_events(body)),
                sys::SPA_CONTROL_Midi => events.extend(midi1_events(body)),
                _ => {}
            }
        }
        at += 16 + value_size.div_ceil(8) * 8;
    }
    events
}

/// Universal MIDI Packets: 32-bit words; only MIDI 1.0 channel voice messages matter.
fn ump_events(body: &[u8]) -> Vec<MidiEvent> {
    let mut events = Vec::new();
    let mut words = body
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_ne_bytes(*w));
    while let Some(word) = words.next() {
        let message_type = word >> 28;
        let words_in_packet = match message_type {
            0x0 | 0x1 | 0x2 | 0x6 | 0x7 => 1,
            0x3 | 0x4 | 0x8 | 0x9 | 0xA => 2,
            0xB | 0xC => 3,
            _ => 4,
        };
        if message_type == 0x2 {
            let status = ((word >> 16) & 0xff) as u8;
            let data1 = ((word >> 8) & 0x7f) as u8;
            let data2 = (word & 0x7f) as u8;
            events.extend(channel_voice(status, data1, data2));
        }
        for _ in 1..words_in_packet {
            words.next();
        }
    }
    events
}

/// Raw MIDI 1.0 bytes, status then data, running status not handled.
fn midi1_events(body: &[u8]) -> Vec<MidiEvent> {
    let mut events = Vec::new();
    let mut at = 0;
    while at + 3 <= body.len() {
        let status = body[at];
        if status & 0x80 == 0 {
            at += 1;
            continue;
        }
        events.extend(channel_voice(
            status,
            body[at + 1] & 0x7f,
            body[at + 2] & 0x7f,
        ));
        at += 3;
    }
    events
}

fn channel_voice(status: u8, data1: u8, data2: u8) -> Option<MidiEvent> {
    let channel = status & 0x0f;
    match status & 0xf0 {
        0xB0 => Some(MidiEvent::Cc {
            channel,
            cc: data1,
            value: data2,
        }),
        0x90 if data2 > 0 => Some(MidiEvent::NoteOn {
            channel,
            note: data1,
            velocity: data2,
        }),
        0x90 | 0x80 => Some(MidiEvent::NoteOff {
            channel,
            note: data1,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence(control_type: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let padded = body.len().div_ceil(8) * 8;
        let size = 8 + 16 + padded;
        bytes.extend_from_slice(&(size as u32).to_ne_bytes());
        bytes.extend_from_slice(&sys::SPA_TYPE_Sequence.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&control_type.to_ne_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_ne_bytes());
        bytes.extend_from_slice(&sys::SPA_TYPE_Bytes.to_ne_bytes());
        bytes.extend_from_slice(body);
        bytes.resize(bytes.len() + padded - body.len(), 0);
        bytes
    }

    #[test]
    fn ump_and_raw_midi_both_become_events() {
        let cc: u32 = (0x2 << 28) | (0xB1 << 16) | (7 << 8) | 100;
        let note: u32 = (0x2 << 28) | (0x90 << 16) | (60 << 8) | 90;
        let mut body = Vec::new();
        body.extend_from_slice(&cc.to_ne_bytes());
        body.extend_from_slice(&note.to_ne_bytes());
        assert_eq!(
            parse_sequence(&sequence(CONTROL_UMP, &body)),
            vec![
                MidiEvent::Cc {
                    channel: 1,
                    cc: 7,
                    value: 100
                },
                MidiEvent::NoteOn {
                    channel: 0,
                    note: 60,
                    velocity: 90
                }
            ]
        );
        assert_eq!(
            parse_sequence(&sequence(
                sys::SPA_CONTROL_Midi,
                &[0xB0, 1, 127, 0x80, 60, 0]
            )),
            vec![
                MidiEvent::Cc {
                    channel: 0,
                    cc: 1,
                    value: 127
                },
                MidiEvent::NoteOff {
                    channel: 0,
                    note: 60
                }
            ]
        );
        assert!(parse_sequence(&[1, 2, 3]).is_empty());
        assert!(!control_format_pod().is_empty());
    }
}
