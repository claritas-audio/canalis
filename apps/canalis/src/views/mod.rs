//! The four views over one `Graph` snapshot, plus the small painters they share.

mod apps;
mod chains;
mod devices;
mod feedback;
mod graph;
mod groups;
mod pads;
mod presets;
mod settings;
pub mod share;

use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Button, Color32, CornerRadius, FontId, Frame, Galley, Margin, Painter, Pos2, Rect, RichText,
    Sense, Shape, Stroke, Ui, vec2,
};

use canalis_pw::{Graph, MediaClass};

use crate::i18n::{t, tf};
use crate::theme;

pub use apps::AppsView;
pub use chains::ChainsView;
pub use devices::DevicesView;
pub use graph::{GraphView, Opens};
pub use pads::{PadsView, is_sound};
pub use settings::{Section, SettingsView};

const CHIP_FONT: f32 = 9.5;
const CHIP_PADDING: egui::Vec2 = vec2(6.0, 2.0);
/// Content of the list views is centred and no wider than this, as in the mockups.
pub(crate) const CONTENT_MAX_WIDTH: f32 = 1232.0;
pub(crate) const CONTENT_PADDING: f32 = 24.0;
const BAR_HEIGHT: f32 = 6.0;
const KNOB_RADIUS: f32 = 7.0;
/// A dB label dragged sideways moves this much per pixel.
const DB_PER_PX: f32 = 0.25;
/// Dragged this low, a level is silence.
const DB_FLOOR: f32 = -60.0;

/// The yellow of the default star, from the mockups.
pub(crate) const STAR: Color32 = Color32::from_rgb(0xf2, 0xc9, 0x4c);
pub(crate) const VIRTUAL_FG: Color32 = Color32::from_rgb(0xb4, 0x8c, 0xf0);
pub(crate) const VIRTUAL_BG: Color32 = Color32::from_rgb(0x2a, 0x24, 0x40);

/// Quietest level a meter shows, in dBFS.
pub(crate) const METER_FLOOR_DB: f32 = -60.0;
/// How fast a meter falls back after a peak, in dB per second; it rises at once.
const METER_FALL_DB_PER_S: f32 = 24.0;

/// Meter ballistics: a peak shows at once and then falls at `METER_FALL_DB_PER_S`,
/// so the bars move every frame instead of stepping at the report rate.
pub(crate) fn smoothed(
    ui: &Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    peaks: [f32; 2],
) -> [f32; 2] {
    let id = ui.make_persistent_id(("meter", id));
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let fall = METER_FALL_DB_PER_S * dt;
    let shown: [f32; 2] = ui.data(|d| d.get_temp(id)).unwrap_or([METER_FLOOR_DB; 2]);
    let next = std::array::from_fn(|c| {
        let db = 20.0 * peaks[c].max(1e-6).log10();
        db.max(shown[c] - fall).max(METER_FLOOR_DB)
    });
    ui.data_mut(|d| d.insert_temp(id, next));
    next.map(|db| 10f32.powf(db / 20.0))
}

/// Width of one of `count` equal columns with `gap` between them, in the current width.
pub(crate) fn column_width(ui: &Ui, count: usize, gap: f32) -> f32 {
    (ui.available_width() - gap * (count as f32 - 1.0)) / count as f32
}

/// Lays `add` out in a centred column of at most `CONTENT_MAX_WIDTH`, with the page padding.
pub(crate) fn content<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let width = (ui.available_width() - 2.0 * CONTENT_PADDING).min(CONTENT_MAX_WIDTH);
    let left = (ui.available_width() - width) / 2.0;
    ui.add_space(CONTENT_PADDING);
    ui.horizontal_top(|ui| {
        ui.add_space(left);
        ui.vertical(|ui| {
            ui.set_width(width);
            add(ui)
        })
        .inner
    })
    .inner
}

/// "[glyph] Title  subtitle (i) … [right side]" above a list; the subtitle sits on the
/// title's baseline and the "i" explains the list on hover.
pub(crate) fn section_header(
    ui: &mut Ui,
    glyph: Option<Glyph>,
    title: &str,
    subtitle: &str,
    help: Option<&str>,
    right: impl FnOnce(&mut Ui),
) {
    // A known height centres the text on the 30 px buttons at the right.
    let row = vec2(ui.available_width(), 30.0);
    ui.allocate_ui_with_layout(
        row,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if let Some(glyph) = glyph {
                device_glyph(ui, glyph, theme::TEXT_DIM);
            }
            // One text job: egui puts mixed sizes in a row on a common baseline.
            let mut job = LayoutJob::default();
            let style = ui.style().clone();
            for part in [
                RichText::new(title).size(15.0).strong(),
                RichText::new(format!("   {subtitle}"))
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            ] {
                part.append_to(
                    &mut job,
                    &style,
                    egui::FontSelection::Default,
                    egui::Align::BOTTOM,
                );
            }
            ui.label(job);
            if let Some(help) = help {
                help_icon(ui, help);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
        },
    );
}

