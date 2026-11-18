//! Graph: the patchbay. Node boxes with inputs left and outputs right, links as
//! beziers, pan and zoom from `egui::Scene`; drag from a port and the ports that can take
//! the link light up and catch it from a distance; select with a click or a
//! marquee dragged on the background, and Delete removes (ADR 0004). F fits, Ctrl+F finds a
//! box, arrows nudge; moves go through undo and the layout is kept across windows. A chain's two ends fold into one box unless asked
//! otherwise; chains and virtual devices are tinted, pinned streams get dashed links,
//! the default device a star.

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;

use canalis_pw::{
    ChainConfig, ChainState, Command, Direction, Graph, MediaClass, Node, NodeId, Port, PortId,
};
use egui::{
    Button, Color32, FontId, Pos2, Rect, RichText, Scene, Sense, Shape, Stroke, StrokeKind, Ui,
    pos2, vec2,
};

use super::feedback;
use super::groups::{self, GROUP_KEY, Group};
use super::{
    METER_FLOOR_DB, STAR, VIRTUAL_FG, chain_color, chip, parse_hex, smoothed, text_center_left,
    truncated,
};
use crate::i18n::{t, tf};
use crate::theme;
use crate::undo::Layout;

const NODE_WIDTH: f32 = 200.0;
const HEADER_HEIGHT: f32 = 30.0;
const PORT_ROW: f32 = 18.0;
const FOOTER: f32 = 6.0;
const PORT_RADIUS: f32 = 4.0;
const COLUMN_GAP: f32 = 140.0;
const ROW_GAP: f32 = 24.0;
const LINK_WIDTH: f32 = 1.8;
const ZOOM_RANGE: RangeInclusive<f32> = 0.25..=1.5;
const FIT_MARGIN: f32 = 40.0;
const GRID_STEP: f32 = 24.0;
/// Grid lines closer than this on screen are skipped by doubling the step.
const MIN_GRID_PX: f32 = 18.0;
const TITLE_FONT: f32 = 12.5;
const PORT_FONT: f32 = 10.5;
/// Square around a port dot that catches drags.
const PORT_HIT: f32 = 14.0;
/// Screen pixels within which a dragged link snaps to a port that can take it.
const SNAP_PX: f32 = 40.0;
/// Screen pixels from a link that count as pointing at it.
const LINK_HIT_PX: f32 = 12.0;
const LINK_SAMPLES: usize = 24;
/// Scene units an arrow key moves the selected boxes; Shift moves four times as far.
const NUDGE: f32 = 8.0;
/// How long a box found with Ctrl+F glows, in seconds.
const FLASH_SECS: f64 = 1.5;
const SEARCH_RESULTS: usize = 8;
/// How often the view is written back to disk, at most, in seconds.
const SAVE_EVERY: f64 = 1.0;
const PLAIN: Color32 = Color32::from_rgb(0x8b, 0x91, 0x9d);

/// Where a box first appears: producers left, consumers right, chains between.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Column {
    Left,
    Middle,
    Right,
}

impl Column {
    fn x(self) -> f32 {
        let index = match self {
            Column::Left => 0.0,
            Column::Middle => 1.0,
            Column::Right => 2.0,
        };
        index * (NODE_WIDTH + COLUMN_GAP)
    }
}

/// Retained view state: the camera and where each box sits, by name so ids changing
/// across daemon restarts keep the layout.
pub struct GraphView {
    scene_rect: Rect,
    /// The camera was fitted to the boxes once they existed; Tidy asks for it again.
    fitted: bool,
    positions: HashMap<String, Pos2>,
    collapse_chains: bool,
    show_midi: bool,
    show_monitors: bool,
    /// A link being drawn from a port, until the button is released.
    dragging: Option<DragFrom>,
    /// A dropped link that lets a mic reach speakers, and what it joins, until answered.
    feedback: Option<(Command, String)>,
    /// A box was double-clicked: the view that configures it, until `main` takes it.
    open: Option<Opens>,
    /// Selected boxes by key and links by id; Shift+click and Shift+marquee add to them.
    selected: HashSet<String>,
    selected_links: HashSet<u32>,
    /// A marquee being dragged on the background, until the button is released.
    band: Option<Band>,
    /// A link right-clicked, and where (on screen) its menu opens.
    link_menu: Option<(u32, Pos2)>,
    /// Deletions that cannot be undone (chains, virtual devices), waiting for a yes.
    confirm: Option<Vec<(String, Command)>>,
    /// The camera to jump to once the scene has drawn: F, Shift+F, a found box.
    fit_to: Option<Rect>,
    /// The layout when a box drag started, until it ends and becomes `layout_change`.
    layout_before: Option<Layout>,
    /// A finished move, until `main` records it for undo.
    layout_change: Option<(Layout, Layout)>,
    search: String,
    search_open: bool,
    /// A box found with Ctrl+F and when, so it glows for a moment.
    flash: Option<(String, f64)>,
    /// What was last written to disk, and when.
    saved: Option<(String, f64)>,
    /// Named frames around boxes, open or folded into one box.
    groups: Vec<Group>,
}

/// What a double-click on a box asks to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opens {
    Devices,
    Apps,
    Chains,
}

/// Where a marquee started, and whether it adds to the selection (Shift) or replaces it.
#[derive(Clone, Copy)]
struct Band {
    start: Pos2,
    adds: bool,
}

/// Whether something ends up selected after a marquee: `was` before it, `inside` it.
fn marquee_selects(adds: bool, was: bool, inside: bool) -> bool {
    inside || (adds && was)
}

/// Whether a link's curve passes through `area`.
fn link_touches(area: Rect, from: Pos2, to: Pos2) -> bool {
    let curve = bezier(from, to, Stroke::NONE);
    (0..=LINK_SAMPLES).any(|i| area.contains(curve.sample(i as f32 / LINK_SAMPLES as f32)))
}

struct DragFrom {
    port: PortId,
    direction: Direction,
    anchor: Pos2,
}

/// Where a port dot sits, which way it faces and what colour it carries.
#[derive(Clone, Copy)]
struct Anchor {
    pos: Pos2,
    direction: Direction,
    color: Color32,
    /// The port belongs to a stream pinned with `target.object`: its links are dashed.
    pinned: bool,
}

impl Default for GraphView {
    fn default() -> Self {
        GraphView {
            scene_rect: Rect::ZERO,
            fitted: false,
            positions: HashMap::new(),
            collapse_chains: true,
            show_midi: false,
            show_monitors: false,
            dragging: None,
            feedback: None,
            open: None,
            selected: HashSet::new(),
            selected_links: HashSet::new(),
            band: None,
            link_menu: None,
            confirm: None,
            fit_to: None,
            layout_before: None,
            layout_change: None,
            search: String::new(),
            search_open: false,
            flash: None,
            saved: None,
            groups: Vec::new(),
        }
    }
}

impl GraphView {
    /// The view a double-clicked box asked for, once.
    pub fn take_open(&mut self) -> Option<Opens> {
        self.open.take()
    }

    /// The view as the last window left it: camera and box positions.
    pub fn restored() -> GraphView {
        let mut view = GraphView::default();
        let Some(text) = state_file().and_then(|path| std::fs::read_to_string(path).ok()) else {
            return view;
        };
        if let Some((camera, positions)) = parse_state(&text) {
            view.scene_rect = camera;
            view.positions = positions;
            view.fitted = true;
        }
        view.groups = parse_groups(&text);
        view.saved = Some((text, 0.0));
        view
    }

    /// Boxes moved since the last call: the layout before and after, for undo.
    pub fn take_layout_change(&mut self) -> Option<(Layout, Layout)> {
        self.layout_change.take()
    }

    /// Puts boxes back where an undo or redo says.
    pub fn set_layout(&mut self, layout: Layout) {
        self.positions.extend(layout);
    }

    /// Writes the camera and the positions back when they changed, at most once a second
    /// and never mid-drag.
    fn save(&mut self, ui: &Ui) {
        let now = ui.input(|i| i.time);
        if self
            .saved
            .as_ref()
            .is_some_and(|(_, at)| now - at < SAVE_EVERY)
            || ui.input(|i| i.pointer.any_down())
            || self.positions.is_empty()
        {
            return;
        }
        let text = state_text(self.scene_rect, &self.positions, &self.groups);
        if self.saved.as_ref().is_some_and(|(saved, _)| *saved == text) {
            self.saved = Some((text, now));
            return;
        }
        let Some(path) = state_file() else {
            return;
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, &text));
        if let Err(e) = written {
            log::warn!("cannot keep the Graph layout in {}: {e}", path.display());
        }
        self.saved = Some((text, now));
    }
}

/// `$XDG_STATE_HOME/canalis/graph.toml`: the Graph's camera and box positions.
fn state_file() -> Option<std::path::PathBuf> {
    Some(dirs::state_dir()?.join("canalis").join("graph.toml"))
}

fn state_text(camera: Rect, positions: &Layout, groups: &[Group]) -> String {
    let mut boxes = toml::Table::new();
    for (key, pos) in positions {
        boxes.insert(key.clone(), toml::Value::from(vec![pos.x, pos.y]));
    }
    let mut table = toml::Table::new();
    let corners = vec![camera.min.x, camera.min.y, camera.max.x, camera.max.y];
    table.insert("camera".into(), toml::Value::from(corners));
    table.insert("boxes".into(), toml::Value::Table(boxes));
    if !groups.is_empty() {
        table.insert("group".into(), groups::to_toml(groups));
    }
    table.to_string()
}

fn parse_groups(text: &str) -> Vec<Group> {
    text.parse::<toml::Table>()
        .map(|table| groups::from_toml(table.get("group")))
        .unwrap_or_default()
}

