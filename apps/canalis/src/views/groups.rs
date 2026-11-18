//! Graph groups and the minimap. A group is a named, coloured frame around boxes (by
//! their position keys) that moves as one, or folds into a single box that keeps only
//! the ports linked outside it. The minimap shows every box and the visible area.

use std::collections::{BTreeSet, HashSet};

use canalis_pw::{Graph, PortId};
use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::{PALETTE, parse_hex};
use crate::i18n::tf;
use crate::theme;

/// Position key of a folded group's box.
pub(super) const GROUP_KEY: &str = "group:";
/// Room around the member boxes, and the title bar above them.
pub(super) const FRAME_PAD: f32 = 12.0;
pub(super) const TITLE_BAR: f32 = 22.0;
const MINIMAP_WIDTH: f32 = 180.0;
const MINIMAP_MARGIN: f32 = 12.0;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Group {
    /// Stable across renames; the folded box's position key is `group:<id>`.
    pub id: u32,
    pub name: String,
    pub color: String,
    /// Position keys of the boxes inside.
    pub members: BTreeSet<String>,
    pub collapsed: bool,
}

impl Group {
    pub fn key(&self) -> String {
        format!("{GROUP_KEY}{}", self.id)
    }

    pub fn color32(&self) -> Color32 {
        parse_hex(&self.color).unwrap_or(theme::ACCENT)
    }
}

/// A new group of `members`, taken out of any group they were in; groups left with
/// fewer than two boxes dissolve.
pub(super) fn group(groups: &mut Vec<Group>, members: BTreeSet<String>) -> u32 {
    // Before any group dissolves, so an id (and its saved position) is never reused here.
    let id = groups.iter().map(|g| g.id + 1).max().unwrap_or(1);
    let mut members: BTreeSet<String> = members
        .into_iter()
        .flat_map(|key| {
            // A folded group in the selection brings its boxes along.
            match groups.iter().find(|g| g.key() == key) {
                Some(g) => g.members.iter().cloned().collect::<Vec<_>>(),
                None => vec![key],
            }
        })
        .collect();
    members.retain(|key| !key.starts_with(GROUP_KEY));
    for g in groups.iter_mut() {
        g.members.retain(|key| !members.contains(key));
    }
    groups.retain(|g| g.members.len() >= 2);
    let name = |n: u32| tf("Group {n}", &[("n", &n)]);
    let n = (1..)
        .find(|n| !groups.iter().any(|g| g.name == name(*n)))
        .unwrap_or(1);
    groups.push(Group {
        id,
        name: name(n),
        color: PALETTE[(id as usize) % PALETTE.len()].to_owned(),
        members,
        collapsed: false,
    });
    id
}

/// The ports among `ports` linked to a port outside `inside`.
pub(super) fn outside_ports<'a>(
    graph: &Graph,
    ports: impl Iterator<Item = &'a canalis_pw::Port>,
    inside: &HashSet<PortId>,
) -> Vec<&'a canalis_pw::Port> {
    ports
        .filter(|port| {
            graph.links.iter().any(|l| {
                (l.input == port.id && !inside.contains(&l.output))
                    || (l.output == port.id && !inside.contains(&l.input))
            })
        })
        .collect()
}

/// The frame around an open group's boxes, title bar included.
pub(super) fn frame(members: impl Iterator<Item = Rect>) -> Option<Rect> {
    let bounds = members.reduce(|a, b| a.union(b))?;
    let framed = bounds.expand(FRAME_PAD);
    Some(Rect::from_min_max(
        pos2(framed.min.x, framed.min.y - TITLE_BAR),
        framed.max,
    ))
}

pub(super) fn paint_frame(ui: &Ui, group: &Group, rect: Rect, hovered: bool) {
    let color = group.color32();
    let painter = ui.painter();
    painter.rect_filled(rect, theme::CORNER_RADIUS, color.gamma_multiply(0.07));
    painter.rect_stroke(
        rect,
        theme::CORNER_RADIUS,
        Stroke::new(if hovered { 2.0 } else { 1.2 }, color.gamma_multiply(0.8)),
        StrokeKind::Inside,
    );
    painter.text(
        pos2(rect.min.x + 10.0, rect.min.y + TITLE_BAR / 2.0),
        egui::Align2::LEFT_CENTER,
        &group.name,
        egui::FontId::proportional(12.5),
        color,
    );
}

