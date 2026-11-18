//! The Pads tab: the soundboard as a grid of 8×4 pads per bank, played by click, keyboard
//! or a MIDI controller, with where the board plays into on top. The service mixes the
//! pads into its Soundboard source; this view only edits the board and presses pads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use canalis_pw::{
    BANKS, Command, ControlTarget, Graph, MediaClass, PADS_PER_BANK, Pad, PadMode, PadState,
    SOUNDBOARD_NODE, Soundboard,
};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Align2, Button, ComboBox, DragValue, FontId, Key, Rect, RichText, Sense, Slider, Stroke,
    StrokeKind, Ui, pos2, vec2,
};

use super::{PALETTE, autofocus, parse_hex};
use crate::file_dialog;
use crate::i18n::{t, tf};
use crate::theme;

const COLUMNS: u32 = 8;
const ROWS: u32 = 4;
const GAP: f32 = 8.0;
/// The keyboard plays the bank in view, row by row.
const KEYS: [[Key; 8]; 4] = [
    [
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
    ],
    [
        Key::Q,
        Key::W,
        Key::E,
        Key::R,
        Key::T,
        Key::Y,
        Key::U,
        Key::I,
    ],
    [
        Key::A,
        Key::S,
        Key::D,
        Key::F,
        Key::G,
        Key::H,
        Key::J,
        Key::K,
    ],
    [
        Key::Z,
        Key::X,
        Key::C,
        Key::V,
        Key::B,
        Key::N,
        Key::M,
        Key::Comma,
    ],
];
const AUDIO_EXTENSIONS: [&str; 5] = ["wav", "flac", "mp3", "ogg", "oga"];

#[derive(Default)]
pub struct PadsView {
    bank: u32,
    /// The file chooser a slot opened, until it answers.
    chooser: Option<(u32, file_dialog::Dialog)>,
    /// Where the last pad's file came from; the chooser opens there.
    last_folder: Option<PathBuf>,
    /// The pad being renamed and the name typed so far.
    renaming: Option<(String, String)>,
    /// The pad the mouse holds down, released when the button goes up.
    pointer_held: Option<String>,
    /// Where each tile was last drawn, for files dropped on one.
    tiles: Vec<(u32, Rect)>,
    /// When each playing pad started, by this window's clock.
    started: HashMap<String, Instant>,
    /// The snapshot `started` was taken from.
    seen: usize,
}

