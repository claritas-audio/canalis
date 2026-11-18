//! Scenes that recall themselves: what a trigger can name (a device's `node.name`, an
//! app's `application.name`) is collected from each snapshot, and a scene fires when its
//! trigger was absent from the previous one and is present now: once per arrival, never
//! for what was already there when the session connected.

use std::collections::BTreeSet;

use crate::config::{Scene, SceneTrigger};
use crate::model::{Graph, MediaClass};

/// What is present now, as the triggers that would match it.
pub(super) fn present(graph: &Graph) -> BTreeSet<SceneTrigger> {
    graph
        .nodes
        .iter()
        .filter_map(|node| match node.class {
            MediaClass::Sink | MediaClass::Source => Some(SceneTrigger::Device {
                name: node.name.clone(),
            }),
            MediaClass::PlaybackStream | MediaClass::CaptureStream => {
                node.app.clone().map(|name| SceneTrigger::App { name })
            }
            MediaClass::Other => None,
        })
        .collect()
}

/// The devices among what is present, by `node.name`.
pub(super) fn devices(present: &BTreeSet<SceneTrigger>) -> BTreeSet<String> {
    present
        .iter()
        .filter_map(|t| match t {
            SceneTrigger::Device { name } => Some(name.clone()),
            SceneTrigger::App { .. } => None,
        })
        .collect()
}

/// The scenes whose trigger appeared between `before` and `now`, in config order.
pub(super) fn fired<'a>(
    scenes: &'a [Scene],
    before: &BTreeSet<SceneTrigger>,
    now: &BTreeSet<SceneTrigger>,
) -> Vec<&'a Scene> {
    scenes
        .iter()
        .filter(|scene| {
            scene
                .when
                .as_ref()
                .is_some_and(|t| now.contains(t) && !before.contains(t))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Node;

    fn node(id: u32, name: &'static str, class: &'static str, app: Option<&'static str>) -> Node {
        Node::from_props(id, |key| match key {
            "node.name" => Some(name),
            "media.class" => Some(class),
            "application.name" => app,
            _ => None,
        })
    }

    fn scene(name: &str, when: SceneTrigger) -> Scene {
        Scene {
            name: name.into(),
            when: Some(when),
            ..Scene::default()
        }
    }

    #[test]
    fn a_trigger_is_saved_with_its_scene() {
        let saved = scene(
            "Calls",
            SceneTrigger::App {
                name: "Discord".into(),
            },
        );
        let text = toml::to_string(&saved).expect("serializes");
        assert!(text.contains("kind = \"app\""), "{text}");
        assert_eq!(toml::from_str::<Scene>(&text).expect("parses"), saved);
        let old: Scene = toml::from_str("name = \"Old\"").expect("a scene without one");
        assert_eq!(old.when, None, "older scenes are recalled by hand");
    }

    #[test]
    fn a_scene_fires_once_when_its_device_or_app_arrives() {
        let headphones = SceneTrigger::Device {
            name: "alsa_output.usb-headset".into(),
        };
        let discord = SceneTrigger::App {
            name: "Discord".into(),
        };
        let scenes = [
            scene("Headphones", headphones.clone()),
            scene("Calls", discord.clone()),
            Scene {
                name: "By hand".into(),
                ..Scene::default()
            },
        ];
        let mut graph = Graph::default();
        graph
            .nodes
            .push(node(1, "alsa_output.speakers", "Audio/Sink", None));
        let before = present(&graph);
        assert!(fired(&scenes, &before, &before).is_empty());

        graph
            .nodes
            .push(node(2, "alsa_output.usb-headset", "Audio/Sink", None));
        graph
            .nodes
            .push(node(3, "Discord", "Stream/Output/Audio", Some("Discord")));
        let now = present(&graph);
        let names: Vec<&str> = fired(&scenes, &before, &now)
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["Headphones", "Calls"]);
        assert!(
            fired(&scenes, &now, &now).is_empty(),
            "still there is not a new arrival"
        );
    }
}
