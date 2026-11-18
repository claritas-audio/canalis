//! Acoustic feedback: when a real microphone would reach real speakers, directly or
//! through chains, and the dialog that asks before it happens.

use canalis_pw::{ChainConfig, ChainKind, Direction, Graph, MediaClass};
use egui::{Button, RichText, Ui, vec2};

use crate::i18n::{t, tf};
use crate::theme;

/// How deep a feedback check follows chains into chains.
const DEPTH: usize = 8;

/// The ends among `outputs` through which a real microphone in `inputs` would reach real
/// speakers: an acoustic loop when the mic can hear them. A mic counts directly or through
/// a chain feeding this one; speakers directly or through a chain this one feeds.
pub fn speakers_heard(
    graph: &Graph,
    configs: &[ChainConfig],
    chain: &ChainConfig,
    inputs: &[String],
    outputs: &[String],
) -> Vec<String> {
    let hears_mic = match chain.kind {
        ChainKind::Mic | ChainKind::Insert => inputs
            .iter()
            .any(|n| reaches(graph, configs, n, DEPTH, Direction::Input)),
        ChainKind::Output | ChainKind::Cable => false,
    };
    if !hears_mic {
        return Vec::new();
    }
    outputs
        .iter()
        .filter(|n| reaches(graph, configs, n, DEPTH, Direction::Output))
        .cloned()
        .collect()
}

/// The speakers the new ends make the mic reach that the current ends did not.
pub fn new_speakers(
    graph: &Graph,
    configs: &[ChainConfig],
    chain: &ChainConfig,
    inputs: &[String],
    outputs: &[String],
) -> Vec<String> {
    let was = speakers_heard(graph, configs, chain, &chain.inputs, &chain.outputs);
    speakers_heard(graph, configs, chain, inputs, outputs)
        .into_iter()
        .filter(|n| !was.contains(n))
        .collect()
}

/// Whether a link from node `output` to node `input` lets a real mic reach real speakers.
pub fn link_heard(graph: &Graph, configs: &[ChainConfig], output: &str, input: &str) -> bool {
    reaches(graph, configs, output, DEPTH, Direction::Input)
        && reaches(graph, configs, input, DEPTH, Direction::Output)
}

/// Whether `node` is, or leads through chains to, hardware: a capture device looking
/// upstream (`Input`), a playback device looking downstream (`Output`).
fn reaches(
    graph: &Graph,
    configs: &[ChainConfig],
    node: &str,
    depth: usize,
    direction: Direction,
) -> bool {
    let hardware = match direction {
        Direction::Input => MediaClass::Source,
        Direction::Output => MediaClass::Sink,
    };
    if graph
        .node_by_name(node)
        .is_some_and(|n| n.class == hardware && !n.is_virtual_device())
    {
        return true;
    }
    if depth == 0 {
        return false;
    }
    configs
        .iter()
        .filter(|c| match direction {
            Direction::Input => {
                c.kind.source_node_name(&c.name) == node
                    || c.kind.playback_node_name(&c.name) == node
            }
            Direction::Output => c.kind.capture_node_name(&c.name) == node,
        })
        .any(|c| {
            let next = match direction {
                Direction::Input if c.kind.picks_inputs() => &c.inputs,
                Direction::Output if c.kind.picks_outputs() || c.kind == ChainKind::Mic => {
                    &c.outputs
                }
                _ => return false,
            };
            next.iter()
                .any(|n| reaches(graph, configs, n, depth - 1, direction))
        })
}