/// Whether a dropped file is a sound for a pad.
pub fn is_sound(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

impl PadsView {
    pub fn show(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        self.follow_clock(graph);
        if !self.started.is_empty() {
            ui.ctx().request_repaint();
        }
        if self.last_folder.is_none() {
            self.last_folder = graph
                .soundboard
                .pads
                .last()
                .and_then(|p| p.path.parent())
                .map(Path::to_path_buf);
        }
        egui::Frame::new()
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                self.top(ui, graph, commands);
                ui.add_space(12.0);
                self.bank_row(ui, graph, commands);
                ui.add_space(10.0);
                self.grid(ui, graph, commands);
            });
        self.keys(ui, graph, commands);
        self.release_pointer(ui, commands);
        self.take_chosen(ui, &graph.soundboard, commands);
    }

    /// Files dropped on the window while this tab shows: each becomes a pad, the first on
    /// the tile under the pointer when it is empty, the rest on the next empty tiles.
    pub fn drop_files(
        &mut self,
        ctx: &egui::Context,
        paths: &[PathBuf],
        graph: &Graph,
    ) -> Option<Command> {
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let mut board = graph.soundboard.clone();
        let mut under = pointer.and_then(|p| {
            self.tiles
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|(slot, _)| *slot)
        });
        for path in paths {
            let slot = match under.take() {
                Some(slot) if board.at(slot).is_none() => slot,
                _ => next_free(&board, self.bank)?,
            };
            add_pad(&mut board, slot, path);
            self.last_folder = path.parent().map(Path::to_path_buf);
        }
        Some(Command::SetSoundboard { board })
    }

    /// Where the board plays into, its level and Stop all.
    fn top(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        let board = &graph.soundboard;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t("Plays into")).strong());
            for output in &board.outputs {
                let label = graph
                    .node_by_name(output)
                    .map_or(output.as_str(), |n| n.label.as_str());
                let chip = ui
                    .add(Button::new(format!("{label}  ×")).small())
                    .on_hover_text(t("stop playing into this"));
                if chip.clicked() {
                    let mut board = board.clone();
                    board.outputs.retain(|o| o != output);
                    commands.push(Command::SetSoundboard { board });
                }
            }
            let candidates: Vec<_> = graph
                .nodes
                .iter()
                .filter(|n| {
                    n.name != SOUNDBOARD_NODE
                        && !board.outputs.contains(&n.name)
                        && !graph.is_hidden(&n.name)
                        && (n.class == MediaClass::Sink
                            || (n.class == MediaClass::Source && n.is_virtual_device()))
                })
                .collect();
            ComboBox::from_id_salt("pads-add-output")
                .selected_text("+")
                .width(28.0)
                .show_ui(ui, |ui| {
                    for node in candidates {
                        if ui.selectable_label(false, &node.label).clicked() {
                            let mut board = board.clone();
                            board.outputs.push(node.name.clone());
                            commands.push(Command::SetSoundboard { board });
                        }
                    }
                });
            ui.add_space(12.0);
            let mut monitor = board.monitor;
            if ui
                .checkbox(&mut monitor, t("Hear them in my output too"))
                .changed()
            {
                let mut board = board.clone();
                board.monitor = monitor;
                commands.push(Command::SetSoundboard { board });
            }
            ui.add_space(12.0);
            ui.label(t("Level"));
            let id = ui.make_persistent_id("pads-level");
            let mut level: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(board.level_db);
            let slider = ui.add(
                Slider::new(&mut level, -40.0..=6.0)
                    .suffix(" dB")
                    .fixed_decimals(1),
            );
            if slider.dragged() {
                ui.data_mut(|d| d.insert_temp(id, level));
            } else {
                ui.data_mut(|d| d.remove_temp::<f32>(id));
            }
            if (slider.drag_stopped() || (slider.changed() && !slider.dragged()))
                && (level - board.level_db).abs() > 0.01
            {
                let mut board = board.clone();
                board.level_db = level;
                commands.push(Command::SetSoundboard { board });
            }
            ui.add_space(12.0);
            let playing = graph.pads.iter().any(|p| p.playing.is_some());
            if ui
                .add_enabled(playing, Button::new(t("■ Stop all")))
                .on_hover_text(t("Esc does the same"))
                .clicked()
            {
                commands.push(Command::StopPads);
            }
        });
        let effects: Vec<&str> = graph
            .chains
            .iter()
            .filter(|c| c.inputs.iter().any(|i| i == SOUNDBOARD_NODE))
            .map(|c| c.label.as_str())
            .collect();
        let hint = if effects.is_empty() {
            t("For effects on the pads, make a chain and pick Soundboard as its input.").to_owned()
        } else {
            tf("Effects: {chains}", &[("chains", &effects.join(", "))])
        };
        ui.label(theme::small_dim(hint));
    }

    fn bank_row(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        ui.horizontal(|ui| {
            ui.label(t("Bank"));
            for bank in 0..BANKS {
                let name = char::from(b'A' + bank as u8).to_string();
                if ui.selectable_label(self.bank == bank, name).clicked() {
                    self.bank = bank;
                    self.renaming = None;
                }
            }
            ui.add_space(16.0);
            let learning = matches!(graph.learning, Some(ControlTarget::Pad { .. }));
            if learning {
                if ui.button(t("Cancel learning")).clicked() {
                    commands.push(Command::Learn { target: None });
                }
                ui.label(
                    RichText::new(t("press the pads on your controller in order"))
                        .color(theme::ORANGE),
                );
            } else {
                let targets: Vec<ControlTarget> = bank_pads(&graph.soundboard, self.bank)
                    .map(|p| ControlTarget::Pad {
                        name: p.name.clone(),
                    })
                    .collect();
                let learn = ui
                    .add_enabled(!targets.is_empty(), Button::new(t("Learn MIDI for this bank")))
                    .on_hover_text(t(
                        "then press your controller's pads one by one, in the order of the grid, on the MIDI input picked in Settings",
                    ));
                if learn.clicked() {
                    commands.push(Command::LearnMany { targets });
                }
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(theme::small_dim(t(
                    "keys 1–8, Q–I, A–K, Z–, play this bank",
                )));
            });
        });
    }

    fn grid(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        let width =
            ((ui.available_width() - GAP * (COLUMNS - 1) as f32) / COLUMNS as f32).max(64.0);
        let height = ((ui.available_height() - GAP * (ROWS - 1) as f32) / ROWS as f32)
            .clamp(56.0, width.min(120.0));
        self.tiles.clear();
        let dragging_files = ui.input(|i| !i.raw.hovered_files.is_empty());
        let pointer = ui.input(|i| i.pointer.hover_pos());
        ui.spacing_mut().item_spacing = vec2(GAP, GAP);
        for row in 0..ROWS {
            ui.horizontal(|ui| {
                for column in 0..COLUMNS {
                    let index = row * COLUMNS + column;
                    let slot = self.bank * PADS_PER_BANK + index;
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
                    self.tiles.push((slot, rect));
                    let key = key_label(index);
                    let drop_here = dragging_files && pointer.is_some_and(|p| rect.contains(p));
                    match graph.soundboard.at(slot) {
                        Some(pad) => {
                            let state = graph.pads.iter().find(|s| s.name == pad.name);
                            self.pad_tile(ui, graph, pad, state, rect, &response, key, commands);
                        }
                        None => self.empty_tile(ui, slot, rect, &response, key, drop_here),
                    }
                }
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pad_tile(
        &mut self,
        ui: &mut Ui,
        graph: &Graph,
        pad: &Pad,
        state: Option<&PadState>,
        rect: Rect,
        response: &egui::Response,
        key: &str,
        commands: &mut Vec<Command>,
    ) {
        let color = pad
            .color
            .as_deref()
            .and_then(parse_hex)
            .unwrap_or(theme::ACCENT);
        let elapsed = self
            .started
            .get(&pad.name)
            .map(|s| s.elapsed().as_secs_f32());
        let playing = elapsed.is_some();
        let error = state.and_then(|s| s.error.as_ref());
        let learning = graph.learning
            == Some(ControlTarget::Pad {
                name: pad.name.clone(),
            });
        let fill = if playing {
            color.gamma_multiply(0.85)
        } else if response.hovered() {
            color.gamma_multiply(0.4)
        } else {
            color.gamma_multiply(0.22)
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, theme::CORNER_RADIUS, fill);
        let border = if learning { theme::ORANGE } else { color };
        let border_width = if learning { 2.5 } else { 1.0 };
        painter.rect_stroke(
            rect,
            theme::CORNER_RADIUS,
            Stroke::new(border_width, border),
            StrokeKind::Inside,
        );
        let ink = if playing {
            theme::BACKGROUND
        } else {
            theme::TEXT
        };
        let dim = if playing {
            theme::BACKGROUND.gamma_multiply(0.7)
        } else {
            theme::TEXT_DIM
        };
        if self.renaming.as_ref().is_some_and(|(n, _)| *n == pad.name) {
            self.rename_box(ui, &graph.soundboard, rect, commands);
        } else {
            let name = name_galley(ui, &pad.name, rect.width() - 12.0, ink);
            let at = pos2(rect.center().x, rect.center().y - name.size().y / 2.0 - 4.0);
            painter.galley(at, name, ink);
        }
        let small = FontId::proportional(11.0);
        painter.text(
            rect.left_top() + vec2(6.0, 4.0),
            Align2::LEFT_TOP,
            key,
            small.clone(),
            dim,
        );
        let mut badges = String::new();
        badges.push_str(match pad.mode {
            PadMode::OneShot => "",
            PadMode::Toggle => "⏯",
            PadMode::Hold => "✋",
        });
        if pad.looped {
            badges.push('⟲');
        }
        if pad.choke != 0 {
            badges.push_str(&format!(" C{}", pad.choke));
        }
        painter.text(
            rect.right_bottom() + vec2(-6.0, -6.0),
            Align2::RIGHT_BOTTOM,
            badges,
            small.clone(),
            dim,
        );
        let length = state.and_then(|s| s.length);
        let time = match (elapsed, length) {
            (Some(e), Some(l)) if !pad.looped => format!("−{}", clock((l - e).max(0.0))),
            (Some(e), _) => clock(e),
            (None, Some(l)) => clock(l),
            (None, None) => String::new(),
        };
        painter.text(
            rect.left_bottom() + vec2(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            time,
            small,
            dim,
        );
        if let (Some(e), Some(l)) = (elapsed, length)
            && l > 0.0
        {
            let fraction = if pad.looped {
                (e % l) / l
            } else {
                (e / l).min(1.0)
            };
            let bar = Rect::from_min_max(
                pos2(rect.left(), rect.bottom() - 3.0),
                pos2(rect.left() + rect.width() * fraction, rect.bottom()),
            );
            painter.rect_filled(bar, 0.0, theme::BACKGROUND.gamma_multiply(0.8));
        }
        if error.is_some() {
            painter.text(
                rect.right_top() + vec2(-6.0, 4.0),
                Align2::RIGHT_TOP,
                "⚠",
                FontId::proportional(13.0),
                theme::ORANGE,
            );
        }
        let primary_down = ui.input(|i| i.pointer.primary_down());
        if response.is_pointer_button_down_on() && primary_down && self.pointer_held.is_none() {
            self.pointer_held = Some(pad.name.clone());
            commands.push(Command::PressPad {
                name: pad.name.clone(),
            });
        } else if response.clicked() && self.pointer_held.is_none() {
            // Pressed and let go within one frame: still a press, and its release.
            let name = pad.name.clone();
            commands.push(Command::PressPad { name: name.clone() });
            commands.push(Command::ReleasePad { name });
        }
        let hover = match error {
            Some(e) => tf("cannot play: {error}", &[("error", e)]),
            None => pad.path.display().to_string(),
        };
        let response = response.clone().on_hover_text(hover);
        if response.double_clicked() {
            self.renaming = Some((pad.name.clone(), pad.name.clone()));
        }
        response.context_menu(|ui| self.pad_menu(ui, &graph.soundboard, pad, playing, commands));
    }

    fn empty_tile(
        &mut self,
        ui: &mut Ui,
        slot: u32,
        rect: Rect,
        response: &egui::Response,
        key: &str,
        drop_here: bool,
    ) {
        let painter = ui.painter_at(rect);
        let stroke = if drop_here || response.hovered() {
            Stroke::new(1.5, theme::ACCENT)
        } else {
            Stroke::new(1.0, theme::BORDER_SOFT)
        };
        painter.rect_stroke(rect, theme::CORNER_RADIUS, stroke, StrokeKind::Inside);
        painter.text(
            rect.left_top() + vec2(6.0, 4.0),
            Align2::LEFT_TOP,
            key,
            FontId::proportional(11.0),
            theme::TEXT_DIM.gamma_multiply(0.6),
        );
        if response.hovered() || drop_here {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "+",
                FontId::proportional(20.0),
                theme::TEXT_DIM,
            );
        }
        let response = response
            .clone()
            .on_hover_text(t("add a sound: click to pick a file, or drop one here"));
        if response.clicked() {
            self.open_chooser(ui, slot);
        }
    }

    fn pad_menu(
        &mut self,
        ui: &mut Ui,
        board: &Soundboard,
        pad: &Pad,
        playing: bool,
        commands: &mut Vec<Command>,
    ) {
        if ui.button(t("Rename…")).clicked() {
            self.renaming = Some((pad.name.clone(), pad.name.clone()));
            ui.close();
        }
        if ui.add_enabled(playing, Button::new(t("Stop"))).clicked() {
            commands.push(Command::StopPad {
                name: pad.name.clone(),
            });
            ui.close();
        }
        ui.separator();
        ui.label(theme::small_dim(t("When pressed")));
        for (mode, label, about) in [
            (
                PadMode::OneShot,
                t("One-shot"),
                t("plays to the end; pressing again starts it over"),
            ),
            (
                PadMode::Toggle,
                t("Toggle"),
                t("pressing starts it, pressing again stops it"),
            ),
            (
                PadMode::Hold,
                t("Hold"),
                t("plays while you hold the pad, key or MIDI note"),
            ),
        ] {
            if ui
                .radio(pad.mode == mode, label)
                .on_hover_text(about)
                .clicked()
                && pad.mode != mode
            {
                commands.push(edited(board, &pad.name, |p| p.mode = mode));
            }
        }
        let mut looped = pad.looped;
        if ui.checkbox(&mut looped, t("Loop")).changed() {
            commands.push(edited(board, &pad.name, |p| p.looped = looped));
        }
        ui.horizontal(|ui| {
            ui.label(t("Choke group"))
                .on_hover_text(t("pads in the same group cut each other off"));
            for group in 0..=4u8 {
                let label = if group == 0 {
                    t("none").to_owned()
                } else {
                    group.to_string()
                };
                if ui.selectable_label(pad.choke == group, label).clicked() && pad.choke != group {
                    commands.push(edited(board, &pad.name, |p| p.choke = group));
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label(t("Gain"));
            let id = ui.make_persistent_id(("pad-gain", &pad.name));
            let mut value: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(pad.gain_db);
            let response = ui.add(
                DragValue::new(&mut value)
                    .range(-40.0..=12.0)
                    .speed(0.1)
                    .fixed_decimals(1)
                    .suffix(" dB"),
            );
            if response.dragged() || response.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, value));
            } else {
                ui.data_mut(|d| d.remove_temp::<f32>(id));
            }
            let done = response.drag_stopped() || (response.changed() && !response.dragged());
            if done && (value - pad.gain_db).abs() > 0.01 {
                commands.push(edited(board, &pad.name, |p| p.gain_db = value));
            }
        });
        ui.label(theme::small_dim(t("Colour")));
        ui.horizontal(|ui| {
            let current = pad.color.as_deref().and_then(parse_hex);
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
                    commands.push(edited(board, &pad.name, |p| p.color = Some(hex.to_owned())));
                    ui.close();
                }
            }
        });
        ui.separator();
        let learn = ui.button(t("Learn MIDI control")).on_hover_text(t(
            "then press a pad or key on the MIDI input picked in Settings",
        ));
        if learn.clicked() {
            commands.push(Command::Learn {
                target: Some(ControlTarget::Pad {
                    name: pad.name.clone(),
                }),
            });
            ui.close();
        }
        if ui.button(t("Replace file…")).clicked() {
            self.open_chooser(ui, pad.slot);
            ui.close();
        }
        if ui.button(t("Remove pad")).clicked() {
            let mut board = board.clone();
            board.pads.retain(|p| p.name != pad.name);
            commands.push(Command::SetSoundboard { board });
            ui.close();
        }
    }

    fn rename_box(
        &mut self,
        ui: &mut Ui,
        board: &Soundboard,
        rect: Rect,
        commands: &mut Vec<Command>,
    ) {
        let Some((name, typed)) = &mut self.renaming else {
            return;
        };
        let field = Rect::from_center_size(rect.center(), vec2(rect.width() - 12.0, 22.0));
        let edit = ui.put(field, egui::TextEdit::singleline(typed));
        autofocus(ui, &edit);
        if edit.lost_focus() {
            let to = typed.trim().to_owned();
            let enter = ui.input(|i| i.key_pressed(Key::Enter));
            if enter && !to.is_empty() && to != *name && board.pad(&to).is_none() {
                commands.push(Command::RenamePad {
                    name: name.clone(),
                    to,
                });
            }
            self.renaming = None;
        }
    }

    /// The bank's keys press and release its pads while no text field has the focus; Esc
    /// stops every pad.
    fn keys(&mut self, ui: &Ui, graph: &Graph, commands: &mut Vec<Command>) {
        if ui.ctx().memory(|m| m.focused().is_some()) {
            return;
        }
        let events = ui.input(|i| i.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                pressed,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if !modifiers.is_none() {
                continue;
            }
            if key == Key::Escape && pressed {
                commands.push(Command::StopPads);
                continue;
            }
            let Some(index) = KEYS.iter().flatten().position(|k| *k == key) else {
                continue;
            };
            let slot = self.bank * PADS_PER_BANK + index as u32;
            if let Some(pad) = graph.soundboard.at(slot) {
                let name = pad.name.clone();
                commands.push(if pressed {
                    Command::PressPad { name }
                } else {
                    Command::ReleasePad { name }
                });
            }
        }
    }

    /// The mouse let go of the pad it pressed, wherever the pointer is now.
    fn release_pointer(&mut self, ui: &Ui, commands: &mut Vec<Command>) {
        if self.pointer_held.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            let name = self.pointer_held.take().unwrap_or_default();
            commands.push(Command::ReleasePad { name });
        }
    }

    /// Restarts each playing pad's clock from a new snapshot's elapsed time.
    fn follow_clock(&mut self, graph: &Graph) {
        let snapshot = std::ptr::from_ref(graph) as usize;
        if snapshot == self.seen {
            return;
        }
        self.seen = snapshot;
        let now = Instant::now();
        self.started = graph
            .pads
            .iter()
            .filter_map(|p| {
                let elapsed = p.playing?;
                Some((
                    p.name.clone(),
                    now - std::time::Duration::from_secs_f32(elapsed),
                ))
            })
            .collect();
    }

    fn open_chooser(&mut self, ui: &Ui, slot: u32) {
        if self.chooser.is_some() {
            return;
        }
        let ctx = ui.ctx().clone();
        let dialog = file_dialog::open(
            t("Pick a sound"),
            self.last_folder.as_deref(),
            file_dialog::Filter {
                name: t("Sound files"),
                glob: "*.wav *.flac *.mp3 *.ogg *.oga *.WAV *.FLAC *.MP3 *.OGG",
            },
            move || ctx.request_repaint(),
        );
        self.chooser = Some((slot, dialog));
    }

    fn take_chosen(&mut self, ui: &Ui, board: &Soundboard, commands: &mut Vec<Command>) {
        let Some(answer) = self.chooser.as_ref().and_then(|(_, d)| d.poll()) else {
            return;
        };
        let Some((slot, _)) = self.chooser.take() else {
            return;
        };
        match answer {
            Ok(Some(path)) => {
                self.last_folder = path.parent().map(Path::to_path_buf);
                let mut board = board.clone();
                match board.pads.iter_mut().find(|p| p.slot == slot) {
                    Some(pad) => pad.path = path,
                    None => add_pad(&mut board, slot, &path),
                }
                commands.push(Command::SetSoundboard { board });
            }
            Ok(None) => {}
            Err(e) => log::warn!("file chooser: {e}"),
        }
        ui.ctx().request_repaint();
    }
}