/// A circled "i" that shows `text` on hover.
pub(crate) fn help_icon(ui: &mut Ui, text: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    let color = if response.hovered() {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    };
    let painter = ui.painter();
    painter.circle_stroke(rect.center(), 7.0, Stroke::new(1.2, color));
    painter.text(
        rect.center() + vec2(0.0, 0.5),
        egui::Align2::CENTER_CENTER,
        "i",
        FontId::proportional(11.5),
        color,
    );
    response.on_hover_ui(|ui| {
        ui.set_max_width(320.0);
        ui.label(RichText::new(text).size(13.0));
    })
}

/// The default marker: a filled star, or a hollow one to click.
pub(crate) fn star_button(
    ui: &mut Ui,
    filled: bool,
    id: impl std::hash::Hash + std::fmt::Debug,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    let response = ui
        .interact(rect.expand(4.0), ui.scope_id().with(id), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let c = rect.center();
    let point = |i: usize| {
        let r = if i.is_multiple_of(2) { 8.0 } else { 3.4 };
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        c + r * vec2(a.cos(), a.sin())
    };
    let outline: Vec<Pos2> = (0..10).map(point).collect();
    let painter = ui.painter();
    if filled {
        // A star is not convex; five convex kites (tip, inner, centre, inner) fill it.
        for tip in (0..10).step_by(2) {
            painter.add(Shape::convex_polygon(
                vec![point(tip), point((tip + 1) % 10), c, point((tip + 9) % 10)],
                STAR,
                Stroke::NONE,
            ));
        }
        painter.add(Shape::closed_line(outline, Stroke::new(1.0, STAR)));
    } else {
        let color = if response.hovered() {
            STAR
        } else {
            theme::TEXT_DIM
        };
        painter.add(Shape::closed_line(outline, Stroke::new(1.3, color)));
    }
    response
}

pub(crate) const DELETE_SIZE: f32 = 24.0;

/// A small square × that turns red on hover.
pub(crate) fn delete_button(ui: &mut Ui) -> egui::Response {
    delete_square(ui, DELETE_SIZE)
}

/// The delete square at `side`, to sit level with the buttons next to it.
pub(crate) fn delete_square(ui: &mut Ui, side: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let (fill, edge, fg) = if response.hovered() {
        (theme::RED_FILL, theme::RED, theme::RED_SOFT)
    } else {
        (theme::PANEL, theme::BORDER, theme::TEXT_DIM)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        theme::CORNER_RADIUS,
        fill,
        Stroke::new(1.0, edge),
        egui::StrokeKind::Inside,
    );
    let c = rect.center();
    let s = Stroke::new(1.5, fg);
    let arm = side / 6.0;
    painter.line_segment([c + vec2(-arm, -arm), c + vec2(arm, arm)], s);
    painter.line_segment([c + vec2(-arm, arm), c + vec2(arm, -arm)], s);
    response
}

/// A list row: panel fill, 1 px edge (`accent` when highlighted, brighter on hover),
/// 10 by 12 padding. The response senses clicks under the row's own widgets, so a
/// right-click anywhere on the row can open its menu.
pub(crate) fn row_frame<R>(
    ui: &mut Ui,
    highlighted: bool,
    add: impl FnOnce(&mut Ui) -> R,
    below: impl FnOnce(&mut Ui),
) -> egui::InnerResponse<R> {
    ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        let edge = if highlighted {
            theme::ACCENT
        } else if ui.response().contains_pointer() {
            theme::BORDER
        } else {
            theme::BORDER_SOFT
        };
        Frame::new()
            .fill(theme::PANEL)
            .stroke(Stroke::new(1.0, edge))
            .corner_radius(theme::CORNER_RADIUS)
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let inner = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        add(ui)
                    })
                    .inner;
                below(ui);
                inner
            })
            .inner
    })
}

