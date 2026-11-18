//! Devices: outputs and inputs from the snapshot, after the mockups. The glyph at the
//! left is the default star or the device kind; a tag says chain, virtual or hardware;
//! the bar is the device volume. Virtual ones Canalis made can be deleted.

use std::collections::BTreeSet;

use canalis_pw::{ChainKind, Command, DeviceRule, Graph, MediaClass, Node, VirtualKind};
use egui::{Checkbox, ComboBox, Frame, Margin, Modal, RichText, Sense, Stroke, Ui};

use super::{
    DELETE_SIZE, Glyph, VIRTUAL_BG, VIRTUAL_FG, autofocus, column_width, content, db_field,
    db_label, delete_button, outline_button, row_frame, section_header, star_button, tag,
    volume_bar,
};
use crate::i18n::{t, tf};
use crate::theme;

const COLUMN_GAP: f32 = 24.0;
const DB_WIDTH: f32 = 56.0;
/// Below this row width the volume bar shrinks and the dB label goes, so names stay readable.
const NARROW_ROW: f32 = 560.0;
#[derive(Default)]
pub struct DevicesView {
    prompt: Option<Prompt>,
    /// A device whose name is being typed: its node name and the text so far.
    renaming: Option<(String, String)>,
    /// Hidden devices are listed, dimmed, so they can be shown again.
    show_hidden: bool,
    /// Devices whose rule is open for editing, by `node.name`.
    rules_open: BTreeSet<String>,
}

struct Prompt {
    kind: VirtualKind,
    label: String,
}