fn parse_state(text: &str) -> Option<(Rect, Layout)> {
    let table: toml::Table = text.parse().ok()?;
    let floats = |value: &toml::Value| -> Option<Vec<f32>> {
        value
            .as_array()?
            .iter()
            .map(|v| v.as_float().map(|f| f as f32))
            .collect()
    };
    let camera = match floats(table.get("camera")?)?.as_slice() {
        [x0, y0, x1, y1] => Rect::from_min_max(pos2(*x0, *y0), pos2(*x1, *y1)),
        _ => return None,
    };
    let positions = table
        .get("boxes")?
        .as_table()?
        .iter()
        .filter_map(|(key, value)| match floats(value)?.as_slice() {
            [x, y] => Some((key.clone(), pos2(*x, *y))),
            _ => None,
        })
        .collect();
    Some((camera, positions))
}

/// One box on the canvas: a node, or a chain's two ends folded together.
struct Entity<'a> {
    /// The position key: the node name, or the chain name when folded.
    key: String,
    title: String,
    subtitle: String,
    tint: Color32,
    /// Every node the box stands for, for hover text and drags.
    nodes: Vec<&'a Node>,
    inputs: Vec<&'a Port>,
    outputs: Vec<&'a Port>,
    column: Column,
    is_default: bool,
    pinned: bool,
}

impl Entity<'_> {
    fn height(&self) -> f32 {
        HEADER_HEIGHT + self.inputs.len().max(self.outputs.len()) as f32 * PORT_ROW + FOOTER
    }
}

