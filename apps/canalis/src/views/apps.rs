//! Apps: one row per application stream with where it goes, after the mockups. A
//! "Default" target follows the star in Devices; a named one pins the stream through
//! WirePlumber's `target.object` and is remembered by app name. The app's label and
//! colour are the user's to set, by app name, and an app can be hidden.

use canalis_pw::{Command, Graph, MediaClass, Node, VirtualKind};
use egui::{ComboBox, RichText, Sense, Stroke, StrokeKind, Ui, vec2};

use super::{
    Glyph, PALETTE, autofocus, avatar, content, db_field, db_label, parse_hex, row_frame,
    section_header, volume_bar,
};
use crate::i18n::{t, tf};
use crate::theme;

const PICKER_WIDTH: f32 = 220.0;
const VOLUME_WIDTH: f32 = 160.0;
const DB_WIDTH: f32 = 56.0;
/// Avatar, picker, volume and dB, plus the gaps between the five columns.
const ROW_FIXED: f32 = AVATAR + PICKER_WIDTH + VOLUME_WIDTH + DB_WIDTH + 8.0 + GAP * 5.0;
const AVATAR: f32 = 28.0;
const GAP: f32 = 12.0;
/// `row_frame`'s inner margin and edge, on each side.
const ROW_INSET: f32 = 13.0;

/// The width of the App and Stream columns in a row whose content is `inner` wide.
fn flexible_width(inner: f32) -> f32 {
    ((inner - ROW_FIXED) / 2.0).max(60.0)
}

/// Column captions laid out like the rows under them.
fn captions(ui: &mut Ui, target_caption: &str) {
    let caption = |ui: &mut Ui, text: &str, width: f32| {
        ui.allocate_ui_with_layout(
            vec2(width, 14.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(width);
                ui.label(
                    RichText::new(text.to_uppercase())
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
            },
        );
    };
    ui.horizontal(|ui| {
        let flexible = flexible_width(ui.available_width() - 2.0 * ROW_INSET);
        ui.add_space(ROW_INSET);
        ui.spacing_mut().item_spacing.x = GAP;
        caption(ui, "", AVATAR);
        caption(ui, t("App"), flexible);
        caption(ui, t("Stream"), flexible);
        caption(ui, target_caption, PICKER_WIDTH);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(ROW_INSET);
            caption(ui, "", DB_WIDTH);
            caption(ui, t("Volume"), VOLUME_WIDTH);
        });
    });
}

#[derive(Default)]
pub struct AppsView {
    /// Hidden apps are listed, dimmed, so they can be shown again.
    show_hidden: bool,
    /// An app whose label is being typed: its name and the text so far.
    renaming: Option<(String, String)>,
}