/// A pad's name over at most two lines, cut with "…", centred.
fn name_galley(
    ui: &Ui,
    name: &str,
    width: f32,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(
        name.to_owned(),
        TextFormat::simple(FontId::proportional(13.0), color),
    );
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: 2,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job.halign = Align::Center;
    ui.fonts_mut(|f| f.layout_job(job))
}

fn key_label(index: u32) -> &'static str {
    const LABELS: [&str; 32] = [
        "1", "2", "3", "4", "5", "6", "7", "8", "Q", "W", "E", "R", "T", "Y", "U", "I", "A", "S",
        "D", "F", "G", "H", "J", "K", "Z", "X", "C", "V", "B", "N", "M", ",",
    ];
    LABELS[index as usize]
}

/// `m:ss`.
fn clock(seconds: f32) -> String {
    let whole = seconds.ceil() as u32;
    format!("{}:{:02}", whole / 60, whole % 60)
}

fn bank_pads(board: &Soundboard, bank: u32) -> impl Iterator<Item = &Pad> {
    let range = bank * PADS_PER_BANK..(bank + 1) * PADS_PER_BANK;
    let mut pads: Vec<&Pad> = board
        .pads
        .iter()
        .filter(|p| range.contains(&p.slot))
        .collect();
    pads.sort_by_key(|p| p.slot);
    pads.into_iter()
}