/// A bordered button in the mockups' style.
pub(crate) fn outline_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        Button::new(RichText::new(text).size(13.0))
            .fill(theme::PANEL)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .min_size(vec2(0.0, 30.0)),
    )
}

/// A small rounded tag with its own colours.
pub(crate) fn tag(ui: &mut Ui, text: &str, bg: Color32, fg: Color32) -> egui::Response {
    Frame::new()
        .fill(bg)
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| ui.label(RichText::new(text).size(11.0).color(fg)))
        .response
}

/// A thin volume bar the user can drag; the new slider value comes back when it changed.
pub(crate) fn volume_bar(
    ui: &mut Ui,
    width: f32,
    cubic: f32,
    color: Color32,
) -> (egui::Response, Option<f32>) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click_and_drag());
    let cursor = if response.dragged() {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Grab
    };
    let response = response.on_hover_cursor(cursor);
    let active = response.hovered() || response.dragged();
    let bar = Rect::from_center_size(rect.center(), vec2(width, BAR_HEIGHT));
    let painter = ui.painter();
    painter.rect_filled(bar, 3.0, theme::BORDER_SOFT);
    let fill = cubic.clamp(0.0, 1.0) * width;
    if fill > 0.0 {
        painter.rect_filled(
            Rect::from_min_size(bar.left_top(), vec2(fill, BAR_HEIGHT)),
            3.0,
            if active { theme::ACCENT } else { color },
        );
    }
    // The knob grows in on hover rather than popping.
    let grow = ui
        .ctx()
        .animate_bool_with_time(response.id.with("knob"), active, 0.12);
    if grow > 0.0 {
        let center = egui::pos2(bar.left() + fill, bar.center().y);
        let radius = KNOB_RADIUS * (0.6 + 0.4 * grow);
        let alpha = |c: Color32| c.gamma_multiply(grow);
        painter.circle_filled(
            center + vec2(0.0, 1.0),
            radius + 1.0,
            alpha(Color32::from_black_alpha(90)),
        );
        painter.circle(
            center,
            radius,
            alpha(theme::TEXT),
            Stroke::new(1.5, alpha(theme::ACCENT)),
        );
        painter.circle_filled(center, radius * 0.35, alpha(theme::ACCENT));
    }
    let changed = if response.dragged() || response.clicked() {
        response
            .interact_pointer_pos()
            .map(|p| ((p.x - bar.left()) / width).clamp(0.0, 1.0))
    } else {
        None
    };
    (response, changed)
}

/// "0 dB", "-6 dB" or "-∞" for a slider value, the cube of which is the linear gain.
pub(crate) fn db_label(cubic: f32) -> String {
    let linear = cubic.powi(3);
    if linear < 1e-4 {
        return "-∞".to_owned();
    }
    let db = 20.0 * linear.log10();
    if db.abs() < 0.05 {
        "0 dB".to_owned()
    } else {
        format!("{db:+.0} dB").replace('+', "+")
    }
}

/// The slider value for a typed level: "-12", "-12 dB", "-inf"; above 0 dB is capped,
/// since the bar ends there. `None` when it is not a number.
pub(crate) fn parse_db(text: &str) -> Option<f32> {
    let text = text.trim().trim_end_matches("dB").trim().replace('−', "-");
    if matches!(text.as_str(), "-inf" | "-∞" | "inf" | "∞") {
        return Some(0.0);
    }
    let db = text.parse::<f32>().ok()?.min(0.0);
    Some(10f32.powf(db / 20.0).cbrt())
}