impl GraphView {
    pub fn show(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        let entities = self.entities(graph);
        let mut tidied_from = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            toggle(ui, &mut self.collapse_chains, t("Chains collapsed"));
            toggle(ui, &mut self.show_midi, t("Show MIDI"));
            toggle(ui, &mut self.show_monitors, t("Show monitors"));
            if ui
                .add(
                    Button::new(RichText::new(t("Tidy")).size(12.0))
                        .fill(theme::PANEL)
                        .stroke(Stroke::new(1.0, theme::BORDER)),
                )
                .on_hover_text(t(
                    "lay the boxes out again (only the selected ones, if any)",
                ))
                .clicked()
            {
                tidied_from = Some(self.positions.clone());
                if self.selected.is_empty() {
                    self.positions.clear();
                    self.fitted = false;
                } else {
                    self.tidy_selection(&entities);
                }
            }
            if ui
                .add_enabled(
                    self.selected.len() >= 2,
                    Button::new(RichText::new(t("Group")).size(12.0))
                        .fill(theme::PANEL)
                        .stroke(Stroke::new(1.0, theme::BORDER)),
                )
                .on_hover_text(t(
                    "put the selected boxes in a named frame that moves as one (Ctrl+G)",
                ))
                .clicked()
            {
                self.group_selected();
            }
            self.search_field(ui, &entities);
        });
        ui.label(theme::small_dim(t("drag from a port to link · click or drag the background to select, Shift adds · Delete removes · Ctrl+drag, right drag or Space to pan")));
        ui.label(theme::small_dim(t("dashed = pinned by the Apps view (target.object) · tinted = chain or virtual device · ★ = default")));
        ui.add_space(4.0);
        self.place_new(&entities);
        if let Some(before) = tidied_from {
            self.layout_change = Some((before, self.positions.clone()));
        }
        if !self.fitted
            && let Some(bounds) = self.bounds(&entities, None)
        {
            self.scene_rect = bounds.expand(FIT_MARGIN);
            self.fitted = true;
        }
        if graph
            .chains
            .iter()
            .any(|c| c.status == canalis_pw::ChainStatus::Running)
        {
            // Every frame, as the Chains view does, so the levels fall smoothly.
            ui.ctx().request_repaint();
        }
        let mut scene_rect = self.scene_rect;
        // A left drag on the background draws the marquee; the middle and right buttons
        // pan, and so does the left one while Space is held.
        let pan = if pan_held(ui) {
            egui::DragPanButtons::all()
        } else {
            egui::DragPanButtons::SECONDARY | egui::DragPanButtons::MIDDLE
        };
        // The scene fills what is left of the view; the minimap sits in its corner.
        let area = ui.available_rect_before_wrap();
        Scene::new()
            .zoom_range(ZOOM_RANGE)
            .drag_pan_buttons(pan)
            .show(ui, &mut scene_rect, |ui| {
                self.canvas(ui, graph, &entities, commands);
            });
        self.scene_rect = self.fit_to.take().unwrap_or(scene_rect);
        // Boxes folded or opened this frame are drawn next frame; the map follows then.
        let boxes: Vec<(Rect, Color32)> = entities
            .iter()
            .filter_map(|e| {
                let pos = self.positions.get(&e.key)?;
                Some((
                    Rect::from_min_size(*pos, vec2(NODE_WIDTH, e.height())),
                    e.tint,
                ))
            })
            .collect();
        if self.confirm.is_none()
            && self.feedback.is_none()
            && let Some(shift) = groups::minimap(ui, area, self.scene_rect, &boxes)
        {
            self.scene_rect = self.scene_rect.translate(shift);
        }
        self.save(ui);
        self.confirm_dialog(ui, commands);
        self.feedback_dialog(ui, commands);
    }

    fn feedback_dialog(&mut self, ui: &Ui, commands: &mut Vec<Command>) {
        let Some((_, joins)) = &self.feedback else {
            return;
        };
        let Some(yes) = feedback::ask(ui, joins, "", t("Connect anyway")) else {
            return;
        };
        if let Some((link, _)) = self.feedback.take()
            && yes
        {
            commands.push(link);
        }
    }

    /// The boxes to draw: chains folded when asked, MIDI and monitors when asked.
    fn entities<'a>(&self, graph: &'a Graph) -> Vec<Entity<'a>> {
        let mut folded: HashMap<&str, &canalis_pw::ChainState> = HashMap::new();
        if self.collapse_chains {
            for chain in &graph.chains {
                folded.insert(&chain.name, chain);
            }
        }
        let mut taken: Vec<NodeId> = Vec::new();
        let mut entities = Vec::new();
        for chain in folded.values() {
            let capture = graph.node_by_name(&chain.kind.capture_node_name(&chain.name));
            let playback = graph.node_by_name(&chain.kind.playback_node_name(&chain.name));
            let device = chain
                .kind
                .source_device()
                .then(|| graph.node_by_name(&chain.kind.source_node_name(&chain.name)))
                .flatten();
            let key = graph.node_by_name(&chain.kind.key_node_name(&chain.name));
            let insert = graph.node_by_name(&chain.kind.insert_return_node_name(&chain.name));
            let nodes: Vec<&Node> = [capture, playback, device, key, insert]
                .into_iter()
                .flatten()
                .collect();
            if nodes.is_empty() {
                continue;
            }
            taken.extend(nodes.iter().map(|n| n.id));
            let (mut inputs, mut outputs) = (Vec::new(), Vec::new());
            for node in &nodes {
                let (i, o) = self.ports_of(graph, node.id);
                // Inside a folded mic or cable chain the playback end feeds the null source;
                // that pair stays inside, the other pairs (Listen) leave the box.
                let feeds_device = |port: &Port| {
                    device.is_some_and(|d| {
                        graph.links.iter().any(|l| {
                            l.output == port.id
                                && graph.port(l.input).is_some_and(|p| p.node == d.id)
                        })
                    })
                };
                let internal_inputs = Some(node.id) == device.map(|n| n.id);
                if !internal_inputs {
                    inputs.extend(i);
                }
                if device.is_some() && Some(node.id) == playback.map(|n| n.id) {
                    outputs.extend(o.into_iter().filter(|p| !feeds_device(p)));
                } else {
                    outputs.extend(o);
                }
            }
            let is_default = nodes.iter().any(|n| is_default(graph, n));
            entities.push(Entity {
                key: chain.name.clone(),
                title: match &chain.icon {
                    Some(icon) => format!("{icon} {}", chain.label),
                    None => chain.label.clone(),
                },
                subtitle: if chain.slots.len() == 1 {
                    tf("chain · {n} slot", &[("n", &chain.slots.len())])
                } else {
                    tf("chain · {n} slots", &[("n", &chain.slots.len())])
                },
                tint: chain_color(chain),
                nodes,
                inputs,
                outputs,
                column: Column::Middle,
                is_default,
                pinned: false,
            });
        }
        for node in &graph.nodes {
            if taken.contains(&node.id) {
                continue;
            }
            if node.class == MediaClass::Other && !self.show_midi {
                continue;
            }
            if graph.is_hidden(&node.name) || graph.is_hidden(node.hide_key()) {
                continue;
            }
            let (inputs, outputs) = self.ports_of(graph, node.id);
            let chain = graph.chain(chain_name_of(&node.name));
            let app_color = node
                .app
                .as_ref()
                .and_then(|app| graph.apps.get(app))
                .and_then(|style| style.color.as_deref())
                .and_then(parse_hex);
            let tint = match chain {
                Some(c) => chain_color(c),
                None if node.is_virtual_device() => VIRTUAL_FG,
                None => app_color.unwrap_or(PLAIN),
            };
            let subtitle = match (chain, node.class) {
                (Some(_), _) => t("chain end").to_owned(),
                (None, _) if node.is_virtual_device() => t("virtual").to_owned(),
                (None, MediaClass::PlaybackStream) => t("playing").to_owned(),
                (None, MediaClass::CaptureStream) => t("recording").to_owned(),
                (None, MediaClass::Sink) => t("output").to_owned(),
                (None, MediaClass::Source) => t("input").to_owned(),
                (None, MediaClass::Other) => t("node").to_owned(),
            };
            let column = match node.class {
                MediaClass::Source | MediaClass::PlaybackStream => Column::Left,
                MediaClass::Other => Column::Middle,
                MediaClass::Sink | MediaClass::CaptureStream => Column::Right,
            };
            // Streams of one app share a node name; each gets its own box.
            let key = if node.is_stream() {
                format!("{}#{}", node.name, node.id.0)
            } else {
                node.name.clone()
            };
            entities.push(Entity {
                key,
                title: node.short_label.clone(),
                subtitle,
                tint,
                nodes: vec![node],
                inputs,
                outputs,
                column,
                is_default: is_default(graph, node),
                pinned: node.is_stream() && node.target.is_some(),
            });
        }
        // A folded group is one box with only the ports its boxes link outside it.
        for group in self.groups.iter().filter(|g| g.collapsed) {
            let (inside, rest): (Vec<Entity<'a>>, Vec<Entity<'a>>) = entities
                .into_iter()
                .partition(|e| group.members.contains(&e.key));
            entities = rest;
            if inside.is_empty() {
                continue;
            }
            let ports: HashSet<PortId> = inside
                .iter()
                .flat_map(|e| e.inputs.iter().chain(&e.outputs))
                .map(|p| p.id)
                .collect();
            let inputs = groups::outside_ports(
                graph,
                inside.iter().flat_map(|e| e.inputs.iter().copied()),
                &ports,
            );
            let outputs = groups::outside_ports(
                graph,
                inside.iter().flat_map(|e| e.outputs.iter().copied()),
                &ports,
            );
            entities.push(Entity {
                key: group.key(),
                title: group.name.clone(),
                subtitle: if inside.len() == 1 {
                    tf("group · {n} box", &[("n", &inside.len())])
                } else {
                    tf("group · {n} boxes", &[("n", &inside.len())])
                },
                tint: group.color32(),
                is_default: inside.iter().any(|e| e.is_default),
                nodes: inside.into_iter().flat_map(|e| e.nodes).collect(),
                inputs,
                outputs,
                column: Column::Middle,
                pinned: false,
            });
        }
        entities
    }

    /// Folds a group into one box where its boxes start, or opens it where its box is.
    fn fold(&mut self, id: u32, collapse: bool) {
        let Some(group) = self.groups.iter_mut().find(|g| g.id == id) else {
            return;
        };
        let key = group.key();
        let start = group
            .members
            .iter()
            .filter_map(|k| self.positions.get(k))
            .fold(None, |min: Option<Pos2>, p| {
                Some(min.map_or(*p, |m| m.min(*p)))
            });
        if collapse {
            if let Some(start) = start {
                self.positions.insert(key, start);
            }
        } else if let (Some(at), Some(start)) = (self.positions.remove(&key), start) {
            let delta = at - start;
            for member in &group.members {
                if let Some(pos) = self.positions.get_mut(member) {
                    *pos += delta;
                }
            }
        }
        group.collapsed = collapse;
        self.selected.clear();
    }

    /// Rename, colour, fold or open, ungroup: a group's right-click menu.
    fn group_menu(&mut self, ui: &mut Ui, id: u32) {
        let Some(index) = self.groups.iter().position(|g| g.id == id) else {
            return;
        };
        ui.label(theme::small_dim(t("Name")));
        ui.text_edit_singleline(&mut self.groups[index].name);
        ui.label(theme::small_dim(t("Colour")));
        ui.horizontal(|ui| {
            for hex in super::PALETTE {
                let color = parse_hex(hex).unwrap_or(theme::ACCENT);
                let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                ui.painter().rect_filled(rect, 3.0, color);
                if self.groups[index].color == hex {
                    ui.painter().rect_stroke(
                        rect.expand(2.0),
                        3.0,
                        Stroke::new(1.5, theme::TEXT),
                        StrokeKind::Outside,
                    );
                }
                if response.clicked() {
                    self.groups[index].color = hex.to_owned();
                }
            }
        });
        ui.separator();
        let collapsed = self.groups[index].collapsed;
        if ui
            .button(if collapsed {
                t("Open group")
            } else {
                t("Fold group")
            })
            .clicked()
        {
            self.fold(id, !collapsed);
            ui.close();
        }
        if ui.button(t("Ungroup")).clicked() {
            if collapsed {
                self.fold(id, false);
            }
            self.groups.retain(|g| g.id != id);
            ui.close();
        }
    }

    /// Puts the selected boxes in a new group.
    fn group_selected(&mut self) {
        if self.selected.len() < 2 {
            return;
        }
        let selected = self.selected.iter().cloned().collect();
        // A folded group in the selection opens first, so its boxes keep their places.
        for id in self
            .groups
            .iter()
            .filter(|g| g.collapsed && self.selected.contains(&g.key()))
            .map(|g| g.id)
            .collect::<Vec<_>>()
        {
            self.fold(id, false);
        }
        groups::group(&mut self.groups, selected);
        self.selected.clear();
    }

    fn ports_of<'a>(&self, graph: &'a Graph, node: NodeId) -> (Vec<&'a Port>, Vec<&'a Port>) {
        // A source's monitor ports are what apps record from, so they always show.
        let is_source = graph
            .node(node)
            .is_some_and(|n| n.class == MediaClass::Source);
        let (mut inputs, mut outputs): (Vec<&Port>, Vec<&Port>) = graph
            .ports_of(node)
            .filter(|p| self.show_monitors || is_source || !p.monitor)
            .partition(|p| p.direction == Direction::Input);
        for ports in [&mut inputs, &mut outputs] {
            ports.sort_by(|a, b| a.monitor.cmp(&b.monitor).then_with(|| a.name.cmp(&b.name)));
        }
        (inputs, outputs)
    }

    /// Around every box, or only the ones in `only`.
    fn bounds(&self, entities: &[Entity<'_>], only: Option<&HashSet<String>>) -> Option<Rect> {
        entities
            .iter()
            .filter(|e| only.is_none_or(|keys| keys.contains(&e.key)))
            .filter_map(|e| {
                let pos = *self.positions.get(&e.key)?;
                Some(Rect::from_min_size(pos, vec2(NODE_WIDTH, e.height())))
            })
            .reduce(|a, b| a.union(b))
    }

    fn place_new(&mut self, entities: &[Entity<'_>]) {
        let mut bottom: HashMap<Column, f32> = HashMap::new();
        for e in entities {
            if let Some(pos) = self.positions.get(&e.key) {
                let entry = bottom.entry(e.column).or_insert(f32::MIN);
                *entry = entry.max(pos.y + e.height());
            }
        }
        for e in entities {
            if self.positions.contains_key(&e.key) {
                continue;
            }
            let y = bottom.get(&e.column).map_or(0.0, |b| b + ROW_GAP);
            self.positions.insert(e.key.clone(), pos2(e.column.x(), y));
            bottom.insert(e.column, y + e.height());
        }
    }

    /// Lays the selected boxes out in their columns from where the selection starts.
    fn tidy_selection(&mut self, entities: &[Entity<'_>]) {
        let Some(origin) = self.bounds(entities, Some(&self.selected)).map(|r| r.min) else {
            return;
        };
        let chosen: Vec<&Entity<'_>> = entities
            .iter()
            .filter(|e| self.selected.contains(&e.key))
            .collect();
        let used = [Column::Left, Column::Middle, Column::Right]
            .into_iter()
            .filter(|c| chosen.iter().any(|e| e.column == *c));
        for (index, column) in used.enumerate() {
            let x = origin.x + index as f32 * (NODE_WIDTH + COLUMN_GAP);
            let mut y = origin.y;
            for e in chosen.iter().filter(|e| e.column == column) {
                self.positions.insert(e.key.clone(), pos2(x, y));
                y += e.height() + ROW_GAP;
            }
        }
    }

    /// Ctrl+F: a field that lists boxes by name; picking one centres it and makes it glow.
    fn search_field(&mut self, ui: &mut Ui, entities: &[Entity<'_>]) {
        let id = ui.make_persistent_id("graph-search");
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .id(id)
                .hint_text(t("Find a box (Ctrl+F)"))
                .desired_width(160.0),
        );
        if field.gained_focus() || field.changed() {
            self.search_open = true;
        }
        let query = self.search.to_lowercase();
        let found: Vec<&Entity<'_>> = entities
            .iter()
            .filter(|e| {
                !query.is_empty()
                    && (e.title.to_lowercase().contains(&query)
                        || e.key.to_lowercase().contains(&query))
            })
            .take(SEARCH_RESULTS)
            .collect();
        if !self.search_open || found.is_empty() {
            return;
        }
        let mut pick = None;
        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            pick = found.first().map(|e| e.key.clone());
        }
        let area = egui::Area::new(id.with("results"))
            .order(egui::Order::Foreground)
            .fixed_pos(field.rect.left_bottom() + vec2(0.0, 2.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    for e in &found {
                        if ui.button(&e.title).clicked() {
                            pick = Some(e.key.clone());
                        }
                    }
                })
            });
        let dismissed = ui.input(|i| i.key_pressed(egui::Key::Escape))
            || (area.response.clicked_elsewhere() && !field.hovered());
        if let Some(key) = pick {
            if let Some(rect) = self.bounds(entities, Some(&HashSet::from([key.clone()]))) {
                self.fit_to = Some(Rect::from_center_size(
                    rect.center(),
                    self.scene_rect.size(),
                ));
                self.selected = HashSet::from([key.clone()]);
                self.flash = Some((key, ui.input(|i| i.time)));
            }
            self.search_open = false;
        } else if dismissed {
            self.search_open = false;
        }
    }

    fn canvas(
        &mut self,
        ui: &mut Ui,
        graph: &Graph,
        entities: &[Entity<'_>],
        commands: &mut Vec<Command>,
    ) {
        grid(ui);
        self.selected
            .retain(|key| entities.iter().any(|e| e.key == *key));
        self.selected_links
            .retain(|id| graph.links.iter().any(|l| l.id == *id));
        let additive = ui.input(|i| i.modifiers.shift);
        let keys = !ui.ctx().egui_wants_keyboard_input();
        if keys && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            // Esc first drops what is being drawn, and only then the selection.
            if self.dragging.is_some() || self.band.is_some() {
                self.dragging = None;
                self.band = None;
            } else {
                self.selected.clear();
                self.selected_links.clear();
            }
        }
        if keys && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A)) {
            self.selected = entities.iter().map(|e| e.key.clone()).collect();
            self.selected_links = graph.links.iter().map(|l| l.id).collect();
        }
        if keys && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::G)) {
            self.group_selected();
        }
        if keys && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::D)) {
            self.selected.clear();
            self.selected_links.clear();
        }
        if keys && ui.input(|i| !i.modifiers.command && i.key_pressed(egui::Key::F)) {
            let only = ui.input(|i| i.modifiers.shift) && !self.selected.is_empty();
            self.fit(entities, only);
        }
        if keys && !self.selected.is_empty() {
            let (step, nudge) = ui.input(|i| {
                let step = if i.modifiers.shift {
                    NUDGE * 4.0
                } else {
                    NUDGE
                };
                let arrows = [
                    (egui::Key::ArrowLeft, vec2(-1.0, 0.0)),
                    (egui::Key::ArrowRight, vec2(1.0, 0.0)),
                    (egui::Key::ArrowUp, vec2(0.0, -1.0)),
                    (egui::Key::ArrowDown, vec2(0.0, 1.0)),
                ];
                let nudge = arrows
                    .into_iter()
                    .filter(|(key, _)| i.key_pressed(*key))
                    .fold(egui::Vec2::ZERO, |sum, (_, dir)| sum + dir);
                (step, nudge)
            });
            if nudge != egui::Vec2::ZERO {
                let before = self.positions.clone();
                for key in &self.selected {
                    if let Some(pos) = self.positions.get_mut(key) {
                        *pos += nudge * step;
                    }
                }
                self.layout_change = Some((before, self.positions.clone()));
            }
        }
        // Boxes and ports sit on top of the marquee; with Space held the scene pans instead.
        // Click and drag: a drag-only sense would start the marquee on press and swallow
        // the click that selects a link.
        let band = (!pan_held(ui) || self.band.is_some()).then(|| {
            ui.interact(
                ui.clip_rect(),
                ui.scope_id().with("selection-band"),
                Sense::click_and_drag(),
            )
        });
        let rects: Vec<Rect> = entities
            .iter()
            .map(|e| {
                let pos = self.positions.get(&e.key).copied().unwrap_or(Pos2::ZERO);
                Rect::from_min_size(pos, vec2(NODE_WIDTH, e.height()))
            })
            .collect();

        let mut anchors: HashMap<PortId, Anchor> = HashMap::new();
        // Which box each port is drawn on, so a link never goes from a box to itself.
        let mut owner: HashMap<PortId, usize> = HashMap::new();
        for (index, e) in entities.iter().enumerate() {
            for port in e.inputs.iter().chain(&e.outputs) {
                owner.insert(port.id, index);
            }
        }
        for (e, rect) in entities.iter().zip(&rects) {
            for (row, port) in e.inputs.iter().enumerate() {
                anchors.insert(
                    port.id,
                    Anchor {
                        pos: pos2(rect.left(), port_y(*rect, row)),
                        direction: Direction::Input,
                        color: e.tint,
                        pinned: e.pinned,
                    },
                );
            }
            for (row, port) in e.outputs.iter().enumerate() {
                anchors.insert(
                    port.id,
                    Anchor {
                        pos: pos2(rect.right(), port_y(*rect, row)),
                        direction: Direction::Output,
                        color: e.tint,
                        pinned: e.pinned,
                    },
                );
            }
        }

        let pointer = scene_pointer(ui);
        let marquee = self
            .band
            .zip(pointer)
            .map(|(band, pointer)| (band.adds, Rect::from_two_pos(band.start, pointer)));
        let mut hovering_widget = false;
        let mut moved = None;
        let mut grabbing = false;
        let mut move_ended = false;
        let now = ui.input(|i| i.time);
        // Open groups: a frame behind their boxes, its title bar a handle for them all.
        let mut group_moved = None;
        let mut fold = None;
        for index in 0..self.groups.len() {
            let group = &self.groups[index];
            if group.collapsed {
                continue;
            }
            let Some(frame) = groups::frame(
                entities
                    .iter()
                    .zip(&rects)
                    .filter(|(e, _)| group.members.contains(&e.key))
                    .map(|(_, r)| *r),
            ) else {
                continue;
            };
            let id = group.id;
            let response = ui
                .interact(
                    groups::title_bar(frame),
                    ui.scope_id().with(("group", id)),
                    Sense::click_and_drag(),
                )
                .on_hover_text(t(
                    "drag to move the group · double-click to fold it · right-click for more",
                ));
            groups::paint_frame(ui, group, frame, response.hovered() || response.dragged());
            if response.drag_started() {
                self.layout_before = Some(self.positions.clone());
            }
            if response.dragged() {
                group_moved = Some((id, response.drag_delta()));
                grabbing = true;
            }
            move_ended |= response.drag_stopped();
            hovering_widget |= response.hovered();
            if response.double_clicked() {
                fold = Some((id, true));
            }
            response.context_menu(|ui| self.group_menu(ui, id));
        }
        if let Some((id, delta)) = group_moved
            && let Some(group) = self.groups.iter().find(|g| g.id == id)
        {
            for key in &group.members {
                if let Some(pos) = self.positions.get_mut(key) {
                    *pos += delta;
                }
            }
        }
        for (e, rect) in entities.iter().zip(&rects) {
            let response = ui.interact(*rect, ui.scope_id().with(&e.key), Sense::click_and_drag());
            let folded = self.groups.iter().find(|g| g.key() == e.key).map(|g| g.id);
            if response.drag_started() {
                self.layout_before = Some(self.positions.clone());
            }
            if response.dragged() {
                moved = Some((e.key.clone(), response.drag_delta()));
                grabbing = true;
            }
            move_ended |= response.drag_stopped();
            if response.clicked() {
                if additive {
                    if !self.selected.remove(&e.key) {
                        self.selected.insert(e.key.clone());
                    }
                } else {
                    self.selected = HashSet::from([e.key.clone()]);
                    self.selected_links.clear();
                }
            }
            let hovered = response.hovered() || response.dragged();
            hovering_widget |= hovered;
            let selected = self.selected.contains(&e.key);
            let selected = marquee.map_or(selected, |(adds, area)| {
                marquee_selects(adds, selected, area.intersects(*rect))
            });
            paint_box(ui, e, *rect, hovered, selected);
            if let Some((key, at)) = &self.flash
                && *key == e.key
                && now - at < FLASH_SECS
            {
                let fade = 1.0 - ((now - at) / FLASH_SECS) as f32;
                ui.painter().rect_stroke(
                    rect.expand(4.0),
                    theme::CORNER_RADIUS + egui::CornerRadius::same(4),
                    Stroke::new(3.0, theme::ACCENT.gamma_multiply(fade)),
                    StrokeKind::Outside,
                );
                ui.ctx().request_repaint();
            }
            if let Some(chain) = graph.chain(&e.key) {
                let peaks = smoothed(ui, (&e.key, "graph-level"), chain.meters.output());
                paint_level(ui, e, *rect, peaks);
            }
            if response.double_clicked()
                && let Some(id) = folded
            {
                fold = Some((id, false));
            } else if response.double_clicked() {
                self.open = Some(if graph.chain(&e.key).is_some() {
                    Opens::Chains
                } else if e.nodes.iter().any(|n| n.is_stream() && n.app.is_some()) {
                    Opens::Apps
                } else {
                    Opens::Devices
                });
            }
            let response = response.on_hover_text(hover_text(graph, e));
            if let Some(id) = folded {
                response
                    .on_hover_text(t("double-click to open the group"))
                    .context_menu(|ui| self.group_menu(ui, id));
                continue;
            }
            let response = response.on_hover_text(t("double-click to configure"));
            response.context_menu(|ui| {
                if self.selected.len() >= 2
                    && self.selected.contains(&e.key)
                    && ui
                        .button(tf("Group {n} boxes", &[("n", &self.selected.len())]))
                        .clicked()
                {
                    self.group_selected();
                    ui.close();
                    return;
                }
                let (unlink, delete) = removal(graph, e);
                if ui
                    .add_enabled(!unlink.is_empty(), Button::new(t("Remove links")))
                    .clicked()
                {
                    commands.extend(unlink);
                    ui.close();
                }
                if let Some(delete) = delete
                    && ui
                        .button(tf("Delete {name}…", &[("name", &e.title)]))
                        .clicked()
                {
                    self.confirm = Some(vec![(e.title.clone(), delete)]);
                    ui.close();
                }
            });
        }

        if let Some((id, collapse)) = fold {
            self.fold(id, collapse);
        }
        // Dragging a selected box carries the rest of the selection with it.
        if let Some((key, delta)) = moved {
            let group: Vec<String> = if self.selected.contains(&key) {
                self.selected.iter().cloned().collect()
            } else {
                vec![key]
            };
            for key in group {
                if let Some(pos) = self.positions.get_mut(&key) {
                    *pos += delta;
                }
            }
        }
        if move_ended && let Some(before) = self.layout_before.take() {
            self.layout_change = Some((before, self.positions.clone()));
        }

        for (port, anchor) in &anchors {
            let hit = Rect::from_center_size(anchor.pos, vec2(PORT_HIT, PORT_HIT));
            let response = ui.interact(hit, ui.scope_id().with(("port", port)), Sense::drag());
            hovering_widget |= response.hovered();
            if response.hovered() && self.dragging.is_none() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.hovered() || response.dragged() {
                ui.painter()
                    .circle_filled(anchor.pos, PORT_RADIUS + 1.5, theme::TEXT);
            }
            if response.drag_started() {
                self.dragging = Some(DragFrom {
                    port: *port,
                    direction: anchor.direction,
                    anchor: anchor.pos,
                });
            }
        }

        let zoom = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .map_or(1.0, |t| t.scaling);
        let targets: Vec<PortId> = self.dragging.as_ref().map_or_else(Vec::new, |from| {
            anchors
                .keys()
                .copied()
                .filter(|target| can_take(graph, &owner, from, *target, &anchors))
                .collect()
        });
        let snapped = match (&self.dragging, pointer) {
            (Some(from), Some(pointer)) => snap(
                graph,
                from,
                pointer,
                SNAP_PX / zoom,
                &targets,
                &anchors,
                entities,
                &rects,
            ),
            _ => None,
        };
        if self.dragging.is_some() {
            for (port, anchor) in &anchors {
                if *port == self.dragging.as_ref().map_or(*port, |f| f.port) {
                    continue;
                }
                if Some(*port) == snapped {
                    ui.painter()
                        .circle_filled(anchor.pos, PORT_RADIUS + 3.0, theme::ACCENT);
                } else if targets.contains(port) {
                    ui.painter().circle_stroke(
                        anchor.pos,
                        PORT_RADIUS + 3.0,
                        Stroke::new(1.5, theme::ACCENT),
                    );
                } else {
                    ui.painter().circle_filled(
                        anchor.pos,
                        PORT_RADIUS + 1.0,
                        theme::PANEL.gamma_multiply(0.85),
                    );
                }
            }
        }

        let hovered_link = if hovering_widget || self.dragging.is_some() {
            None
        } else {
            pointer.and_then(|p| nearest_link(graph, &anchors, p, LINK_HIT_PX / zoom))
        };
        for link in &graph.links {
            // Inside a folded group: both ends are on its one box.
            if owner.contains_key(&link.output) && owner.get(&link.output) == owner.get(&link.input)
            {
                continue;
            }
            if let (Some(from), Some(to)) = (anchors.get(&link.output), anchors.get(&link.input)) {
                let hovered = hovered_link == Some(link.id);
                let selected = self.selected_links.contains(&link.id);
                let selected = marquee.map_or(selected, |(adds, area)| {
                    marquee_selects(adds, selected, link_touches(area, from.pos, to.pos))
                });
                let stroke = if selected {
                    Stroke::new(LINK_WIDTH * 1.8, theme::ACCENT)
                } else if hovered {
                    Stroke::new(LINK_WIDTH * 1.6, theme::TEXT)
                } else {
                    Stroke::new(LINK_WIDTH, from.color)
                };
                let curve = bezier(from.pos, to.pos, stroke);
                if from.pinned || to.pinned {
                    let points: Vec<Pos2> = (0..=LINK_SAMPLES * 2)
                        .map(|i| curve.sample(i as f32 / (LINK_SAMPLES * 2) as f32))
                        .collect();
                    ui.painter()
                        .extend(Shape::dashed_line(&points, stroke, 6.0, 4.0));
                } else {
                    ui.painter().add(curve);
                }
            }
        }
        if let Some(link) = hovered_link.and_then(|id| graph.links.iter().find(|l| l.id == id)) {
            for end in [link.output, link.input] {
                if let Some(anchor) = anchors.get(&end) {
                    ui.painter()
                        .circle_filled(anchor.pos, PORT_RADIUS + 2.0, theme::ACCENT);
                }
            }
            if let Some(text) = link_text(graph, link) {
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    ui.scope_id().with("link-tip"),
                    egui::PopupAnchor::Pointer,
                )
                .show(|ui| ui.label(text));
            }
        }
        if ui.input(|i| {
            i.pointer
                .button_double_clicked(egui::PointerButton::Primary)
        }) && !hovering_widget
            && hovered_link.is_none()
            && pointer.is_some()
        {
            self.fit(entities, false);
        }
        let clicked_empty = ui.input(|i| i.pointer.primary_clicked())
            && !hovering_widget
            && self.dragging.is_none()
            && band.as_ref().is_none_or(|b| !b.dragged());
        match hovered_link {
            Some(id) if clicked_empty && additive => {
                if !self.selected_links.remove(&id) {
                    self.selected_links.insert(id);
                }
            }
            Some(id) if clicked_empty => {
                self.selected_links = HashSet::from([id]);
                self.selected.clear();
            }
            None if clicked_empty && !additive && pointer.is_some() => {
                self.selected.clear();
                self.selected_links.clear();
            }
            _ => {}
        }
        if let Some(band) = band {
            self.selection_band(ui, &band, pointer, entities, &rects, graph, &anchors);
        }
        if let Some(id) = hovered_link
            && ui.input(|i| i.pointer.secondary_clicked())
            && let Some(at) = ui.ctx().pointer_latest_pos()
        {
            self.selected_links = HashSet::from([id]);
            self.selected.clear();
            self.link_menu = Some((id, at));
        }
        self.link_menu_ui(ui, commands);
        if keys
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        {
            self.delete_selected(graph, entities, commands);
        }
        let cursor = if self.dragging.is_some() || self.band.is_some() {
            Some(egui::CursorIcon::Crosshair)
        } else if grabbing {
            Some(egui::CursorIcon::Grabbing)
        } else if hovered_link.is_some() {
            Some(egui::CursorIcon::PointingHand)
        } else if pan_held(ui) && pointer.is_some() {
            Some(egui::CursorIcon::Grab)
        } else {
            None
        };
        if let Some(cursor) = cursor {
            ui.ctx().set_cursor_icon(cursor);
        }

        if let (Some(from), Some(pointer)) = (&self.dragging, pointer) {
            let end = snapped
                .and_then(|p| anchors.get(&p))
                .map_or(pointer, |a| a.pos);
            let (a, b) = match from.direction {
                Direction::Output => (from.anchor, end),
                Direction::Input => (end, from.anchor),
            };
            let color = if snapped.is_some() {
                theme::ACCENT
            } else {
                theme::TEXT
            };
            ui.painter()
                .add(bezier(a, b, Stroke::new(LINK_WIDTH, color)));
        }
        // Released: the drag ends either way, and links where it snapped.
        if ui.input(|i| i.pointer.any_released())
            && let (Some(from), Some(target)) = (self.dragging.take(), snapped)
        {
            let link = link_command(graph, &from, target);
            match heard(graph, &link) {
                Some(joins) => self.feedback = Some((link, joins)),
                None => commands.push(link),
            }
        }
    }
}