impl DevicesView {
    pub fn show(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        content(ui, |ui| {
            let column = column_width(ui, 2, COLUMN_GAP);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = COLUMN_GAP;
                ui.allocate_ui_with_layout(
                    egui::vec2(column, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.set_width(column);
                        self.column(ui, graph, VirtualKind::Sink, commands);
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(column, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.set_width(column);
                        self.column(ui, graph, VirtualKind::Source, commands);
                    },
                );
            });
        });
        self.show_prompt(ui, commands);
    }

    fn column(
        &mut self,
        ui: &mut Ui,
        graph: &Graph,
        kind: VirtualKind,
        commands: &mut Vec<Command>,
    ) {
        let (glyph, title, subtitle, help, add_label, class, default) = match kind {
            VirtualKind::Sink => (
                Glyph::Speaker,
                t("Outputs"),
                t("where apps play"),
                t(
                    "An output device is where sound goes: speakers, headphones, an HDMI screen, \
                    or a virtual output that exists only in PipeWire. Apps play into the default \
                    one unless Apps sends them elsewhere; an output chain processes the sound \
                    before passing it on. Click a hollow star to make that device the default.",
                ),
                t("+ Virtual output"),
                MediaClass::Sink,
                graph.defaults.sink.as_deref(),
            ),
            VirtualKind::Source => (
                Glyph::Mic,
                t("Inputs"),
                t("what apps record"),
                t(
                    "An input device is where sound comes from: a microphone, a line in, or a \
                    virtual input that exists only in PipeWire. Calls and recorders record from \
                    the default one; a mic chain processes the microphone and is what they \
                    record from. Click a hollow star to make that device the default.",
                ),
                t("+ Virtual input"),
                MediaClass::Source,
                graph.defaults.source.as_deref(),
            ),
        };
        ui.spacing_mut().item_spacing.y = 10.0;
        let hidden_count = graph
            .nodes
            .iter()
            .filter(|n| n.class == class && graph.is_hidden(&n.name))
            .count();
        section_header(ui, Some(glyph), title, subtitle, Some(help), |ui| {
            if outline_button(ui, add_label).clicked() {
                self.prompt = Some(Prompt {
                    kind,
                    label: String::new(),
                });
            }
            if hidden_count > 0 {
                ui.checkbox(
                    &mut self.show_hidden,
                    RichText::new(tf("show hidden ({n})", &[("n", &hidden_count)])).size(12.0),
                );
            }
        });
        let mut nodes: Vec<&Node> = graph
            .nodes
            .iter()
            .filter(|n| n.class == class && (self.show_hidden || !graph.is_hidden(&n.name)))
            .collect();
        nodes.sort_by(|a, b| a.label.cmp(&b.label));
        if nodes.is_empty() {
            ui.label(theme::small_dim(t("none")));
        }
        for node in nodes {
            row(
                ui,
                graph,
                node,
                kind,
                default == Some(node.name.as_str()),
                &mut self.renaming,
                &mut self.rules_open,
                commands,
            );
            if self.rules_open.contains(&node.name) {
                rule_panel(ui, graph, &node.name, kind, &mut self.rules_open, commands);
            }
        }
        // Rules of devices that are not plugged in, dimmed, so they can be edited or dropped.
        for rule in graph
            .device_rules
            .iter()
            .filter(|r| rule_kind(r) == kind && !graph.nodes.iter().any(|n| n.name == r.device))
        {
            absent_rule(ui, graph, rule, &mut self.rules_open, commands);
            if self.rules_open.contains(&rule.device) {
                rule_panel(
                    ui,
                    graph,
                    &rule.device,
                    kind,
                    &mut self.rules_open,
                    commands,
                );
            }
        }
    }

    fn show_prompt(&mut self, ui: &mut Ui, commands: &mut Vec<Command>) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let title = match prompt.kind {
            VirtualKind::Sink => t("New virtual output"),
            VirtualKind::Source => t("New virtual input"),
        };
        let mut done = false;
        let modal = Modal::new(ui.scope_id().with("new-virtual-device")).show(ui.ctx(), |ui| {
            ui.set_width(320.0);
            ui.label(RichText::new(title).strong());
            ui.label(theme::small_dim(t(
                "Apps will see it under this name; Canalis names the node canalis.<slug>.",
            )));
            ui.add_space(6.0);
            let edit = ui.text_edit_singleline(&mut prompt.label);
            autofocus(ui, &edit);
            let valid = !prompt.kind.node_name(&prompt.label).ends_with('.');
            let submitted = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(valid, egui::Button::new(t("Create")))
                    .clicked()
                    || (submitted && valid)
                {
                    commands.push(Command::CreateVirtual {
                        kind: prompt.kind,
                        label: prompt.label.trim().to_owned(),
                    });
                    done = true;
                }
                if ui.button(t("Cancel")).clicked() {
                    done = true;
                }
            });
        });
        if done || modal.should_close() {
            self.prompt = None;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    graph: &Graph,
    node: &Node,
    kind: VirtualKind,
    is_default: bool,
    renaming: &mut Option<(String, String)>,
    rules_open: &mut BTreeSet<String>,
    commands: &mut Vec<Command>,
) {
    // A cable's two devices (sink and source) both belong to its chain.
    let chain = graph.chains.iter().find(|c| {
        c.name == node.name
            || (c.kind.source_device() && c.kind.source_node_name(&c.name) == node.name)
    });
    let is_virtual = node.is_virtual_device();
    let hidden = graph.is_hidden(&node.name);
    // Virtual devices and chains never come and go, so a rule would never fire.
    let can_rule = !is_virtual && chain.is_none();
    let rule = graph.device_rule(&node.name);
    let row = row_frame(
        ui,
        is_default,
        |ui| {
            if hidden {
                ui.set_opacity(0.45);
            }
            let star = star_button(ui, is_default, ("default", node.id));
            if star.clicked() && !is_default {
                commands.push(Command::SetDefault {
                    kind,
                    name: node.name.clone(),
                });
            }
            star.on_hover_text(if is_default {
                t("the default")
            } else {
                t("make this the default")
            });

            let subtitle = match chain {
                Some(c) => {
                    let args: &[(&str, &dyn std::fmt::Display)] = &[
                        ("name", &node.name),
                        ("n", &c.slots.len()),
                        ("smp", &c.latency_samples),
                    ];
                    if c.slots.len() == 1 {
                        tf("chain · {name} · {n} slot · {smp} smp", args)
                    } else {
                        tf("chain · {name} · {n} slots · {smp} smp", args)
                    }
                }
                None if is_virtual => tf("virtual · {name}", &[("name", &node.name)]),
                None => node.name.clone(),
            };
            let narrow = ui.available_width() < NARROW_ROW;
            let volume_width = if narrow { 80.0 } else { 140.0 };
            let db_width = DB_WIDTH;
            let fixed = 18.0 + 64.0 + volume_width + db_width + 12.0 * 5.0 + DELETE_SIZE + 4.0;
            let text_width = (ui.available_width() - fixed).max(60.0);
            ui.allocate_ui_with_layout(
                egui::vec2(text_width, 34.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_width(text_width);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if let Some((name, text)) = renaming.as_mut().filter(|(n, _)| *n == node.name) {
                        let edit =
                            ui.add(egui::TextEdit::singleline(text).desired_width(text_width));
                        autofocus(ui, &edit);
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if enter || (edit.lost_focus() && !escape) {
                            commands.push(Command::SetLabel {
                                node_name: name.clone(),
                                label: Some(text.clone()),
                            });
                            *renaming = None;
                        } else if escape {
                            *renaming = None;
                        }
                    } else {
                        let label = ui
                            .add(
                                egui::Label::new(RichText::new(&node.label).size(14.0))
                                    .truncate()
                                    .sense(Sense::click()),
                            )
                            .on_hover_text(t("double-click to rename; right-click for more"));
                        if label.double_clicked() {
                            *renaming = Some((node.name.clone(), node.label.clone()));
                        }
                        label.context_menu(|ui| {
                            device_menu(ui, node, hidden, can_rule, renaming, rules_open, commands)
                        });
                    }
                    ui.add(
                        egui::Label::new(RichText::new(subtitle).size(12.0).color(theme::TEXT_DIM))
                            .truncate(),
                    );
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Rows without a delete keep its room, so bars and dB line up down the list.
                if !is_virtual {
                    ui.allocate_space(egui::vec2(DELETE_SIZE, DELETE_SIZE));
                } else if let Some(chain) = chain {
                    // A chain's device goes with its chain: alone it would only come back.
                    if delete_button(ui)
                        .on_hover_text(tf("delete the chain {name}", &[("name", &chain.label)]))
                        .clicked()
                    {
                        commands.push(Command::DeleteChain {
                            name: chain.name.clone(),
                        });
                    }
                } else if delete_button(ui)
                    .on_hover_text(t("delete this virtual device"))
                    .clicked()
                {
                    commands.push(Command::DeleteVirtual {
                        name: node.name.clone(),
                    });
                }
                let cubic = node.volume.as_ref().map_or(1.0, |v| v.cubic());
                ui.allocate_ui_with_layout(
                    egui::vec2(DB_WIDTH, 20.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // A fixed box, so "-12 dB" and "0 dB" leave the bar where it is.
                        ui.set_min_width(DB_WIDTH);
                        ui.set_max_width(DB_WIDTH);
                        if node.volume.is_none() {
                            ui.label(RichText::new("–").size(12.0).color(theme::TEXT_DIM));
                        } else if let Some(new) = db_field(
                            ui,
                            ui.scope_id().with(("db", node.id)),
                            cubic,
                            db_label(cubic),
                        ) {
                            commands.push(Command::SetVolume {
                                node: node.id,
                                cubic: new,
                            });
                        }
                    },
                );
                let bar_color = if is_default {
                    theme::ACCENT
                } else {
                    theme::TEXT_DIM
                };
                let (response, bar) = volume_bar(ui, volume_width, cubic, bar_color);
                if node.volume.is_some() {
                    response.on_hover_text(db_label(cubic));
                }
                if let Some(new) = bar
                    && node.volume.is_some()
                {
                    commands.push(Command::SetVolume {
                        node: node.id,
                        cubic: new,
                    });
                }
                // A device with a rule is hardware: the rule takes the tag's place.
                if let Some(rule) = rule {
                    let chip = tag(ui, t("rule"), theme::SELECTION, theme::ACCENT)
                        .interact(Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(rule_summary(graph, rule));
                    if chip.clicked() && !rules_open.remove(&node.name) {
                        rules_open.insert(node.name.clone());
                    }
                } else {
                    let (text, bg, fg) = if chain.is_some() {
                        (t("chain"), theme::SELECTION, theme::ACCENT)
                    } else if is_virtual {
                        (t("virtual"), VIRTUAL_BG, VIRTUAL_FG)
                    } else {
                        (t("hardware"), theme::BORDER_SOFT, theme::TEXT_DIM)
                    };
                    tag(ui, text, bg, fg);
                }
            });
        },
        |_| {},
    );
    row.response
        .context_menu(|ui| device_menu(ui, node, hidden, can_rule, renaming, rules_open, commands));
}

/// The column a rule belongs in while its device is away.
fn rule_kind(rule: &DeviceRule) -> VirtualKind {
    if rule.input.is_some() || (rule.output.is_none() && rule.device.contains("input")) {
        VirtualKind::Source
    } else {
        VirtualKind::Sink
    }
}

/// A chain's label for the device name it shows as, or the name itself.
fn device_label(graph: &Graph, name: &str) -> String {
    graph
        .chains
        .iter()
        .find(|c| {
            c.name == name || (c.kind.source_device() && c.kind.source_node_name(&c.name) == name)
        })
        .map(|c| c.label.clone())
        .or_else(|| {
            graph
                .nodes
                .iter()
                .find(|n| n.name == name)
                .map(|n| n.label.clone())
        })
        .unwrap_or_else(|| name.to_owned())
}

/// What a rule does, in one line.
fn rule_summary(graph: &Graph, rule: &DeviceRule) -> String {
    let mut parts = Vec::new();
    match &rule.output {
        Some(o) if *o == rule.device => parts.push(t("default output").to_owned()),
        Some(o) => parts.push(tf(
            "default output through {chain}",
            &[("chain", &device_label(graph, o))],
        )),
        None => {}
    }
    match &rule.input {
        Some(i) if *i == rule.device => parts.push(t("default input").to_owned()),
        Some(i) => parts.push(tf(
            "default input through {chain}",
            &[("chain", &device_label(graph, i))],
        )),
        None => {}
    }
    if let Some(scene) = &rule.scene {
        parts.push(tf("scene {name}", &[("name", scene)]));
    }
    let what = parts.join(" · ");
    if rule.restore {
        tf(
            "when it connects: {what}; undone when it goes",
            &[("what", &what)],
        )
    } else {
        tf("when it connects: {what}", &[("what", &what)])
    }
}

/// A rule whose device is not plugged in: its name, what it does, edit and remove.
fn absent_rule(
    ui: &mut Ui,
    graph: &Graph,
    rule: &DeviceRule,
    rules_open: &mut BTreeSet<String>,
    commands: &mut Vec<Command>,
) {
    let label = if rule.label.is_empty() {
        rule.device.clone()
    } else {
        rule.label.clone()
    };
    let summary = rule_summary(graph, rule);
    ui.horizontal(|ui| {
        ui.set_opacity(0.55);
        // Room for the edit button and the delete square at the right.
        let text_width = (ui.available_width() - DELETE_SIZE - 80.0).max(60.0);
        ui.allocate_ui_with_layout(
            egui::vec2(text_width, 34.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.set_width(text_width);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(egui::Label::new(RichText::new(label).size(13.0)).truncate());
                ui.add(
                    egui::Label::new(theme::small_dim(tf(
                        "not connected · {what}",
                        &[("what", &summary)],
                    )))
                    .truncate(),
                )
                .on_hover_text(&summary);
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if delete_button(ui)
                .on_hover_text(t("remove this rule"))
                .clicked()
            {
                commands.push(Command::SetDeviceRule {
                    device: rule.device.clone(),
                    rule: None,
                });
                rules_open.remove(&rule.device);
            }
            if ui.small_button(t("edit")).clicked() && !rules_open.remove(&rule.device) {
                rules_open.insert(rule.device.clone());
            }
        });
    });
}

/// What happens when `device` connects, edited in place under its row.
fn rule_panel(
    ui: &mut Ui,
    graph: &Graph,
    device: &str,
    kind: VirtualKind,
    rules_open: &mut BTreeSet<String>,
    commands: &mut Vec<Command>,
) {
    let current = graph
        .device_rule(device)
        .cloned()
        .unwrap_or_else(|| DeviceRule::new(device));
    let mut rule = current.clone();
    if let Some(node) = graph.nodes.iter().find(|n| n.name == device) {
        rule.label.clone_from(&node.label);
    }
    let mut close = false;
    Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new(t("When it connects")).strong());
            let (target, text, chains): (&mut Option<String>, &str, Vec<(String, String)>) =
                match kind {
                    VirtualKind::Sink => (
                        &mut rule.output,
                        t("make it the default output"),
                        graph
                            .chains
                            .iter()
                            .filter(|c| c.kind == ChainKind::Output)
                            .map(|c| (c.name.clone(), c.label.clone()))
                            .collect(),
                    ),
                    VirtualKind::Source => (
                        &mut rule.input,
                        t("make it the default input"),
                        graph
                            .chains
                            .iter()
                            .filter(|c| c.kind == ChainKind::Mic)
                            .map(|c| (c.kind.source_node_name(&c.name), c.label.clone()))
                            .collect(),
                    ),
                };
            ui.horizontal(|ui| {
                let mut on = target.is_some();
                if ui.checkbox(&mut on, text).changed() {
                    *target = on.then(|| device.to_owned());
                }
                if let Some(name) = target.as_mut() {
                    let shown = if name == device {
                        t("directly").to_owned()
                    } else {
                        tf("through {chain}", &[("chain", &device_label(graph, name))])
                    };
                    ComboBox::from_id_salt(("rule-target", device))
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(name, device.to_owned(), t("directly"));
                            for (chain, label) in &chains {
                                ui.selectable_value(
                                    name,
                                    chain.clone(),
                                    tf("through {chain}", &[("chain", label)]),
                                );
                            }
                        });
                }
            });
            ui.horizontal(|ui| {
                let mut on = rule.scene.is_some();
                let toggle = ui
                    .add_enabled(
                        !graph.scenes.is_empty(),
                        Checkbox::new(&mut on, t("recall a scene")),
                    )
                    .on_disabled_hover_text(t("save a scene first, under Scene ▾"));
                if toggle.changed() {
                    rule.scene = on.then(|| graph.scenes[0].name.clone());
                }
                if let Some(scene) = rule.scene.as_mut() {
                    ComboBox::from_id_salt(("rule-scene", device))
                        .selected_text(scene.clone())
                        .show_ui(ui, |ui| {
                            for s in &graph.scenes {
                                ui.selectable_value(scene, s.name.clone(), &s.name);
                            }
                        });
                }
            });
            ui.checkbox(
                &mut rule.restore,
                t("put the previous defaults back when it disconnects"),
            )
            .on_hover_text(t(
                "Unless you picked another default while it was connected: then yours stays.",
            ));
            ui.horizontal(|ui| {
                if ui.button(t("Done")).clicked() {
                    close = true;
                }
                if !current.is_empty() && ui.button(t("Remove rule")).clicked() {
                    rule = DeviceRule {
                        label: rule.label.clone(),
                        ..DeviceRule::new(device)
                    };
                    close = true;
                }
            });
        });
    if rule != current {
        commands.push(Command::SetDeviceRule {
            device: device.to_owned(),
            rule: Some(rule),
        });
    }
    if close {
        rules_open.remove(device);
    }
}

/// Rename, PipeWire's name, hide: the row's right-click menu.
fn device_menu(
    ui: &mut Ui,
    node: &Node,
    hidden: bool,
    can_rule: bool,
    renaming: &mut Option<(String, String)>,
    rules_open: &mut BTreeSet<String>,
    commands: &mut Vec<Command>,
) {
    if ui.button(t("Rename…")).clicked() {
        *renaming = Some((node.name.clone(), node.label.clone()));
        ui.close();
    }
    if ui.button(t("Use PipeWire's name")).clicked() {
        commands.push(Command::SetLabel {
            node_name: node.name.clone(),
            label: None,
        });
        ui.close();
    }
    if can_rule && ui.button(t("When it connects…")).clicked() {
        rules_open.insert(node.name.clone());
        ui.close();
    }
    ui.separator();
    if ui
        .button(if hidden { t("Show") } else { t("Hide") })
        .clicked()
    {
        commands.push(Command::SetHidden {
            key: node.name.clone(),
            hidden: !hidden,
        });
        ui.close();
    }
}
