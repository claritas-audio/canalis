//! Reading and writing the `Props` param (volumes and mute) and reading `Latency`.

use std::io::Cursor;

use pipewire::spa::pod::deserialize::PodDeserializer;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Pod, Property, PropertyFlags, Value, ValueArray};
use pipewire::spa::sys;

use crate::model::{Direction, Latency, Volume};

/// The volume a `Props` object carries; `None` when it has no `channelVolumes`.
pub(crate) fn volume_from_props(pod: &Pod) -> Option<Volume> {
    let (_, value) = PodDeserializer::deserialize_any_from(pod.as_bytes()).ok()?;
    let Value::Object(object) = value else {
        return None;
    };
    let mut channels = None;
    let mut mute = false;
    for property in object.properties {
        match (property.key, property.value) {
            (sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(v))) => {
                channels = Some(v);
            }
            (sys::SPA_PROP_mute, Value::Bool(m)) => mute = m,
            _ => {}
        }
    }
    Some(Volume {
        channels: channels?,
        mute,
    })
}

/// A port's `Latency` param: which way it counts (`Input` is upstream) and its upper bound.
pub(crate) fn latency_from_param(pod: &Pod) -> Option<(Direction, Latency)> {
    let (_, value) = PodDeserializer::deserialize_any_from(pod.as_bytes()).ok()?;
    let Value::Object(object) = value else {
        return None;
    };
    let mut direction = None;
    let mut latency = Latency::default();
    for property in object.properties {
        match (property.key, property.value) {
            (sys::SPA_PARAM_LATENCY_direction, Value::Id(id)) => {
                direction = Some(if id.0 == sys::SPA_DIRECTION_INPUT {
                    Direction::Input
                } else {
                    Direction::Output
                });
            }
            (sys::SPA_PARAM_LATENCY_maxQuantum, Value::Float(q)) => latency.quanta = q,
            (sys::SPA_PARAM_LATENCY_maxRate, Value::Int(r)) => latency.samples = r.max(0) as u32,
            (sys::SPA_PARAM_LATENCY_maxNs, Value::Long(ns)) => latency.ns = ns.max(0) as u64,
            _ => {}
        }
    }
    Some((direction?, latency))
}

/// A serialized `Props` object setting every channel to `linear`.
pub(crate) fn props_with_volume(linear: f32, channel_count: usize) -> Option<Vec<u8>> {
    props(
        sys::SPA_PROP_channelVolumes,
        Value::ValueArray(ValueArray::Float(vec![linear; channel_count.max(1)])),
    )
}

/// A serialized `Props` object muting or unmuting the node.
pub(crate) fn props_with_mute(mute: bool) -> Option<Vec<u8>> {
    props(sys::SPA_PROP_mute, Value::Bool(mute))
}

fn props(key: u32, value: Value) -> Option<Vec<u8>> {
    let object = Object {
        type_: sys::SPA_TYPE_OBJECT_Props,
        id: sys::SPA_PARAM_Props,
        properties: vec![Property {
            key,
            flags: PropertyFlags::empty(),
            value,
        }],
    };
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object)).ok()?;
    Some(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_latency_param_reads_its_upper_bound() {
        let object = Object {
            type_: sys::SPA_TYPE_OBJECT_ParamLatency,
            id: sys::SPA_PARAM_Latency,
            properties: [
                (
                    sys::SPA_PARAM_LATENCY_direction,
                    Value::Id(pipewire::spa::utils::Id(sys::SPA_DIRECTION_OUTPUT)),
                ),
                (sys::SPA_PARAM_LATENCY_minQuantum, Value::Float(1.0)),
                (sys::SPA_PARAM_LATENCY_maxQuantum, Value::Float(2.0)),
                (sys::SPA_PARAM_LATENCY_minRate, Value::Int(0)),
                (sys::SPA_PARAM_LATENCY_maxRate, Value::Int(480)),
                (sys::SPA_PARAM_LATENCY_minNs, Value::Long(0)),
                (sys::SPA_PARAM_LATENCY_maxNs, Value::Long(1_000_000)),
            ]
            .into_iter()
            .map(|(key, value)| Property {
                key,
                flags: PropertyFlags::empty(),
                value,
            })
            .collect(),
        };
        let (cursor, _) =
            PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object)).unwrap();
        let bytes = cursor.into_inner();
        let (direction, latency) =
            latency_from_param(Pod::from_bytes(&bytes).unwrap()).expect("a latency");
        assert_eq!(direction, Direction::Output);
        assert_eq!(
            latency,
            Latency {
                quanta: 2.0,
                samples: 480,
                ns: 1_000_000
            }
        );
        let clock = crate::model::Clock {
            rate: 48_000,
            quantum: 256,
        };
        assert_eq!(latency.in_samples(clock), 512.0 + 480.0 + 48.0);
    }

    #[test]
    fn a_volume_pod_round_trips() {
        let bytes = props_with_volume(0.125, 2).expect("serializes");
        let pod = Pod::from_bytes(&bytes).expect("a pod");
        let volume = volume_from_props(pod).expect("has volumes");
        assert_eq!(volume.channels, vec![0.125, 0.125]);
        assert!(!volume.mute);
    }

    #[test]
    fn a_mute_pod_carries_only_the_flag() {
        let bytes = props_with_mute(true).expect("serializes");
        let (_, value) = PodDeserializer::deserialize_any_from(&bytes).expect("a pod");
        let Value::Object(object) = value else {
            panic!("an object");
        };
        assert_eq!(object.properties.len(), 1);
        assert_eq!(object.properties[0].key, sys::SPA_PROP_mute);
        assert_eq!(object.properties[0].value, Value::Bool(true));
    }
}