/// The first empty slot from `bank` on.
fn next_free(board: &Soundboard, bank: u32) -> Option<u32> {
    (bank * PADS_PER_BANK..BANKS * PADS_PER_BANK).find(|s| board.at(*s).is_none())
}

fn add_pad(board: &mut Soundboard, slot: u32, path: &Path) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| t("Pad").to_owned());
    board.pads.push(Pad {
        name: unique_name(board, &stem),
        path: path.to_path_buf(),
        slot,
        gain_db: 0.0,
        color: None,
        mode: PadMode::OneShot,
        looped: false,
        choke: 0,
    });
}

/// The board with pad `name` changed by `change`.
fn edited(board: &Soundboard, name: &str, change: impl FnOnce(&mut Pad)) -> Command {
    let mut board = board.clone();
    if let Some(pad) = board.pads.iter_mut().find(|p| p.name == name) {
        change(pad);
    }
    Command::SetSoundboard { board }
}

/// `stem`, or `stem 2`, `stem 3`… when a pad already has that name.
fn unique_name(board: &Soundboard, stem: &str) -> String {
    let mut name = stem.to_owned();
    let mut n = 1;
    while board.pad(&name).is_some() {
        n += 1;
        name = format!("{stem} {n}");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(name: &str, slot: u32) -> Pad {
        Pad {
            name: name.into(),
            path: "/x.wav".into(),
            slot,
            gain_db: 0.0,
            color: None,
            mode: PadMode::OneShot,
            looped: false,
            choke: 0,
        }
    }

    #[test]
    fn a_dropped_file_takes_a_free_slot_and_a_free_name() {
        let mut board = Soundboard {
            pads: vec![pad("airhorn", 0), pad("airhorn 2", 1)],
            ..Soundboard::default()
        };
        assert_eq!(next_free(&board, 0), Some(2));
        assert_eq!(next_free(&board, 1), Some(PADS_PER_BANK));
        add_pad(&mut board, 2, Path::new("/sounds/airhorn.mp3"));
        assert_eq!(board.at(2).map(|p| p.name.as_str()), Some("airhorn 3"));
        assert!(is_sound(Path::new("/a/B.WAV")));
        assert!(!is_sound(Path::new("/a/plugin.clap")));
    }

    #[test]
    fn times_round_up_to_the_second() {
        assert_eq!(clock(0.2), "0:01");
        assert_eq!(clock(61.0), "1:01");
        assert_eq!(key_label(8), "Q");
        assert_eq!(key_label(31), ",");
    }
}