/// The dB readout of a volume; a click turns it into a field to type a level in.
/// Returns the new slider value once one is entered.
pub(crate) fn db_field(ui: &mut Ui, id: egui::Id, cubic: f32, shown: String) -> Option<f32> {
    let font = FontId::proportional(12.0);
    let Some(mut text) = ui.data(|d| d.get_temp::<String>(id)) else {
        let label = ui
            .add(
                egui::Label::new(RichText::new(shown).font(font).color(theme::TEXT_DIM))
                    .sense(Sense::click_and_drag()),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
            .on_hover_text(t("drag sideways to change, click to type a level in dB"));
        if label.clicked() {
            let current = db_label(cubic).trim_end_matches(" dB").to_owned();
            ui.data_mut(|d| d.insert_temp(id, current));
        }
        // The drag adds up from where it started, so a slow round trip loses no steps.
        let drag_id = id.with("drag");
        if label.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, (cubic_db(cubic), 0.0f32)));
        }
        if label.dragged()
            && let Some((start, moved)) = ui.data(|d| d.get_temp::<(f32, f32)>(drag_id))
        {
            let moved = moved + label.drag_delta().x;
            ui.data_mut(|d| d.insert_temp(drag_id, (start, moved)));
            let db = (start + moved * DB_PER_PX).clamp(DB_FLOOR, 0.0);
            return Some(if db <= DB_FLOOR {
                0.0
            } else {
                10f32.powf(db / 60.0)
            });
        }
        return None;
    };
    let edit = ui.add(
        egui::TextEdit::singleline(&mut text)
            .font(font)
            .desired_width(ui.available_width())
            .align(egui::Align2::RIGHT_CENTER),
    );
    if autofocus(ui, &edit) {
        // Selected on open, so typing replaces the old level straight away.
        let mut state = egui::text_edit::TextEditState::load(ui.ctx(), edit.id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::select_all_str(&text)));
        state.store(ui.ctx(), edit.id);
    }
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if edit.lost_focus() || escape {
        ui.data_mut(|d| d.remove_temp::<String>(id));
        return if escape { None } else { parse_db(&text) };
    }
    ui.data_mut(|d| d.insert_temp(id, text));
    None
}

/// The level a slider value stands for, in dB, floored at `DB_FLOOR`.
fn cubic_db(cubic: f32) -> f32 {
    if cubic <= 0.0 {
        DB_FLOOR
    } else {
        (60.0 * cubic.log10()).max(DB_FLOOR)
    }
}

/// The app streams on a device: playing into it, or recording from it.
pub(crate) fn app_streams<'a>(graph: &'a Graph, device: &str) -> Vec<&'a canalis_pw::Node> {
    let Some(device) = graph.node_by_name(device) else {
        return Vec::new();
    };
    graph
        .nodes
        .iter()
        .filter(|n| n.app.is_some() && !n.is_virtual_device() && !graph.is_hidden(n.hide_key()))
        .filter(|n| match n.class {
            MediaClass::PlaybackStream => graph.downstream_of(n.id).contains(&device.id),
            MediaClass::CaptureStream => graph.upstream_of(n.id).contains(&device.id),
            _ => false,
        })
        .collect()
}

/// One chip per app stream, up to `max`, then "+N more": the app's colour, its name
/// and its level, dragged or typed like the rows'.
pub(crate) fn app_chips(
    ui: &mut Ui,
    graph: &Graph,
    streams: &[&canalis_pw::Node],
    max: usize,
    commands: &mut Vec<canalis_pw::Command>,
) {
    // Two fixed columns: egui does not wrap framed chips, and a row that grows with
    // every app would widen the strip.
    let gap = 4.0;
    let width = ((ui.available_width() - gap) / 2.0).floor();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        for pair in streams[..streams.len().min(max)].chunks(2) {
            ui.horizontal_top(|ui| {
                for stream in pair {
                    app_chip(ui, graph, stream, width, commands);
                }
            });
        }
        if streams.len() > max {
            let rest: Vec<&str> = streams[max..]
                .iter()
                .map(|s| graph.app_label(s.hide_key()))
                .collect();
            ui.label(theme::small_dim(tf("+{n} more", &[("n", &rest.len())])))
                .on_hover_text(rest.join(", "));
        }
    });
}

/// Room for a chip's level: "-12 dB", "muted".
const CHIP_LEVEL: f32 = 36.0;