pub(super) fn title_bar(rect: Rect) -> Rect {
    Rect::from_min_size(rect.min, vec2(rect.width(), TITLE_BAR))
}

/// A small map of `boxes` in the corner of `area` once they do not all fit in `view`;
/// returns how far to move the view: to a clicked point, or by as much as a drag moved
/// the outlined area (the map's scale holds still for the whole drag).
pub(super) fn minimap(
    ui: &Ui,
    area: Rect,
    view: Rect,
    boxes: &[(Rect, Color32)],
) -> Option<egui::Vec2> {
    let bounds = boxes.iter().map(|(r, _)| *r).reduce(|a, b| a.union(b))?;
    let id = ui.scope_id().with("graph-minimap");
    // While dragging, the map keeps the extent it had when the drag began; otherwise it
    // would rescale under the pointer as the view moves.
    let frozen: Option<Rect> = ui.ctx().data(|d| d.get_temp(id));
    if frozen.is_none() && view.contains_rect(bounds) {
        return None;
    }
    let world = frozen.unwrap_or_else(|| bounds.union(view).expand(40.0));
    let height = (MINIMAP_WIDTH * world.height() / world.width()).clamp(60.0, 160.0);
    let mini = Rect::from_min_size(
        pos2(
            area.max.x - MINIMAP_WIDTH - MINIMAP_MARGIN,
            area.max.y - height - MINIMAP_MARGIN,
        ),
        vec2(MINIMAP_WIDTH, height),
    );
    let scale = (mini.width() / world.width()).min(mini.height() / world.height());
    let to_mini = |p: Pos2| mini.min + (p - world.min) * scale;
    let to_world = |p: Pos2| world.min + (p - mini.min) / scale;
    let mut shift = None;
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(mini.min)
        .show(ui.ctx(), |ui| {
            let (rect, response) = ui.allocate_exact_size(mini.size(), Sense::click_and_drag());
            let painter = ui.painter();
            painter.rect_filled(
                rect,
                theme::CORNER_RADIUS,
                theme::PANEL.gamma_multiply(0.92),
            );
            painter.rect_stroke(
                rect,
                theme::CORNER_RADIUS,
                Stroke::new(1.0, theme::BORDER),
                StrokeKind::Inside,
            );
            for (r, color) in boxes {
                let small = Rect::from_min_max(to_mini(r.min), to_mini(r.max));
                painter.rect_filled(small, 1.0, color.gamma_multiply(0.7));
            }
            let seen = Rect::from_min_max(to_mini(view.min), to_mini(view.max)).intersect(rect);
            painter.rect_stroke(
                seen,
                2.0,
                Stroke::new(1.5, theme::ACCENT),
                StrokeKind::Inside,
            );
            if response.drag_started() {
                ui.ctx().data_mut(|d| d.insert_temp(id, world));
            }
            if response.dragged() {
                shift = Some(response.drag_delta() / scale);
            } else if response.clicked()
                && let Some(at) = response.interact_pointer_pos()
            {
                shift = Some(to_world(at) - view.center());
            }
            if response.drag_stopped() {
                ui.ctx().data_mut(|d| d.remove::<Rect>(id));
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            }
        });
    shift
}

/// `[[group]]` tables for the layout file.
pub(super) fn to_toml(groups: &[Group]) -> toml::Value {
    toml::Value::Array(
        groups
            .iter()
            .map(|g| {
                let mut table = toml::Table::new();
                table.insert("id".into(), toml::Value::from(i64::from(g.id)));
                table.insert("name".into(), toml::Value::from(g.name.clone()));
                table.insert("color".into(), toml::Value::from(g.color.clone()));
                table.insert(
                    "members".into(),
                    toml::Value::from(g.members.iter().cloned().collect::<Vec<_>>()),
                );
                table.insert("collapsed".into(), toml::Value::from(g.collapsed));
                toml::Value::Table(table)
            })
            .collect(),
    )
}