impl AppsView {
    pub fn show(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        content(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            self.section(
                ui,
                Glyph::Speaker,
                t("Playing"),
                t("each stream and where it goes"),
                t(
                    "Every app stream that is playing and the output it goes to. \"Default\" \
                    follows the star in Devices; picking an output pins the stream there through \
                    WirePlumber and is remembered by app name, so the app lands there again next \
                    time it starts. Right-click an app to rename, colour or hide it.",
                ),
                t("Send to"),
                graph,
                MediaClass::PlaybackStream,
                VirtualKind::Sink,
                commands,
            );
            ui.add_space(16.0);
            self.section(
                ui,
                Glyph::Mic,
                t("Recording"),
                t("what each app hears"),
                t(
                    "Every app stream that is recording and the input it listens to. \"Default\" \
                    follows the star in Devices; picking an input pins the stream there through \
                    WirePlumber and is remembered by app name. Right-click an app to rename, \
                    colour or hide it.",
                ),
                t("Listen to"),
                graph,
                MediaClass::CaptureStream,
                VirtualKind::Source,
                commands,
            );
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn section(
        &mut self,
        ui: &mut Ui,
        glyph: Glyph,
        title: &str,
        subtitle: &str,
        help: &str,
        target_caption: &str,
        graph: &Graph,
        class: MediaClass,
        side: VirtualKind,
        commands: &mut Vec<Command>,
    ) {
        // Chain ends are streams too, but they are Canalis' own, not an app's.
        let is_app = |n: &&Node| n.class == class && !n.is_virtual_device();
        let hidden_count = graph
            .nodes
            .iter()
            .filter(is_app)
            .filter(|n| graph.is_hidden(n.hide_key()))
            .count();
        section_header(ui, Some(glyph), title, subtitle, Some(help), |ui| {
            if hidden_count > 0 {
                ui.checkbox(
                    &mut self.show_hidden,
                    RichText::new(tf("show hidden ({n})", &[("n", &hidden_count)])).size(12.0),
                );
            }
        });
        let mut streams: Vec<&Node> = graph
            .nodes
            .iter()
            .filter(is_app)
            .filter(|n| self.show_hidden || !graph.is_hidden(n.hide_key()))
            .collect();
        streams.sort_by(|a, b| a.app.cmp(&b.app).then_with(|| a.id.cmp(&b.id)));
        if streams.is_empty() {
            ui.label(theme::small_dim(t("none")));
            return;
        }
        captions(ui, target_caption);
        let mut devices: Vec<&Node> = graph
            .nodes
            .iter()
            .filter(|n| match side {
                VirtualKind::Sink => n.class == MediaClass::Sink,
                VirtualKind::Source => n.class == MediaClass::Source,
            })
            .collect();
        devices.sort_by(|a, b| a.label.cmp(&b.label));
        let default_name = match side {
            VirtualKind::Sink => graph.defaults.sink.as_deref(),
            VirtualKind::Source => graph.defaults.source.as_deref(),
        };
        for stream in streams {
            row(
                ui,
                graph,
                stream,
                &devices,
                default_name,
                side,
                &mut self.renaming,
                commands,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    graph: &Graph,
    stream: &Node,
    devices: &[&Node],
    default_name: Option<&str>,
    side: VirtualKind,
    renaming: &mut Option<(String, String)>,
    commands: &mut Vec<Command>,
) {
    let app = stream.hide_key();
    let style = graph.apps.get(app).cloned().unwrap_or_default();
    let hidden = graph.is_hidden(app);
    let row = row_frame(
        ui,
        false,
        |ui| {
            if hidden {
                ui.set_opacity(0.45);
            }
            avatar(
                ui,
                graph.app_label(app),
                style.color.as_deref().and_then(parse_hex),
            );
            // Measured from the row's left edge, before the avatar took its room.
            let flexible = flexible_width(ui.available_width() + AVATAR + GAP);
            ui.allocate_ui_with_layout(
                egui::vec2(flexible, 34.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_width(flexible);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    app_title(ui, graph, app, &style, hidden, flexible, renaming, commands);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&stream.name)
                                .size(12.0)
                                .color(theme::TEXT_DIM),
                        )
                        .truncate(),
                    );
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(flexible, 34.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    ui.set_width(flexible);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(
                        egui::Label::new(
                            RichText::new(stream.media_name.as_deref().unwrap_or(&stream.label))
                                .color(theme::TEXT_DIM),
                        )
                        .truncate(),
                    );
                    ui.add(
                        egui::Label::new(
                            RichText::new(connection(graph, stream, side))
                                .size(12.0)
                                .color(theme::TEXT_DIM),
                        )
                        .truncate(),
                    );
                },
            );
            // `ComboBox::width` is a minimum; the box caps it so a long name truncates.
            ui.allocate_ui_with_layout(
                vec2(PICKER_WIDTH, 24.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_max_width(PICKER_WIDTH);
                    picker(ui, graph, stream, devices, default_name, commands);
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let Some(volume) = &stream.volume else {
                    ui.label(theme::small_dim(t("volume unknown")));
                    return;
                };
                let cubic = volume.cubic();
                ui.allocate_ui_with_layout(
                    egui::vec2(DB_WIDTH, 20.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // A fixed box, so "-12 dB" and "0 dB" leave the bar where it is.
                        ui.set_min_width(DB_WIDTH);
                        ui.set_max_width(DB_WIDTH);
                        let shown = if volume.mute {
                            t("muted").into()
                        } else {
                            db_label(cubic)
                        };
                        if let Some(new) =
                            db_field(ui, ui.scope_id().with(("db", stream.id)), cubic, shown)
                        {
                            commands.push(Command::SetVolume {
                                node: stream.id,
                                cubic: new,
                            });
                        }
                    },
                );
                if let (_, Some(new)) = volume_bar(ui, VOLUME_WIDTH, cubic, theme::TEXT_DIM) {
                    commands.push(Command::SetVolume {
                        node: stream.id,
                        cubic: new,
                    });
                }
            });
        },
        |_| {},
    );
    row.response
        .context_menu(|ui| app_menu(ui, graph, app, &style, hidden, renaming, commands));
}

/// The app's name, editable in place, with the rename, colour and hide menu.
#[allow(clippy::too_many_arguments)]
fn app_title(
    ui: &mut Ui,
    graph: &Graph,
    app: &str,
    style: &canalis_pw::AppStyle,
    hidden: bool,
    width: f32,
    renaming: &mut Option<(String, String)>,
    commands: &mut Vec<Command>,
) {
    if let Some((name, text)) = renaming.as_mut().filter(|(n, _)| n == app) {
        let edit = ui.add(egui::TextEdit::singleline(text).desired_width(width));
        autofocus(ui, &edit);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if enter || (edit.lost_focus() && !escape) {
            commands.push(Command::SetAppStyle {
                app: name.clone(),
                label: Some(text.clone()),
                color: style.color.clone(),
            });
            *renaming = None;
        } else if escape {
            *renaming = None;
        }
        return;
    }
    let label = ui
        .add(
            egui::Label::new(RichText::new(graph.app_label(app)).size(14.0))
                .truncate()
                .sense(Sense::click()),
        )
        .on_hover_text(t("double-click to rename; right-click for more"));
    if label.double_clicked() {
        *renaming = Some((app.to_owned(), graph.app_label(app).to_owned()));
    }
    label.context_menu(|ui| app_menu(ui, graph, app, style, hidden, renaming, commands));
}

/// "→ Scarlett 2i2 USB" or "← Canalis Mic": what the stream is linked to right now.
fn connection(graph: &Graph, stream: &Node, side: VirtualKind) -> String {
    let connected = match side {
        VirtualKind::Sink => graph.downstream_of(stream.id),
        VirtualKind::Source => graph.upstream_of(stream.id),
    };
    let names: Vec<&str> = connected
        .iter()
        .filter_map(|id| graph.node(*id))
        .map(|n| n.short_label.as_str())
        .collect();
    let arrow = match side {
        VirtualKind::Sink => "→",
        VirtualKind::Source => "←",
    };
    if names.is_empty() {
        tf("{arrow} not connected", &[("arrow", &arrow)])
    } else {
        format!("{arrow} {}", names.join(", "))
    }
}

fn picker(
    ui: &mut Ui,
    graph: &Graph,
    stream: &Node,
    devices: &[&Node],
    default_name: Option<&str>,
    commands: &mut Vec<Command>,
) {
    let default_label = default_name
        .and_then(|n| graph.node_by_name(n))
        .map_or(t("Default").to_owned(), |n| {
            tf("Default ({name})", &[("name", &n.short_label)])
        });
    let selected = stream
        .target
        .as_deref()
        .and_then(|t| graph.node_by_name(t))
        .map_or(default_label.clone(), |n| n.short_label.clone());
    ComboBox::from_id_salt(("target", stream.id))
        .selected_text(selected)
        .width(PICKER_WIDTH)
        .truncate()
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(stream.target.is_none(), default_label)
                .clicked()
                && stream.target.is_some()
            {
                commands.push(Command::SetTarget {
                    stream: stream.id,
                    target: None,
                });
            }
            for device in devices {
                let current = stream.target.as_deref() == Some(device.name.as_str());
                if ui.selectable_label(current, &device.label).clicked() && !current {
                    commands.push(Command::SetTarget {
                        stream: stream.id,
                        target: Some(device.name.clone()),
                    });
                }
            }
        });
}

/// Rename, colour, hide: the row's right-click menu.
fn app_menu(
    ui: &mut Ui,
    graph: &Graph,
    app: &str,
    style: &canalis_pw::AppStyle,
    hidden: bool,
    renaming: &mut Option<(String, String)>,
    commands: &mut Vec<Command>,
) {
    if ui.button(t("Rename…")).clicked() {
        *renaming = Some((app.to_owned(), graph.app_label(app).to_owned()));
        ui.close();
    }
    if style.label.is_some() && ui.button(t("Use the app's name")).clicked() {
        commands.push(Command::SetAppStyle {
            app: app.to_owned(),
            label: None,
            color: style.color.clone(),
        });
        ui.close();
    }
    ui.label(theme::small_dim(t("Colour")));
    ui.horizontal(|ui| {
        let current = style.color.as_deref().and_then(parse_hex);
        for hex in PALETTE {
            let color = parse_hex(hex).unwrap_or(theme::ACCENT);
            let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
            ui.painter().rect_filled(rect, 3.0, color);
            if current == Some(color) {
                ui.painter().rect_stroke(
                    rect,
                    3.0,
                    Stroke::new(2.0, theme::TEXT),
                    StrokeKind::Outside,
                );
            }
            if response.clicked() {
                commands.push(Command::SetAppStyle {
                    app: app.to_owned(),
                    label: style.label.clone(),
                    color: Some(hex.to_owned()),
                });
                ui.close();
            }
        }
    });
    if style.color.is_some() && ui.button(t("Hashed colour")).clicked() {
        commands.push(Command::SetAppStyle {
            app: app.to_owned(),
            label: style.label.clone(),
            color: None,
        });
        ui.close();
    }
    ui.separator();
    if ui
        .button(if hidden { t("Show") } else { t("Hide") })
        .clicked()
    {
        commands.push(Command::SetHidden {
            key: app.to_owned(),
            hidden: !hidden,
        });
        ui.close();
    }
}