/// One app's chip, `width` wide: colour dot, name (truncated to fit), level.
fn app_chip(
    ui: &mut Ui,
    graph: &Graph,
    stream: &canalis_pw::Node,
    width: f32,
    commands: &mut Vec<canalis_pw::Command>,
) {
    let app = stream.hide_key();
    let style = graph.apps.get(app);
    let color = app_color(
        graph.app_label(app),
        style.and_then(|s| s.color.as_deref()).and_then(parse_hex),
    );
    // Margins, dot, two gaps and the level field around the name.
    let name_width = (width - 12.0 - 8.0 - 10.0 - CHIP_LEVEL).max(20.0);
    ui.allocate_ui(vec2(width, 20.0), |ui| {
        ui.set_width(width);
        Frame::new()
            .fill(theme::BORDER_SOFT)
            .corner_radius(theme::CORNER_RADIUS)
            .inner_margin(Margin {
                left: 6,
                right: 6,
                top: 2,
                bottom: 2,
            })
            .show(ui, |ui| {
                ui.set_width(width - 12.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, color);
                    ui.scope(|ui| {
                        ui.set_max_width(name_width);
                        ui.add(
                            egui::Label::new(RichText::new(graph.app_label(app)).size(11.0))
                                .truncate(),
                        )
                        .on_hover_text(graph.app_label(app));
                    });
                    let Some(volume) = &stream.volume else {
                        return;
                    };
                    let cubic = volume.cubic();
                    let shown = if volume.mute {
                        t("muted").to_owned()
                    } else {
                        db_label(cubic)
                    };
                    // Pinned to the chip's right edge, whatever the name's length.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.set_max_width(CHIP_LEVEL);
                        if let Some(new) =
                            db_field(ui, ui.scope_id().with(("chip-db", stream.id)), cubic, shown)
                        {
                            commands.push(canalis_pw::Command::SetVolume {
                                node: stream.id,
                                cubic: new,
                            });
                        }
                    });
                });
            });
    });
}

/// Focuses a text field on the frame it appears and leaves it alone after, so a click
/// elsewhere can take the focus away (and an inline edit can commit on it). True on
/// that first frame.
pub(crate) fn autofocus(ui: &Ui, edit: &egui::Response) -> bool {
    let key = edit.id.with("autofocus");
    let frame = ui.ctx().cumulative_frame_nr();
    let last = ui.data_mut(|d| {
        let last = d.get_temp::<u64>(key);
        d.insert_temp(key, frame);
        last
    });
    // Not drawn last frame: it just appeared (or came back), so it takes the focus.
    let first = last.is_none_or(|l| l + 1 < frame);
    if first {
        edit.request_focus();
    }
    first
}

/// A 28 px disc with the first letter, coloured by the name.
/// The icons a chain can carry, from egui's emoji font.
pub(crate) const ICONS: [&str; 12] = [
    "🔊", "🎧", "🎤", "🎙", "🎵", "🎮", "💬", "📞", "🎬", "📺", "🎚", "🎛",
];

/// A coloured circle with the name's initial; `color` overrides the one hashed from it.
pub(crate) fn avatar(ui: &mut Ui, name: &str, color: Option<Color32>) {
    let color = app_color(name, color);
    let (rect, _) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 14.0, color);
    let initial = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or("?".to_owned(), |c| c.to_uppercase().to_string());
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        FontId::proportional(12.0),
        theme::BACKGROUND,
    );
}

/// An app's colour: the user's, or one hashed from its name.
pub(crate) fn app_color(name: &str, color: Option<Color32>) -> Color32 {
    const PALETTE: [Color32; 6] = [
        Color32::from_rgb(0xe8, 0x71, 0x6b),
        Color32::from_rgb(0x9a, 0xa8, 0xf5),
        Color32::from_rgb(0x6c, 0xc6, 0x8a),
        Color32::from_rgb(0xf0, 0x9a, 0x4a),
        Color32::from_rgb(0x4f, 0xc3, 0xf7),
        Color32::from_rgb(0xf2, 0xc9, 0x4c),
    ];
    let hash = name
        .bytes()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    color.unwrap_or(PALETTE[hash % PALETTE.len()])
}

/// The colour a chain kind is drawn with: its strip edge, its graph node, its links.
pub(crate) fn kind_color(kind: canalis_pw::ChainKind) -> Color32 {
    match kind {
        canalis_pw::ChainKind::Output => Color32::from_rgb(0x56, 0xb4, 0xe9),
        canalis_pw::ChainKind::Mic => Color32::from_rgb(0x6c, 0xc6, 0x8a),
        canalis_pw::ChainKind::Insert => Color32::from_rgb(0xf0, 0x9a, 0x4a),
        canalis_pw::ChainKind::Cable => Color32::from_rgb(0x3f, 0xd0, 0xc8),
    }
}

/// The user's colour for a chain when set and valid, else its kind's.
pub(crate) fn chain_color(chain: &canalis_pw::ChainState) -> Color32 {
    chain
        .color
        .as_deref()
        .and_then(parse_hex)
        .unwrap_or_else(|| kind_color(chain.kind))
}

/// `#rrggbb` to a colour.
pub(crate) fn parse_hex(text: &str) -> Option<Color32> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color32::from_rgb(channel(0)?, channel(2)?, channel(4)?))
}