impl GraphView {
    /// F: everything in view; Shift+F (`only` selected): the selection.
    fn fit(&mut self, entities: &[Entity<'_>], only: bool) {
        let keys = only.then_some(&self.selected);
        if let Some(bounds) = self.bounds(entities, keys) {
            self.fit_to = Some(bounds.expand(FIT_MARGIN));
        }
    }

    /// The marquee: on release the boxes it touches and the links it holds are selected,
    /// added or taken away, by the modifiers held when it started.
    #[allow(clippy::too_many_arguments)]
    fn selection_band(
        &mut self,
        ui: &Ui,
        band: &egui::Response,
        pointer: Option<Pos2>,
        entities: &[Entity<'_>],
        rects: &[Rect],
        graph: &Graph,
        anchors: &HashMap<PortId, Anchor>,
    ) {
        if band.drag_started_by(egui::PointerButton::Primary)
            && let Some(start) = pointer
        {
            self.band = Some(Band {
                start,
                adds: ui.input(|i| i.modifiers.shift),
            });
        }
        let (Some(Band { start, adds }), Some(pointer)) = (self.band, pointer) else {
            return;
        };
        let area = Rect::from_two_pos(start, pointer);
        ui.painter().rect(
            area,
            theme::CORNER_RADIUS,
            theme::ACCENT.gamma_multiply(0.08),
            Stroke::new(1.0, theme::ACCENT),
            StrokeKind::Inside,
        );
        if !band.drag_stopped() {
            return;
        }
        self.band = None;
        for (e, rect) in entities.iter().zip(rects) {
            let was = self.selected.contains(&e.key);
            if marquee_selects(adds, was, area.intersects(*rect)) {
                self.selected.insert(e.key.clone());
            } else {
                self.selected.remove(&e.key);
            }
        }
        for link in &graph.links {
            let inside = match (anchors.get(&link.output), anchors.get(&link.input)) {
                (Some(a), Some(b)) => link_touches(area, a.pos, b.pos),
                _ => false,
            };
            if marquee_selects(adds, self.selected_links.contains(&link.id), inside) {
                self.selected_links.insert(link.id);
            } else {
                self.selected_links.remove(&link.id);
            }
        }
    }
}

impl GraphView {
    /// The menu of a right-clicked link: Remove link.
    fn link_menu_ui(&mut self, ui: &Ui, commands: &mut Vec<Command>) {
        let Some((id, at)) = self.link_menu else {
            return;
        };
        let area = egui::Area::new(ui.scope_id().with("link-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    if ui.button(t("Remove link")).clicked() {
                        commands.push(Command::Unlink { link: id });
                        self.link_menu = None;
                    }
                })
            });
        if area.response.clicked_elsewhere() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.link_menu = None;
        }
    }

    /// Delete: selected links go at once (Ctrl+Z brings them back); a selected chain or
    /// virtual device is deleted after a confirmation; any other box loses its links.
    fn delete_selected(
        &mut self,
        graph: &Graph,
        entities: &[Entity<'_>],
        commands: &mut Vec<Command>,
    ) {
        let mut unlink: Vec<u32> = self.selected_links.iter().copied().collect();
        let mut delete = Vec::new();
        // A folded group's box stands for its boxes; Delete leaves them alone.
        for e in entities
            .iter()
            .filter(|e| self.selected.contains(&e.key) && !e.key.starts_with(GROUP_KEY))
        {
            match removal(graph, e) {
                (_, Some(command)) => delete.push((e.title.clone(), command)),
                (links, None) => unlink.extend(links.iter().filter_map(|c| match c {
                    Command::Unlink { link } => Some(*link),
                    _ => None,
                })),
            }
        }
        unlink.sort_unstable();
        unlink.dedup();
        commands.extend(unlink.into_iter().map(|link| Command::Unlink { link }));
        if !delete.is_empty() {
            self.confirm = Some(delete);
        }
        self.selected_links.clear();
    }

    /// Asks before deleting chains and virtual devices, which Ctrl+Z cannot bring back.
    fn confirm_dialog(&mut self, ui: &Ui, commands: &mut Vec<Command>) {
        let Some(items) = &self.confirm else {
            return;
        };
        let names: Vec<&str> = items.iter().map(|(name, _)| name.as_str()).collect();
        let mut answer = None;
        let modal = egui::Modal::new(ui.scope_id().with("graph-delete")).show(ui.ctx(), |ui| {
            ui.set_width(360.0);
            ui.label(
                RichText::new(tf("Delete {names}?", &[("names", &names.join(", "))]))
                    .size(15.0)
                    .strong(),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(t(
                    "A chain goes with its slots and plugin settings, a virtual \
                    device with what is routed to it. This cannot be undone.",
                ))
                .size(12.5)
                .color(theme::TEXT_DIM),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let delete = Button::new(RichText::new(t("Delete")).color(theme::RED_SOFT))
                        .fill(theme::RED_FILL)
                        .stroke(Stroke::new(1.0, theme::RED));
                    if ui.add(delete).clicked() {
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
        if let Some(yes) = answer
            && let Some(items) = self.confirm.take()
            && yes
        {
            commands.extend(items.into_iter().map(|(_, command)| command));
            self.selected.clear();
        }
    }
}

/// What removing a box means: its links as Unlink commands, and the deletion when the
/// box is something Canalis made (a chain, a virtual device) rather than hardware or an
/// app's stream.
fn removal(graph: &Graph, e: &Entity<'_>) -> (Vec<Command>, Option<Command>) {
    let ports: HashSet<PortId> = e.inputs.iter().chain(&e.outputs).map(|p| p.id).collect();
    let unlink = graph
        .links
        .iter()
        .filter(|l| ports.contains(&l.input) || ports.contains(&l.output))
        .map(|l| Command::Unlink { link: l.id })
        .collect();
    let delete = if graph.chain(&e.key).is_some() {
        Some(Command::DeleteChain {
            name: e.key.clone(),
        })
    } else {
        match e.nodes.as_slice() {
            [node]
                if node.is_virtual_device() && graph.chain(chain_name_of(&node.name)).is_none() =>
            {
                Some(Command::DeleteVirtual {
                    name: node.name.clone(),
                })
            }
            _ => None,
        }
    };
    (unlink, delete)
}

/// Ctrl or Space held: a left drag pans instead of drawing the marquee.
fn pan_held(ui: &Ui) -> bool {
    ui.input(|i| i.modifiers.command || i.modifiers.ctrl)
        || (!ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_down(egui::Key::Space)))
}

fn toggle(ui: &mut Ui, value: &mut bool, text: &str) {
    let (fill, color) = if *value {
        (theme::SELECTION, theme::TEXT)
    } else {
        (theme::PANEL, theme::TEXT_DIM)
    };
    if ui
        .add(
            Button::new(RichText::new(text).size(12.0).color(color))
                .fill(fill)
                .stroke(Stroke::new(1.0, theme::BORDER)),
        )
        .clicked()
    {
        *value = !*value;
    }
}

/// `canalis.x` for `canalis.x`, `canalis.x.out` and `canalis.x.in`.
fn chain_name_of(node_name: &str) -> &str {
    node_name
        .strip_suffix(".out")
        .or_else(|| node_name.strip_suffix(".in"))
        .or_else(|| node_name.strip_suffix(".source"))
        .unwrap_or(node_name)
}

fn is_default(graph: &Graph, node: &Node) -> bool {
    match node.class {
        MediaClass::Sink => graph.defaults.sink.as_deref() == Some(node.name.as_str()),
        MediaClass::Source => graph.defaults.source.as_deref() == Some(node.name.as_str()),
        _ => false,
    }
}

/// The pointer in scene coordinates, when it is over the canvas.
fn scene_pointer(ui: &Ui) -> Option<Pos2> {
    let global = ui.ctx().pointer_latest_pos()?;
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id())?;
    let local = to_global.inverse() * global;
    ui.clip_rect().contains(local).then_some(local)
}

/// MIDI ports carry no channel and sit on non-audio nodes; audio only links to audio.
fn is_midi(graph: &Graph, port: &Port) -> bool {
    port.channel.is_none()
        && graph
            .node(port.node)
            .is_none_or(|n| n.class == MediaClass::Other)
}

/// Whether a link dragged from `from` may end on `target`: the other direction, the same
/// kind of signal, another box, and not linked already.
fn can_take(
    graph: &Graph,
    owner: &HashMap<PortId, usize>,
    from: &DragFrom,
    target: PortId,
    anchors: &HashMap<PortId, Anchor>,
) -> bool {
    let (Some(source), Some(port)) = (graph.port(from.port), graph.port(target)) else {
        return false;
    };
    let (output, input) = match from.direction {
        Direction::Output => (from.port, target),
        Direction::Input => (target, from.port),
    };
    anchors
        .get(&target)
        .is_some_and(|a| a.direction != from.direction)
        && owner.get(&target) != owner.get(&from.port)
        && is_midi(graph, source) == is_midi(graph, port)
        && !graph
            .links
            .iter()
            .any(|l| l.output == output && l.input == input)
}

/// Where a link released at `pointer` lands: the nearest port that can take it within
/// `radius`, else, over a box, that box's best port for it (the same channel first, then
/// one not linked yet, then the first).
#[allow(clippy::too_many_arguments)]
fn snap(
    graph: &Graph,
    from: &DragFrom,
    pointer: Pos2,
    radius: f32,
    targets: &[PortId],
    anchors: &HashMap<PortId, Anchor>,
    entities: &[Entity<'_>],
    rects: &[Rect],
) -> Option<PortId> {
    let near = targets
        .iter()
        .filter_map(|p| Some((*p, anchors.get(p)?.pos.distance(pointer))))
        .filter(|(_, d)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((port, _)) = near {
        return Some(port);
    }
    let (entity, _) = entities
        .iter()
        .zip(rects)
        .find(|(_, rect)| rect.contains(pointer))?;
    let channel = graph.port(from.port).and_then(|p| p.channel.clone());
    let ports = match from.direction {
        Direction::Output => &entity.inputs,
        Direction::Input => &entity.outputs,
    };
    let linked = |p: &Port| {
        graph
            .links
            .iter()
            .any(|l| l.input == p.id || l.output == p.id)
    };
    ports
        .iter()
        .filter(|p| targets.contains(&p.id))
        .min_by_key(|p| (channel.is_none() || p.channel != channel, linked(p)))
        .map(|p| p.id)
}

/// What linking `from` to `target` asks for: an app's stream dropped on a device is sent
/// there through WirePlumber (`target.object`), like the Apps picker; anything else is a
/// link.
fn link_command(graph: &Graph, from: &DragFrom, target: PortId) -> Command {
    let (output, input) = match from.direction {
        Direction::Output => (from.port, target),
        Direction::Input => (target, from.port),
    };
    let node_of = |port: PortId| graph.port(port).and_then(|p| graph.node(p.node));
    if let (Some(out), Some(inp)) = (node_of(output), node_of(input)) {
        match (out.class, inp.class) {
            (MediaClass::PlaybackStream, MediaClass::Sink) if out.app.is_some() => {
                return Command::SetTarget {
                    stream: out.id,
                    target: Some(inp.name.clone()),
                };
            }
            (MediaClass::Source, MediaClass::CaptureStream) if inp.app.is_some() => {
                return Command::SetTarget {
                    stream: inp.id,
                    target: Some(out.name.clone()),
                };
            }
            _ => {}
        }
    }
    Command::Link { output, input }
}

/// For a link that lets a real mic reach real speakers, the two ends it joins.
fn heard(graph: &Graph, link: &Command) -> Option<String> {
    let Command::Link { output, input } = link else {
        return None;
    };
    let node_of = |port: &PortId| graph.port(*port).and_then(|p| graph.node(p.node));
    let (out, inp) = (node_of(output)?, node_of(input)?);
    let configs: Vec<ChainConfig> = graph.chains.iter().map(ChainState::config).collect();
    let (from, to) = (&out.label, &inp.label);
    feedback::link_heard(graph, &configs, &out.name, &inp.name)
        .then(|| tf("This links {from} to {to}.", &[("from", from), ("to", to)]))
}

fn nearest_link(
    graph: &Graph,
    anchors: &HashMap<PortId, Anchor>,
    pointer: Pos2,
    radius: f32,
) -> Option<u32> {
    graph
        .links
        .iter()
        .filter_map(|link| {
            let from = anchors.get(&link.output)?.pos;
            let to = anchors.get(&link.input)?.pos;
            let shape = bezier(from, to, Stroke::NONE);
            let distance = (0..=LINK_SAMPLES)
                .map(|i| {
                    shape
                        .sample(i as f32 / LINK_SAMPLES as f32)
                        .distance(pointer)
                })
                .fold(f32::INFINITY, f32::min);
            (distance <= radius).then_some((link.id, distance))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

fn bezier(from: Pos2, to: Pos2, stroke: Stroke) -> egui::epaint::CubicBezierShape {
    let reach = ((to.x - from.x).abs() * 0.5).max(40.0);
    egui::epaint::CubicBezierShape::from_points_stroke(
        [from, from + vec2(reach, 0.0), to - vec2(reach, 0.0), to],
        false,
        Color32::TRANSPARENT,
        stroke,
    )
}

fn port_y(rect: Rect, row: usize) -> f32 {
    rect.top() + HEADER_HEIGHT + (row as f32 + 0.5) * PORT_ROW
}

/// Faint lines every `GRID_STEP` scene units, thinned out when zoomed far away.
fn grid(ui: &Ui) {
    let zoom = ui
        .ctx()
        .layer_transform_to_global(ui.layer_id())
        .map_or(1.0, |t| t.scaling);
    let mut step = GRID_STEP;
    while step * zoom < MIN_GRID_PX {
        step *= 2.0;
    }
    let clip = ui.clip_rect();
    let painter = ui.painter();
    let stroke = Stroke::new(1.0 / zoom, theme::TEXT_DIM.gamma_multiply(0.12));
    let mut x = (clip.left() / step).floor() * step;
    while x <= clip.right() {
        painter.vline(x, clip.y_range(), stroke);
        x += step;
    }
    let mut y = (clip.top() / step).floor() * step;
    while y <= clip.bottom() {
        painter.hline(clip.x_range(), y, stroke);
        y += step;
    }
}

fn paint_box(ui: &Ui, e: &Entity<'_>, rect: Rect, hovered: bool, selected: bool) {
    let painter = ui.painter();
    let tinted = e.tint != PLAIN;
    let outline = if selected {
        theme::ACCENT
    } else if hovered {
        theme::TEXT
    } else if tinted {
        e.tint
    } else {
        theme::BORDER
    };
    painter.rect(
        rect,
        theme::CORNER_RADIUS,
        theme::PANEL,
        Stroke::new(if selected { 2.0 } else { 1.0 }, outline),
        StrokeKind::Inside,
    );
    let header = Rect::from_min_size(rect.min, vec2(NODE_WIDTH, HEADER_HEIGHT));
    painter.hline(
        header.x_range(),
        header.bottom(),
        Stroke::new(1.0, theme::BORDER_SOFT),
    );

    let mut right = rect.right() - 8.0;
    if e.is_default {
        let galley = painter.layout_no_wrap("★".to_owned(), FontId::proportional(12.0), STAR);
        right -= galley.size().x;
        text_center_left(painter, pos2(right, header.center().y), galley, STAR);
        right -= 6.0;
    }
    let chip_rect = chip(
        painter,
        pos2(right, header.center().y),
        &e.subtitle,
        if tinted { e.tint } else { theme::TEXT_DIM },
    );
    let title_color = if tinted { e.tint } else { theme::TEXT };
    let title = truncated(
        ui,
        &e.title,
        FontId::proportional(TITLE_FONT),
        title_color,
        chip_rect.left() - 8.0 - (rect.left() + 8.0),
    );
    text_center_left(
        ui.painter(),
        pos2(rect.left() + 8.0, header.center().y),
        title,
        title_color,
    );

    let half = NODE_WIDTH / 2.0 - 14.0;
    for (row, port) in e.inputs.iter().enumerate() {
        let y = port_y(rect, row);
        let name = truncated(
            ui,
            port_label(port),
            FontId::proportional(PORT_FONT),
            theme::TEXT_DIM,
            half,
        );
        let painter = ui.painter();
        painter.circle_filled(pos2(rect.left(), y), PORT_RADIUS, e.tint);
        text_center_left(painter, pos2(rect.left() + 10.0, y), name, theme::TEXT_DIM);
    }
    for (row, port) in e.outputs.iter().enumerate() {
        let y = port_y(rect, row);
        let dot = if port.monitor { theme::BORDER } else { e.tint };
        let name = truncated(
            ui,
            port_label(port),
            FontId::proportional(PORT_FONT),
            theme::TEXT_DIM,
            half,
        );
        let width = name.size().x;
        let painter = ui.painter();
        painter.circle_filled(pos2(rect.right(), y), PORT_RADIUS, dot);
        text_center_left(
            painter,
            pos2(rect.right() - 10.0 - width, y),
            name,
            theme::TEXT_DIM,
        );
    }
}

/// Above this the bar turns orange: close to clipping.
const LEVEL_HOT_DB: f32 = -3.0;

/// A chain box's output level: a thin bar along the bottom of its header.
fn paint_level(ui: &Ui, e: &Entity<'_>, rect: Rect, peaks: [f32; 2]) {
    let (fill, hot) = level(peaks);
    let track = Rect::from_min_max(
        pos2(rect.left() + 8.0, rect.top() + HEADER_HEIGHT - 4.0),
        pos2(rect.right() - 8.0, rect.top() + HEADER_HEIGHT - 2.0),
    );
    let painter = ui.painter();
    painter.rect_filled(track, 1.0, theme::BORDER_SOFT);
    if fill > 0.0 {
        let lit = Rect::from_min_size(track.min, vec2(track.width() * fill, track.height()));
        painter.rect_filled(lit, 1.0, if hot { theme::ORANGE } else { e.tint });
    }
}

/// The louder channel's peak as a share of the bar, and whether it is near clipping.
fn level(peaks: [f32; 2]) -> (f32, bool) {
    let db = 20.0 * peaks[0].max(peaks[1]).max(1e-6).log10();
    let fill = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
    (fill, db > LEVEL_HOT_DB)
}

/// "Firefox → Desktop EQ (FL→FL, FR→FR)": the two boxes a link joins and every channel
/// pair between them.
fn link_text(graph: &Graph, link: &canalis_pw::Link) -> Option<String> {
    let ends = |l: &canalis_pw::Link| Some((graph.port(l.output)?, graph.port(l.input)?));
    let (out, inp) = ends(link)?;
    let (from, to) = (graph.node(out.node)?, graph.node(inp.node)?);
    let pairs: Vec<String> = graph
        .links
        .iter()
        .filter_map(ends)
        .filter(|(o, i)| o.node == from.id && i.node == to.id)
        .map(|(o, i)| format!("{}→{}", port_label(o), port_label(i)))
        .collect();
    let mut text = format!("{} → {} ({})", from.label, to.label, pairs.join(", "));
    // The whole path through this link: from the sources to its output port, then from
    // its input port to where the sound goes out, both as PipeWire sums them.
    let path = [out.upstream, inp.downstream];
    if path.iter().any(Option::is_some) {
        let samples: f64 = path
            .iter()
            .flatten()
            .map(|l| l.in_samples(graph.clock))
            .sum();
        text.push('\n');
        text.push_str(&tf(
            "path latency {ms} ms (quantum {q} at {rate} Hz)",
            &[
                ("ms", &format!("{:.1}", graph.clock.ms(samples))),
                ("q", &graph.clock.quantum),
                ("rate", &graph.clock.rate),
            ],
        ));
    }
    Some(text)
}

fn port_label(port: &Port) -> &str {
    if port.monitor {
        return &port.name;
    }
    port.channel.as_deref().unwrap_or(&port.name)
}

fn hover_text(graph: &Graph, e: &Entity<'_>) -> String {
    let mut lines = vec![e.title.clone()];
    for node in &e.nodes {
        if let Some(app) = &node.app {
            lines.push(app.clone());
        }
        lines.push(node.name.clone());
    }
    // Over every port of the box: a chain's input ports carry its own delay downstream.
    let most = |side: fn(&Port) -> Option<canalis_pw::Latency>| {
        e.inputs
            .iter()
            .chain(&e.outputs)
            .filter_map(|p| side(p))
            .map(|l| l.in_samples(graph.clock))
            .fold(None, |max: Option<f64>, s| {
                Some(max.map_or(s, |m| m.max(s)))
            })
    };
    if let Some(samples) = most(|p| p.upstream).filter(|s| *s > 0.0) {
        lines.push(tf(
            "{ms} ms from where the sound comes in",
            &[("ms", &format!("{:.1}", graph.clock.ms(samples)))],
        ));
    }
    if let Some(samples) = most(|p| p.downstream).filter(|s| *s > 0.0) {
        lines.push(tf(
            "{ms} ms to where the sound goes out",
            &[("ms", &format!("{:.1}", graph.clock.ms(samples)))],
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marquee_replaces_the_selection_or_with_shift_adds_to_it() {
        let cases = [
            (false, [false, true, false, true]),
            (true, [false, true, true, true]),
        ];
        for (adds, expected) in cases {
            let got = [(false, false), (false, true), (true, false), (true, true)]
                .map(|(was, inside)| marquee_selects(adds, was, inside));
            assert_eq!(got, expected, "adds: {adds}");
        }
    }

    #[test]
    fn a_marquee_catches_a_link_it_crosses_even_with_both_ends_outside() {
        let (from, to) = (pos2(0.0, 0.0), pos2(400.0, 0.0));
        assert!(link_touches(
            Rect::from_min_max(pos2(180.0, -20.0), pos2(220.0, 20.0)),
            from,
            to
        ));
        assert!(!link_touches(
            Rect::from_min_max(pos2(180.0, 50.0), pos2(220.0, 90.0)),
            from,
            to
        ));
    }

    #[test]
    fn a_level_fills_by_the_louder_channel_in_db() {
        assert_eq!(level([0.0, 0.0]), (0.0, false), "silence");
        let (half, hot) = level([0.0, 0.031_622_8]);
        assert!(
            (half - 0.5).abs() < 1e-3 && !hot,
            "-30 dB is half way: {half}"
        );
        assert_eq!(level([1.0, 0.2]), (1.0, true), "0 dBFS is full and hot");
    }

    fn node(id: u32, name: &'static str, class: &'static str) -> Node {
        Node::from_props(id, |key| match key {
            "node.name" => Some(name),
            "media.class" => Some(class),
            _ => None,
        })
    }

    fn port(id: u32, node: u32, dir: &'static str, channel: Option<&'static str>) -> Port {
        let node = node.to_string();
        Port::from_props(id, |key| match key {
            "node.id" => Some(node.as_str()),
            "port.direction" => Some(dir),
            "port.name" => Some("p"),
            "audio.channel" => channel,
            _ => None,
        })
        .expect("a port")
    }

    /// A stereo app stream, a stereo sink and a MIDI node, with their anchors.
    fn scene() -> (Graph, HashMap<PortId, Anchor>, HashMap<PortId, usize>) {
        let mut graph = Graph::default();
        graph.nodes.push(node(1, "app", "Stream/Output/Audio"));
        graph.nodes.push(node(2, "speakers", "Audio/Sink"));
        graph.nodes.push(node(3, "midi", "Midi/Bridge"));
        graph.ports = vec![
            port(10, 1, "out", Some("FL")),
            port(11, 1, "out", Some("FR")),
            port(20, 2, "in", Some("FL")),
            port(21, 2, "in", Some("FR")),
            port(30, 3, "in", None),
        ];
        let mut anchors = HashMap::new();
        let mut owner = HashMap::new();
        for p in &graph.ports {
            let x = if p.direction == Direction::Output {
                0.0
            } else {
                300.0
            };
            anchors.insert(
                p.id,
                Anchor {
                    pos: pos2(x, p.id.0 as f32 * 10.0),
                    direction: p.direction,
                    color: PLAIN,
                    pinned: false,
                },
            );
            owner.insert(p.id, p.node.0 as usize);
        }
        (graph, anchors, owner)
    }

    #[test]
    fn a_hovered_link_names_both_boxes_and_every_channel_pair() {
        let (mut graph, _, _) = scene();
        for (id, output, input) in [(1, 10, 20), (2, 11, 21)] {
            graph.links.push(canalis_pw::Link {
                id,
                output: PortId(output),
                input: PortId(input),
                ours: false,
            });
        }
        let text = link_text(&graph, &graph.links[1]);
        assert_eq!(text.as_deref(), Some("app → speakers (FL→FL, FR→FR)"));

        // With PipeWire's latencies on both ends, the path through the link is their sum.
        let latency = |quanta, samples| canalis_pw::Latency {
            quanta,
            samples,
            ns: 0,
        };
        graph.clock = canalis_pw::Clock {
            rate: 48_000,
            quantum: 480,
        };
        for port in &mut graph.ports {
            match port.id {
                PortId(11) => port.upstream = Some(latency(0.0, 0)),
                PortId(21) => port.downstream = Some(latency(1.0, 240)),
                _ => {}
            }
        }
        let text = link_text(&graph, &graph.links[1]).unwrap();
        assert!(
            text.ends_with("path latency 15.0 ms (quantum 480 at 48000 Hz)"),
            "{text}"
        );
    }

    #[test]
    fn the_layout_survives_a_round_trip_through_disk() {
        let camera = Rect::from_min_max(pos2(-40.0, -40.0), pos2(800.5, 600.0));
        let positions = Layout::from([
            ("mic".to_owned(), pos2(0.0, 24.0)),
            ("canalis.desk eq".to_owned(), pos2(340.0, -12.5)),
        ]);
        let text = state_text(camera, &positions, &[]);
        assert_eq!(parse_state(&text), Some((camera, positions)));
        assert_eq!(
            parse_state("camera = [1.0]"),
            None,
            "a broken file is ignored"
        );
    }

    #[test]
    fn tidy_lays_out_only_the_selection_from_where_it_starts() {
        let entity = |key: &str, column| Entity {
            key: key.to_owned(),
            title: key.to_owned(),
            subtitle: String::new(),
            tint: PLAIN,
            nodes: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            column,
            is_default: false,
            pinned: false,
        };
        let entities = [
            entity("a", Column::Left),
            entity("b", Column::Right),
            entity("c", Column::Right),
            entity("d", Column::Left),
        ];
        let mut view = GraphView::default();
        for (key, at) in [
            ("a", (100.0, 50.0)),
            ("b", (900.0, 400.0)),
            ("c", (120.0, 300.0)),
        ] {
            view.positions.insert(key.into(), pos2(at.0, at.1));
        }
        view.positions.insert("d".into(), pos2(-500.0, -500.0));
        view.selected = ["a", "b", "c"].map(String::from).into();
        view.tidy_selection(&entities);
        let step = entities[0].height() + ROW_GAP;
        assert_eq!(view.positions["a"], pos2(100.0, 50.0));
        assert_eq!(
            view.positions["b"],
            pos2(100.0 + NODE_WIDTH + COLUMN_GAP, 50.0)
        );
        assert_eq!(
            view.positions["c"],
            pos2(100.0 + NODE_WIDTH + COLUMN_GAP, 50.0 + step)
        );
        assert_eq!(view.positions["d"], pos2(-500.0, -500.0), "not selected");
    }

    #[test]
    fn audio_links_only_to_audio_of_another_box_not_linked_yet() {
        let (mut graph, anchors, owner) = scene();
        let from = DragFrom {
            port: PortId(10),
            direction: Direction::Output,
            anchor: pos2(0.0, 100.0),
        };
        let can = |graph: &Graph, p: u32| can_take(graph, &owner, &from, PortId(p), &anchors);
        assert!(can(&graph, 20) && can(&graph, 21));
        assert!(!can(&graph, 11), "same direction, same box");
        assert!(!can(&graph, 30), "audio to MIDI");
        graph.links.push(canalis_pw::Link {
            id: 99,
            output: PortId(10),
            input: PortId(20),
            ours: false,
        });
        assert!(!can(&graph, 20), "already linked");
    }

    #[test]
    fn delete_removes_links_of_hardware_and_deletes_what_canalis_made() {
        let (mut graph, _, _) = scene();
        graph
            .nodes
            .push(node(4, "canalis.mic-b", "Audio/Source/Virtual"));
        graph.links.push(canalis_pw::Link {
            id: 7,
            output: PortId(10),
            input: PortId(20),
            ours: false,
        });
        let entity = |node_id: u32| {
            let n = graph.nodes.iter().find(|n| n.id.0 == node_id).unwrap();
            Entity {
                key: n.name.clone(),
                title: n.name.clone(),
                subtitle: String::new(),
                tint: PLAIN,
                nodes: vec![n],
                inputs: graph
                    .ports
                    .iter()
                    .filter(|p| p.node == n.id && p.direction == Direction::Input)
                    .collect(),
                outputs: graph
                    .ports
                    .iter()
                    .filter(|p| p.node == n.id && p.direction == Direction::Output)
                    .collect(),
                column: Column::Left,
                is_default: false,
                pinned: false,
            }
        };
        let (unlink, delete) = removal(&graph, &entity(2));
        assert_eq!(
            unlink,
            vec![Command::Unlink { link: 7 }],
            "the speakers lose their link"
        );
        assert!(delete.is_none(), "hardware cannot be deleted");
        let (_, delete) = removal(&graph, &entity(4));
        assert_eq!(
            delete,
            Some(Command::DeleteVirtual {
                name: "canalis.mic-b".into()
            })
        );
    }

    #[test]
    fn a_drop_on_a_box_picks_the_same_channel() {
        let (graph, anchors, owner) = scene();
        let from = DragFrom {
            port: PortId(11),
            direction: Direction::Output,
            anchor: pos2(0.0, 110.0),
        };
        let targets: Vec<PortId> = anchors
            .keys()
            .copied()
            .filter(|t| can_take(&graph, &owner, &from, *t, &anchors))
            .collect();
        let entities = vec![Entity {
            key: "speakers".into(),
            title: String::new(),
            subtitle: String::new(),
            tint: PLAIN,
            nodes: Vec::new(),
            inputs: graph.ports.iter().filter(|p| p.node.0 == 2).collect(),
            outputs: Vec::new(),
            column: Column::Right,
            is_default: false,
            pinned: false,
        }];
        let rects = [Rect::from_min_size(
            pos2(300.0, 0.0),
            vec2(NODE_WIDTH, 100.0),
        )];
        let far_from_ports = pos2(420.0, 60.0);
        assert_eq!(
            snap(
                &graph,
                &from,
                far_from_ports,
                5.0,
                &targets,
                &anchors,
                &entities,
                &rects
            ),
            Some(PortId(21)),
            "FR goes to FR"
        );
        let near_fl = pos2(305.0, 200.0);
        assert_eq!(
            snap(
                &graph, &from, near_fl, 10.0, &targets, &anchors, &entities, &rects
            ),
            Some(PortId(20)),
            "a port within the radius wins"
        );
        assert_eq!(
            snap(
                &graph,
                &from,
                pos2(900.0, 900.0),
                10.0,
                &targets,
                &anchors,
                &entities,
                &rects
            ),
            None
        );
    }
}