pub(super) fn from_toml(value: Option<&toml::Value>) -> Vec<Group> {
    let Some(groups) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    groups
        .iter()
        .filter_map(|g| {
            Some(Group {
                id: u32::try_from(g.get("id")?.as_integer()?).ok()?,
                name: g.get("name")?.as_str()?.to_owned(),
                color: g.get("color")?.as_str()?.to_owned(),
                members: g
                    .get("members")?
                    .as_array()?
                    .iter()
                    .filter_map(|m| m.as_str().map(str::to_owned))
                    .collect(),
                collapsed: g.get("collapsed").and_then(|c| c.as_bool()) == Some(true),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn grouping_takes_boxes_out_of_other_groups_and_dissolves_what_is_left_alone() {
        let mut groups = Vec::new();
        let first = group(&mut groups, keys(&["a", "b", "c"]));
        assert_eq!(groups[0].name, "Group 1");
        let second = group(&mut groups, keys(&["b", "c", "d"]));
        assert_ne!(first, second);
        // "Group 1" kept only "a": a group of one is no group.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, keys(&["b", "c", "d"]));
        assert_eq!(groups[0].name, "Group 1");
    }

    #[test]
    fn a_folded_group_in_the_selection_brings_its_boxes() {
        let mut groups = Vec::new();
        let id = group(&mut groups, keys(&["a", "b"]));
        groups[0].collapsed = true;
        let folded = format!("{GROUP_KEY}{id}");
        group(&mut groups, keys(&[&folded, "c"]));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, keys(&["a", "b", "c"]));
    }

    #[test]
    fn groups_round_trip_through_the_layout_file() {
        let mut groups = Vec::new();
        group(&mut groups, keys(&["alsa_output.x", "canalis.desktop-eq"]));
        groups[0].collapsed = true;
        groups[0].name = "Music".into();
        let mut table = toml::Table::new();
        table.insert("group".into(), to_toml(&groups));
        let text = table.to_string();
        let back: toml::Table = text.parse().unwrap();
        assert_eq!(from_toml(back.get("group")), groups);
    }

    #[test]
    fn a_folded_group_keeps_only_ports_linked_outside_it() {
        let port = |id: u32, node: &'static str, dir: &'static str| {
            canalis_pw::Port::from_props(id, |key| match key {
                "node.id" => Some(node),
                "port.direction" => Some(dir),
                "port.name" => Some("p"),
                _ => None,
            })
            .unwrap()
        };
        let link = |id, output, input| canalis_pw::Link {
            id,
            output: PortId(output),
            input: PortId(input),
            ours: false,
        };
        // Mic (out 1) → chain (in 2, out 3) inside; chain out 3 → speakers (in 4) outside.
        let graph = Graph {
            ports: vec![
                port(1, "10", "out"),
                port(2, "11", "in"),
                port(3, "11", "out"),
                port(4, "12", "in"),
            ],
            links: vec![link(1, 1, 2), link(2, 3, 4)],
            ..Graph::default()
        };
        let inside: HashSet<PortId> = [1, 2, 3].map(PortId).into_iter().collect();
        let kept: Vec<u32> = outside_ports(&graph, graph.ports[..3].iter(), &inside)
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(kept, [3]);
    }

    #[test]
    fn the_frame_wraps_the_boxes_with_room_for_its_title() {
        let rect = frame(
            [
                Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 60.0)),
                Rect::from_min_size(pos2(300.0, 100.0), vec2(200.0, 60.0)),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(rect.min, pos2(-FRAME_PAD, -FRAME_PAD - TITLE_BAR));
        assert_eq!(rect.max, pos2(500.0 + FRAME_PAD, 160.0 + FRAME_PAD));
    }
}