/// The swatches a chain can take, besides its kind's colour.
pub(crate) const PALETTE: [&str; 8] = [
    "#56b4e9", "#6cc68a", "#f09a4a", "#3fd0c8", "#b48cf0", "#e8716b", "#f2c94c", "#9aa8f5",
];

/// Background and text of a chain kind's tag.
pub(crate) fn kind_tag_colors(kind: canalis_pw::ChainKind) -> (Color32, Color32) {
    match kind {
        canalis_pw::ChainKind::Output => (theme::SELECTION, theme::ACCENT),
        canalis_pw::ChainKind::Mic => (Color32::from_rgb(0x1f, 0x3a, 0x2a), theme::GREEN),
        canalis_pw::ChainKind::Insert => (
            Color32::from_rgb(0x2b, 0x24, 0x10),
            Color32::from_rgb(0xf0, 0xc0, 0x60),
        ),
        canalis_pw::ChainKind::Cable => (
            Color32::from_rgb(0x16, 0x38, 0x38),
            Color32::from_rgb(0x6f, 0xe0, 0xd8),
        ),
    }
}

/// A loudspeaker, microphone, screen or plus sign, 18 px, in `color`.
pub(crate) fn device_glyph(ui: &mut Ui, glyph: Glyph, color: Color32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    let p = ui.painter();
    let s = Stroke::new(1.6, color);
    let c = rect.center();
    match glyph {
        Glyph::Speaker => {
            p.rect_stroke(
                Rect::from_center_size(c - vec2(4.5, 0.0), vec2(4.0, 6.0)),
                0.5,
                s,
                egui::StrokeKind::Middle,
            );
            p.line_segment([c + vec2(-2.5, -3.0), c + vec2(1.5, -6.5)], s);
            p.line_segment([c + vec2(1.5, -6.5), c + vec2(1.5, 6.5)], s);
            p.line_segment([c + vec2(1.5, 6.5), c + vec2(-2.5, 3.0)], s);
            p.add(Shape::line(
                (0..8)
                    .map(|i| {
                        let a = -0.9 + 1.8 * i as f32 / 7.0;
                        c + vec2(4.0 + 3.0 * a.cos(), 4.0 * a.sin())
                    })
                    .collect(),
                s,
            ));
        }
        Glyph::Mic => {
            p.rect_stroke(
                Rect::from_center_size(c - vec2(0.0, 2.5), vec2(5.0, 9.0)),
                2.5,
                s,
                egui::StrokeKind::Middle,
            );
            p.add(Shape::line(
                (0..9)
                    .map(|i| {
                        let a = std::f32::consts::PI * i as f32 / 8.0;
                        c + vec2(5.0 * a.cos(), 1.0 + 5.0 * a.sin())
                    })
                    .collect(),
                s,
            ));
            p.line_segment([c + vec2(0.0, 6.0), c + vec2(0.0, 8.5)], s);
        }
    }
    rect
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Glyph {
    Speaker,
    Mic,
}

/// One line of text cut with an ellipsis at `max_width`.
fn truncated(ui: &Ui, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(max_width);
    ui.painter().layout_job(job)
}

/// A small rounded tag anchored by its right-centre; returns the rect it covered.
fn chip(painter: &Painter, right_center: Pos2, text: &str, color: Color32) -> Rect {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(CHIP_FONT), color);
    let size = galley.size() + 2.0 * CHIP_PADDING;
    let rect = Rect::from_min_size(right_center - vec2(size.x, size.y / 2.0), size);
    painter.rect_filled(rect, CornerRadius::same(3), color.gamma_multiply(0.18));
    painter.galley(rect.min + CHIP_PADDING, galley, color);
    rect
}

fn text_center_left(painter: &Painter, pos: Pos2, galley: Arc<Galley>, color: Color32) {
    let size = galley.size();
    painter.galley(pos - vec2(0.0, size.y / 2.0), galley, color);
}

#[cfg(test)]
mod tests {
    use super::{db_label, parse_db};

    #[test]
    fn parse_db_round_trips_and_caps() {
        assert_eq!(db_label(parse_db("-12").unwrap()), "-12 dB");
        assert_eq!(db_label(parse_db(" -6 dB ").unwrap()), "-6 dB");
        assert_eq!(db_label(parse_db("−20").unwrap()), "-20 dB");
        assert_eq!(parse_db("3"), Some(1.0));
        assert_eq!(parse_db("-inf"), Some(0.0));
        assert_eq!(parse_db("loud"), None);
    }
}