/// The warning before a mic reaches speakers: `Some(true)` to go ahead, `Some(false)` to
/// leave things as they were, `None` while it waits.
pub fn ask(ui: &Ui, what: &str, after: &str, yes: &str) -> Option<bool> {
    let mut answer = None;
    let modal = egui::Modal::new(ui.scope_id().with("feedback")).show(ui.ctx(), |ui| {
        ui.set_width(400.0);
        ui.label(
            RichText::new(t("⚠ Your microphone will play through the speakers"))
                .size(15.0)
                .strong()
                .color(theme::ORANGE),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(tf(
                "{what} If the mic can hear them, the sound loops back and howls, \
                 loudly. Use headphones, or keep the volume low.{after}",
                &[("what", &what), ("after", &after)],
            ))
            .size(12.5)
            .color(theme::TEXT_DIM),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(Button::new(yes).min_size(vec2(0.0, 28.0))).clicked() {
                    answer = Some(true);
                }
                if ui.button(t("Cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::{link_heard, new_speakers, speakers_heard};
    use canalis_pw::{ChainConfig, Graph, Node};

    fn node(name: &'static str, class: &'static str) -> Node {
        Node::from_props(1, |key| match key {
            "node.name" => Some(name),
            "media.class" => Some(class),
            _ => None,
        })
    }

    fn chain(kind: &str, name: &str, inputs: &[&str], outputs: &[&str]) -> ChainConfig {
        toml::from_str(&format!(
            "name = {name:?}\nlabel = \"x\"\nkind = {kind:?}\ninputs = {inputs:?}\noutputs = {outputs:?}"
        ))
        .expect("a chain")
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn a_mic_reaching_speakers_is_a_feedback_risk_directly_or_through_a_chain() {
        let mut graph = Graph::default();
        graph.nodes.push(node("alsa_input.laptop", "Audio/Source"));
        graph.nodes.push(node("alsa_output.laptop", "Audio/Sink"));
        graph.nodes.push(node("canalis.desktop-eq", "Audio/Sink"));
        let desktop = chain("output", "canalis.desktop-eq", &[], &["alsa_output.laptop"]);
        let mic = chain("mic", "canalis.voice", &["alsa_input.laptop"], &[]);
        let configs = [desktop.clone(), mic.clone()];
        let speakers = names(&["alsa_output.laptop"]);
        let mic_in = names(&["alsa_input.laptop"]);
        assert_eq!(
            speakers_heard(&graph, &configs, &mic, &mic_in, &speakers),
            speakers,
            "listening to the mic on the speakers"
        );
        let through_eq = names(&["canalis.desktop-eq"]);
        assert_eq!(
            speakers_heard(&graph, &configs, &mic, &mic_in, &through_eq).len(),
            1,
            "listening through an output chain that plays to the speakers"
        );
        assert!(
            speakers_heard(&graph, &configs, &mic, &[], &speakers).is_empty(),
            "no mic, no loop"
        );
        assert!(
            speakers_heard(&graph, &configs, &desktop, &mic_in, &speakers).is_empty(),
            "an output chain carries what apps play"
        );
        let insert = chain("insert", "canalis.fx", &[], &[]);
        assert_eq!(
            speakers_heard(&graph, &configs, &insert, &mic_in, &speakers).len(),
            1,
            "an insert from the mic to the speakers"
        );
        let after_mic = names(&["canalis.voice.out"]);
        assert_eq!(
            speakers_heard(&graph, &configs, &insert, &after_mic, &speakers).len(),
            1,
            "an insert fed by a mic chain's listen stream"
        );
    }

    #[test]
    fn only_speakers_the_chain_did_not_reach_yet_are_new() {
        let mut graph = Graph::default();
        graph.nodes.push(node("alsa_input.laptop", "Audio/Source"));
        graph.nodes.push(node("alsa_output.laptop", "Audio/Sink"));
        graph.nodes.push(node("alsa_output.hdmi", "Audio/Sink"));
        let mic = chain(
            "mic",
            "canalis.voice",
            &["alsa_input.laptop"],
            &["alsa_output.laptop"],
        );
        let configs = [mic.clone()];
        let inputs = mic.inputs.clone();
        assert_eq!(
            new_speakers(
                &graph,
                &configs,
                &mic,
                &inputs,
                &names(&["alsa_output.laptop", "alsa_output.hdmi"])
            ),
            names(&["alsa_output.hdmi"]),
            "a second speaker on a chain that already reaches one"
        );
        assert!(
            new_speakers(&graph, &configs, &mic, &inputs, &[]).is_empty(),
            "removing ends never asks"
        );
    }

    #[test]
    fn a_link_from_a_mic_to_speakers_is_a_feedback_risk() {
        let mut graph = Graph::default();
        graph.nodes.push(node("alsa_input.laptop", "Audio/Source"));
        graph.nodes.push(node("alsa_output.laptop", "Audio/Sink"));
        let configs = [
            chain("mic", "canalis.voice", &["alsa_input.laptop"], &[]),
            chain("output", "canalis.desktop-eq", &[], &["alsa_output.laptop"]),
        ];
        assert!(link_heard(
            &graph,
            &configs,
            "alsa_input.laptop",
            "alsa_output.laptop"
        ));
        assert!(
            link_heard(&graph, &configs, "canalis.voice.out", "alsa_output.laptop"),
            "a mic chain's listen stream"
        );
        assert!(
            link_heard(&graph, &configs, "canalis.voice", "canalis.desktop-eq"),
            "a mic chain into an output chain that plays to the speakers"
        );
        assert!(
            !link_heard(
                &graph,
                &configs,
                "canalis.desktop-eq.out",
                "alsa_output.laptop"
            ),
            "an output chain carries what apps play"
        );
    }
}
