//! Chains: one DAW-style strip per chain, after the mockups. Header with kind and status,
//! the input end, ordered slot cards with GUI and bypass buttons next to the input meter,
//! the output end, and a footer with the output meter and latency. A dashed card creates
//! a chain; a window picks plugins from the catalogue.

use canalis_chain::ipc::ToChain;
use canalis_chain::{Lane, MAX_LANES, SlotConfig, SlotPath, SlotSpec, lane_slot_id};
use canalis_pw::{
    AbSide, ChainConfig, ChainKind, ChainState, ChainStatus, Command, CompareStep, ControlTarget,
    Direction, GAIN_OFF_DB, Graph, MediaClass, Node, ParamInfo, PluginEntry, PluginRef, SlotInfo,
    would_cycle,
};
use egui::collapsing_header::CollapsingState;
use egui::containers::menu::MenuButton;
use egui::{
    Button, Color32, ComboBox, DragValue, Frame, Margin, Rect, RichText, Sense, Shape, Slider,
    Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use super::feedback;
use super::presets::{self, PRESETS};
use super::share;
use super::{
    ICONS, METER_FLOOR_DB, PALETTE, app_chips, app_streams, autofocus, chain_color,
    kind_tag_colors, parse_hex, smoothed,
};
use crate::i18n::{t, tf};
use crate::theme;
use crate::{autoeq, file_dialog};

const STRIP_WIDTH: f32 = 290.0;
const STRIP_PADDING: f32 = 12.0;
/// The input meter column beside the slots: two 6 px bars and their gap.
const METER_BAR: f32 = 6.0;
const METER_GAP: f32 = 2.0;
const METER_COLUMN: f32 = 26.0;
const METER_MIN_HEIGHT: f32 = 120.0;
const SQUARE_BUTTON: Vec2 = vec2(30.0, 30.0);
/// The parameter rows of a plugin slot scroll inside this height.
const PARAMS_MAX_HEIGHT: f32 = 240.0;
/// The lane pan field: room for "R100".
const PAN_WIDTH: f32 = 32.0;
/// Height of the dashed add boxes inside lanes and folders.
const SMALL_ADD_HEIGHT: f32 = 24.0;
const NEW_CHAIN_CARD: Vec2 = vec2(220.0, 120.0);
/// Space after the last strip and below the tallest, where the scrollbars float.
const VIEW_END_MARGIN: f32 = 24.0;
const DASH: (f32, f32) = (4.0, 3.0);
/// The integrated loudness a strip aims at when the user picked none, in LUFS.
/// The targets the strip offers: where each kind of delivery normalizes, in LUFS.
fn loudness_targets() -> [(&'static str, f32); 6] {
    [
        (t("Spotify · \"Loud\" setting"), -11.0),
        (t("Streaming · YouTube, Spotify, Tidal"), -14.0),
        ("Deezer", -15.0),
        (t("Apple Music · podcasts"), -16.0),
        (t("Broadcast · EBU R128"), -23.0),
        (t("Broadcast US · ATSC A/85"), -24.0),
    ]
}
const STATUS_BUTTONS_DROP: f32 = 4.0;
/// Slots in one lane: lane slot ids keep the position in a single digit.
const LANE_SLOTS: usize = 10;
/// Hover states fade in and out over this long, in seconds.
const HOVER_FADE: f32 = 0.22;
/// App chips in a strip's apps box before "+N more".
const CHAIN_APP_CHIPS: usize = 4;
/// Room between the header divider and the status row.
const STATUS_ROW_DROP: f32 = 4.0;

pub struct ChainsView {
    new_label: String,
    new_kind: ChainKind,
    /// The "New chain" card is unfolded into its form.
    creating: bool,
    /// The form just opened: scroll it into view once, it may be past the window's edge.
    reveal_form: bool,
    editing: Editing,
    /// A preset's "Get plugins" asked for Settings › Plugins.
    open_plugins: bool,
    /// "From a preset" opened the presets card while chains exist.
    show_presets: bool,
    /// The Import chain box: the pasted text and why the last try failed.
    importing: Option<ImportBox>,
    /// The file chooser an export or an import opened, until it answers.
    export_chooser: Option<(ChainConfig, file_dialog::Dialog)>,
    import_chooser: Option<file_dialog::Dialog>,
    /// AutoEQ's headphone index and the curve being fetched.
    autoeq: autoeq::Library,
}

/// What the strips are in the middle of, across frames.
#[derive(Default)]
struct Editing {
    picker: Option<Picker>,
    /// The soloed chain and the chains the solo muted, to unmute them when it ends.
    solo: Option<Solo>,
    /// A chain whose label is being typed: its name and the text so far.
    renaming: Option<(String, String)>,
    /// An end change that would let a microphone reach speakers, waiting for a yes.
    feedback: Option<FeedbackRisk>,
    /// What an export or import did, for the status bar.
    notice: Option<String>,
    /// A mic check under way.
    mic_check: Option<MicCheck>,
    /// "Export to file…" was picked for this chain: open the chooser.
    export: Option<ChainConfig>,
}

/// The chain's new ends and the speaker ends that start silent if the user goes ahead.
struct FeedbackRisk {
    chain: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    silenced: Vec<String>,
    speakers: String,
}

struct Solo {
    chain: String,
    muted: Vec<String>,
}

/// The "Add plugin" window, open for one chain.
struct Picker {
    chain: String,
    query: String,
    /// Headphone models from AutoEQ instead of the plugin catalogue.
    headphones: bool,
    /// The group lane the plugin goes into, else the chain's slot list.
    lane: Option<(usize, usize)>,
    /// The folder the plugin joins, at the end of its run.
    folder: Option<String>,
}

/// A strip being dragged to another place in the row, by chain name.
struct DragChain(String);

/// A slot card being dragged within its chain.
struct DragSlot {
    chain: String,
    index: usize,
}

impl Default for ChainsView {
    fn default() -> Self {
        ChainsView {
            new_label: String::new(),
            new_kind: ChainKind::Output,
            creating: false,
            reveal_form: false,
            editing: Editing::default(),
            open_plugins: false,
            show_presets: false,
            importing: None,
            export_chooser: None,
            import_chooser: None,
            autoeq: autoeq::Library::default(),
        }
    }
}

impl ChainsView {
    /// A preset asked to open Settings › Plugins, once.
    pub fn take_open_plugins(&mut self) -> bool {
        std::mem::take(&mut self.open_plugins)
    }

    pub fn show(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        if graph
            .chains
            .iter()
            .any(|c| c.status == ChainStatus::Running)
        {
            // Every frame (vsync caps it), so the meters fall smoothly between reports.
            ui.ctx().request_repaint();
        }
        let configs: Vec<ChainConfig> = graph.chains.iter().map(ChainState::config).collect();
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(12.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(12.0);
                    ui.spacing_mut().item_spacing.x = 20.0;
                    for chain in &graph.chains {
                        strip(ui, graph, &configs, chain, &mut self.editing, commands);
                    }
                    if graph.chains.is_empty() || self.show_presets {
                        let before = commands.len();
                        let shown = (!graph.chains.is_empty()).then_some(&mut self.show_presets);
                        self.open_plugins |= presets_card(ui, graph, shown, commands);
                        if commands.len() > before {
                            self.show_presets = false;
                        }
                    }
                    ui.vertical(|ui| {
                        self.new_chain(ui, graph, commands);
                        if self.creating {
                            return;
                        }
                        if !graph.chains.is_empty() {
                            ui.add_space(6.0);
                            let label = if self.show_presets {
                                t("Hide presets")
                            } else {
                                t("From a preset")
                            };
                            if ui.add(column_button(label)).clicked() {
                                self.show_presets = !self.show_presets;
                            }
                        }
                        ui.add_space(6.0);
                        if ui
                            .add(column_button(t("Import chain")))
                            .on_hover_text(t(
                                "paste a chain a colleague sent, or drop its .canalis-chain.toml file on the window",
                            ))
                            .clicked()
                        {
                            self.importing = Some(ImportBox::open(graph));
                        }
                    });
                    // Room past the last card, so the floating scrollbar never covers it.
                    ui.add_space(VIEW_END_MARGIN);
                });
                ui.add_space(VIEW_END_MARGIN);
            });
        self.picker_window(ui.ctx(), graph, commands);
        self.feedback_dialog(ui, commands);
        self.import_dialog(ui, graph, commands);
        self.choosers(ui, graph, commands);
        self.drive_mic_check(ui.ctx(), graph, commands);
    }

    /// Moves a mic check along: stop recording after `MIC_CHECK`, play the take once the
    /// chain has completed it, forget it when the playback ends.
    fn drive_mic_check(&mut self, ctx: &egui::Context, graph: &Graph, commands: &mut Vec<Command>) {
        let Some(check) = &mut self.editing.mic_check else {
            return;
        };
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
        let take = graph
            .chain(&check.chain)
            .and_then(|c| c.recording.as_ref())
            .filter(|r| std::path::Path::new(&r.path) == check.path);
        match check.phase {
            MicCheckPhase::Recording(since) if since.elapsed() >= MIC_CHECK => {
                commands.push(message(&check.chain, &ToChain::Record { path: None }));
                check.phase = MicCheckPhase::Saving;
            }
            MicCheckPhase::Recording(_) | MicCheckPhase::Saving
                if take.is_some_and(|t| t.done && t.error.is_some()) =>
            {
                let error = take.and_then(|t| t.error.clone()).unwrap_or_default();
                self.editing.notice = Some(tf("mic check: {error}", &[("error", &error)]));
                self.editing.mic_check = None;
            }
            MicCheckPhase::Saving if take.is_some_and(|t| t.done) => {
                commands.push(Command::PlayFile {
                    path: check.path.clone(),
                    delete_after: true,
                });
                check.phase = MicCheckPhase::Playing(std::time::Instant::now());
            }
            // The service shows the playback on its next snapshot; give it a moment.
            MicCheckPhase::Playing(since)
                if graph.playing.is_none()
                    && since.elapsed() > std::time::Duration::from_secs(1) =>
            {
                self.editing.mic_check = None;
            }
            _ => {}
        }
    }

    /// Opens the save chooser an export asked for, and acts on what the choosers answer.
    fn choosers(&mut self, ui: &Ui, graph: &Graph, commands: &mut Vec<Command>) {
        if let Some(chain) = self.editing.export.take() {
            let ctx = ui.ctx().clone();
            let folder = share::default_folder();
            let dialog = file_dialog::save(
                &tf("Export {label}", &[("label", &chain.label)]),
                &share::default_name(&chain),
                folder.as_deref(),
                chain_files(),
                move || ctx.request_repaint(),
            );
            self.export_chooser = Some((chain, dialog));
        }
        if let Some(answer) = self.export_chooser.as_ref().and_then(|(_, d)| d.poll())
            && let Some((chain, _)) = self.export_chooser.take()
        {
            let saved = match answer {
                Ok(Some(path)) => share::write(&chain, &path),
                Ok(None) => return,
                Err(e) => {
                    log::warn!("file chooser: {e}");
                    share::export_file(&chain)
                }
            };
            self.editing.notice = Some(match saved {
                Ok(path) => tf(
                    "exported {label} to {path}",
                    &[("label", &chain.label), ("path", &path.display())],
                ),
                Err(e) => tf("export failed: {error}", &[("error", &e)]),
            });
        }
        if let Some(answer) = self
            .import_chooser
            .as_ref()
            .and_then(file_dialog::Dialog::poll)
        {
            self.import_chooser = None;
            let result = match answer {
                Ok(Some(path)) => share::import_file(&path, graph),
                Ok(None) => return,
                Err(e) => Err(tf(
                    "the file chooser did not open ({error}); paste the chain or drop its file on the window",
                    &[("error", &e)],
                )),
            };
            match result {
                Ok(imported) => {
                    self.editing.notice = Some(imported.summary());
                    commands.extend(imported.commands);
                    self.importing = None;
                }
                Err(e) => {
                    self.importing
                        .get_or_insert_with(|| ImportBox::open(graph))
                        .error = Some(e);
                }
            }
        }
    }

    /// What an export or an import did, once, for the status bar.
    pub fn take_notice(&mut self) -> Option<String> {
        self.editing.notice.take()
    }

    /// The Import chain box: paste, Import; a file dropped on the window does the same.
    fn import_dialog(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        let Some(import) = &mut self.importing else {
            return;
        };
        let ImportBox {
            text,
            error,
            library,
        } = import;
        let mut close = false;
        let modal = egui::Modal::new(ui.scope_id().with("import-chain")).show(ui.ctx(), |ui| {
            ui.set_width(460.0);
            ui.label(RichText::new(t("Import chain")).size(15.0).strong());
            ui.label(theme::small_dim(t(
                "Choose the file someone sent you, drop it on the window, or paste the text \
                 (Export to file or Copy to clipboard in a chain's ⋮ menu make them).",
            )));
            ui.add_space(6.0);
            if !library.is_empty() {
                ui.label(
                    RichText::new(t("Your chains"))
                        .size(12.5)
                        .color(theme::TEXT_DIM),
                );
                egui::ScrollArea::vertical()
                    .id_salt("chain-library")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for shelved in library.iter() {
                            ui.horizontal(|ui| {
                                let (bg, fg) = kind_tag_colors(shelved.kind);
                                chip(ui, kind_label(shelved.kind), bg, fg);
                                ui.label(&shelved.label)
                                    .on_hover_text(shelved.path.display().to_string());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button(t("Import")).clicked() {
                                            match share::import_file(&shelved.path, graph) {
                                                Ok(imported) => {
                                                    self.editing.notice = Some(imported.summary());
                                                    commands.extend(imported.commands);
                                                    close = true;
                                                }
                                                Err(e) => *error = Some(e),
                                            }
                                        }
                                        ui.add(
                                            egui::Label::new(theme::small_dim(
                                                shelved.plugins.join(" → "),
                                            ))
                                            .truncate(),
                                        );
                                    },
                                );
                            });
                        }
                    });
                ui.add_space(6.0);
                ui.label(theme::small_dim(t("or from somewhere else:")));
            }
            let choosing = self.import_chooser.is_some();
            if ui
                .add_enabled(
                    !choosing,
                    Button::new(if choosing {
                        t("Choosing…")
                    } else {
                        t("Choose file…")
                    }),
                )
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.import_chooser = Some(file_dialog::open(
                    t("Import a chain"),
                    None,
                    chain_files(),
                    move || ctx.request_repaint(),
                ));
            }
            ui.add_space(4.0);
            ui.label(theme::small_dim(t("or paste it here:")));
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .code_editor()
                            .desired_rows(10)
                            .desired_width(f32::INFINITY)
                            .hint_text("version = …\n[[chain]]\n…"),
                    );
                });
            if let Some(error) = error.as_deref() {
                ui.label(RichText::new(error).size(12.0).color(theme::RED_SOFT));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!text.trim().is_empty(), Button::new(t("Import")))
                        .clicked()
                    {
                        match share::import(text, graph) {
                            Ok(imported) => {
                                self.editing.notice = Some(imported.summary());
                                commands.extend(imported.commands);
                                close = true;
                            }
                            Err(e) => *error = Some(e),
                        }
                    }
                    if ui.button(t("Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        });
        if close || modal.should_close() {
            self.importing = None;
        }
    }

    fn new_chain(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        if !self.creating {
            let (rect, response) = ui.allocate_exact_size(NEW_CHAIN_CARD, Sense::click());
            let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
            let fade = hover_fade(ui, &response);
            let color = theme::TEXT_DIM.lerp_to_gamma(theme::TEXT, fade);
            if fade > 0.0 {
                ui.painter().rect_filled(
                    rect,
                    theme::CORNER_RADIUS,
                    theme::ACCENT.gamma_multiply(0.05 * fade),
                );
            }
            dashed_rect(ui, rect, theme::BORDER.lerp_to_gamma(theme::ACCENT, fade));
            let painter = ui.painter();
            painter.text(
                rect.center() - vec2(0.0, 22.0),
                egui::Align2::CENTER_CENTER,
                "+",
                egui::FontId::proportional(22.0),
                color,
            );
            painter.text(
                rect.center() + vec2(0.0, 6.0),
                egui::Align2::CENTER_CENTER,
                t("New chain"),
                egui::FontId::proportional(14.0),
                color,
            );
            painter.text(
                rect.center() + vec2(0.0, 26.0),
                egui::Align2::CENTER_CENTER,
                t("output · mic · insert · cable"),
                egui::FontId::proportional(11.0),
                theme::TEXT_DIM,
            );
            if response.clicked() {
                self.creating = true;
                self.reveal_form = true;
            }
            return;
        }
        let (_, form) = panel(ui, theme::BORDER, |ui| {
            ui.set_width(NEW_CHAIN_CARD.x);
            // The form fades in where the dashed card was.
            let shown =
                ui.ctx()
                    .animate_bool_with_time(ui.scope_id().with("new-chain-form"), true, 0.18);
            ui.set_opacity(shown);
            ui.label(RichText::new(t("New chain")).strong());
            ui.add_space(6.0);
            let edit = ui.text_edit_singleline(&mut self.new_label);
            autofocus(ui, &edit);
            ComboBox::from_id_salt("new-chain-kind")
                .selected_text(kind_label(self.new_kind))
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for kind in ChainKind::ALL {
                        ui.selectable_value(&mut self.new_kind, kind, kind_label(kind));
                    }
                });
            ui.label(theme::small_dim(kind_help(self.new_kind)));
            ui.add_space(6.0);
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                let valid = !self.new_label.trim().is_empty();
                if ui.add_enabled(valid, Button::new(t("Create"))).clicked() || (valid && enter) {
                    let label = self.new_label.trim().to_owned();
                    commands.push(Command::CreateChain {
                        label: label.clone(),
                        kind: self.new_kind,
                    });
                    // A fresh chain goes to the default device, so it makes sound right away.
                    let (inputs, outputs) = presets::default_ends(graph, self.new_kind);
                    if !inputs.is_empty() || !outputs.is_empty() {
                        commands.push(Command::SetChainEndpoints {
                            name: canalis_pw::virtual_node_name(&label),
                            inputs,
                            outputs,
                        });
                    }
                    self.new_label.clear();
                    self.creating = false;
                }
                if ui.button(t("Cancel")).clicked() {
                    self.new_label.clear();
                    self.creating = false;
                }
            });
        });
        if std::mem::take(&mut self.reveal_form) {
            form.scroll_to_me(Some(egui::Align::Max));
        }
    }

    /// The headphone picker: AutoEQ's measured models, searchable; a pick fetches the
    /// model's curve and adds a Claritas EQ slot that draws it.
    fn headphone_window(
        &mut self,
        ctx: &egui::Context,
        graph: &Graph,
        commands: &mut Vec<Command>,
    ) {
        let Some(picker) = &mut self.editing.picker else {
            return;
        };
        let Some(chain) = graph.chain(&picker.chain) else {
            self.editing.picker = None;
            return;
        };
        if let Some((entry, profile)) = self.autoeq.take_profile() {
            let spec = profile.map(|profile| {
                presets::claritas_eq_curve(graph, &profile.bands, profile.preamp_db)
            });
            match spec {
                Ok(Some(spec)) => {
                    let mut config = SlotConfig::new(spec);
                    config.label = Some(entry.name.clone());
                    commands.push(add_slot(chain, config, picker.lane, picker.folder.clone()));
                    self.editing.notice = Some(tf(
                        "added the AutoEQ correction for {name} ({source})",
                        &[("name", &entry.name), ("source", &entry.source)],
                    ));
                    self.editing.picker = None;
                    return;
                }
                Ok(None) => {
                    self.editing.notice = Some(
                        t("Claritas EQ is missing from this install; reinstall Canalis").into(),
                    );
                }
                Err(e) => {
                    self.editing.notice = Some(tf(
                        "AutoEQ for {name}: {error}",
                        &[("name", &entry.name), ("error", &e)],
                    ));
                }
            }
        }
        let wake = {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };
        let mut open = true;
        let mut pick = None;
        let fetching = self.autoeq.fetching();
        egui::Window::new(t("Headphone correction"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([460.0, 420.0])
            .show(ctx, |ui| {
                ui.label(theme::small_dim(t(
                    "Claritas EQ with the curve AutoEQ measured for your model, so it \
                     sounds neutral. Put it on the chain your headphones play from.",
                )));
                let search = ui.add(
                    egui::TextEdit::singleline(&mut picker.query)
                        .hint_text(t("search your headphone model"))
                        .desired_width(ui.available_width()),
                );
                autofocus(ui, &search);
                if fetching {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(theme::small_dim(t("fetching the curve…")));
                    });
                }
                ui.add_space(4.0);
                match self.autoeq.index(wake.clone()) {
                    autoeq::Index::Idle | autoeq::Index::Loading => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(theme::small_dim(t("loading the AutoEQ index…")));
                        });
                    }
                    autoeq::Index::Failed(e) => {
                        ui.label(RichText::new(e.as_str()).size(12.0).color(theme::RED_SOFT));
                    }
                    autoeq::Index::Ready(entries) => {
                        let words: Vec<String> = picker
                            .query
                            .to_lowercase()
                            .split_whitespace()
                            .map(str::to_owned)
                            .collect();
                        let matching: Vec<&autoeq::Entry> = entries
                            .iter()
                            .filter(|e| {
                                let name = e.name.to_lowercase();
                                words.iter().all(|w| name.contains(w.as_str()))
                            })
                            .take(HEADPHONES_SHOWN)
                            .collect();
                        if words.is_empty() {
                            ui.label(theme::small_dim(tf(
                                "{n} measured models; type part of the name",
                                &[("n", &entries.len())],
                            )));
                            return;
                        }
                        if matching.is_empty() {
                            ui.label(theme::small_dim(t("no model by that name in AutoEQ")));
                        }
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for entry in matching {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            !fetching,
                                            Button::new(&entry.name).frame(false),
                                        )
                                        .clicked()
                                    {
                                        pick = Some(entry.clone());
                                    }
                                    ui.label(theme::small_dim(&entry.source));
                                });
                            }
                        });
                    }
                }
            });
        if let Some(entry) = pick {
            self.autoeq.request(entry, wake);
        }
        if !open {
            self.editing.picker = None;
        }
    }

    /// Asks before a microphone can reach speakers; yes adds the ends with the speaker
    /// sends at −∞, to be raised by hand.
    fn feedback_dialog(&mut self, ui: &mut Ui, commands: &mut Vec<Command>) {
        let Some(risk) = &self.editing.feedback else {
            return;
        };
        let Some(yes) = feedback::ask(
            ui,
            &tf(
                "This sends the mic to {speakers}.",
                &[("speakers", &risk.speakers)],
            ),
            t("\n\nThe new send starts at −∞ (off): raise its dB slowly once you are sure."),
            t("Connect at −∞"),
        ) else {
            return;
        };
        let Some(risk) = self.editing.feedback.take() else {
            return;
        };
        if yes {
            for target in &risk.silenced {
                commands.push(Command::SetChainGain {
                    name: risk.chain.clone(),
                    target: target.clone(),
                    gain_db: canalis_pw::GAIN_OFF_DB,
                });
            }
            commands.push(Command::SetChainEndpoints {
                name: risk.chain,
                inputs: risk.inputs,
                outputs: risk.outputs,
            });
        }
    }

    fn picker_window(&mut self, ctx: &egui::Context, graph: &Graph, commands: &mut Vec<Command>) {
        if self.editing.picker.as_ref().is_some_and(|p| p.headphones) {
            self.headphone_window(ctx, graph, commands);
            return;
        }
        let Some(picker) = &mut self.editing.picker else {
            return;
        };
        let Some(chain) = graph.chain(&picker.chain) else {
            self.editing.picker = None;
            return;
        };
        let mut open = true;
        let mut chosen = None;
        egui::Window::new(t("Add plugin"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([460.0, 420.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut picker.query)
                            .hint_text(t("search name, vendor or id"))
                            .desired_width(ui.available_width() - 90.0),
                    );
                    autofocus(ui, &search);
                    if ui
                        .add_enabled(!graph.scanning, Button::new(t("Rescan")))
                        .clicked()
                    {
                        commands.push(Command::ScanPlugins);
                    }
                });
                if graph.scanning {
                    ui.label(theme::small_dim(t("scanning…")));
                } else if graph.plugins.is_empty() {
                    ui.label(theme::small_dim(t("no plugins found yet; get some below")));
                }
                if let Some(why) = &graph.lv2_off {
                    ui.label(RichText::new(why).size(12.0).color(theme::ORANGE));
                }
                get_more_plugins(ui, graph.lv2_off.is_some());
                ui.add_space(4.0);
                let query = picker.query.to_lowercase();
                // Recent stays out of the way while searching: the matches are below.
                let recent: Vec<&PluginEntry> = if query.is_empty() {
                    graph
                        .recent_plugins
                        .iter()
                        .filter_map(|r| graph.plugins.iter().find(|p| r.matches(p)))
                        .collect()
                } else {
                    Vec::new()
                };
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if !recent.is_empty() {
                        ui.label(theme::small_dim(t("Recent")));
                        for plugin in recent {
                            if plugin_row(ui, plugin) {
                                chosen = Some(plugin);
                            }
                        }
                        ui.add_space(4.0);
                        ui.separator();
                        ui.label(theme::small_dim(t("All plugins")));
                    }
                    for plugin in graph.plugins.iter().filter(|p| {
                        query.is_empty()
                            || p.name.to_lowercase().contains(&query)
                            || p.vendor.to_lowercase().contains(&query)
                            || p.id.to_lowercase().contains(&query)
                    }) {
                        if plugin_row(ui, plugin) {
                            chosen = Some(plugin);
                        }
                    }
                });
            });
        if let Some(plugin) = chosen {
            commands.push(Command::RememberPlugin {
                plugin: PluginRef::of(plugin),
            });
            commands.push(add_slot(
                chain,
                SlotConfig::new(slot_spec(plugin)),
                picker.lane,
                picker.folder.clone(),
            ));
            open = false;
        }
        if !open {
            self.editing.picker = None;
        }
    }
}

/// One catalogue line: name, vendor and format; `true` when clicked.
fn plugin_row(ui: &mut Ui, plugin: &PluginEntry) -> bool {
    let text = if plugin.vendor.is_empty() {
        plugin.name.clone()
    } else {
        format!("{}  —  {}", plugin.name, plugin.vendor)
    };
    let format = if plugin.is_lv2() { "LV2" } else { "CLAP" };
    ui.horizontal(|ui| {
        let clicked = ui
            .selectable_label(false, text)
            .on_hover_text(format!("{}\n{}", plugin.id, plugin.path))
            .clicked();
        chip(ui, format, theme::BORDER_SOFT, theme::TEXT_DIM);
        clicked
    })
    .inner
}

/// A fresh slot for a catalogue plugin.
fn slot_spec(plugin: &PluginEntry) -> SlotSpec {
    if plugin.is_lv2() {
        SlotSpec::Lv2 {
            uri: plugin.id.clone(),
            params: Default::default(),
            state: None,
            preset: None,
        }
    } else {
        SlotSpec::Clap {
            path: plugin.path.clone(),
            id: plugin.id.clone(),
            params: Default::default(),
            state: None,
        }
    }
}

/// The empty view's offer: one click per preset chain. A preset whose plugins are missing
/// says so, with a way to get them; `true` when that was clicked.
/// `shown` is the toggle that opened the card, which its × closes; the card on an empty
/// view has none.
fn presets_card(
    ui: &mut Ui,
    graph: &Graph,
    shown: Option<&mut bool>,
    commands: &mut Vec<Command>,
) -> bool {
    let mut get_plugins = false;
    panel(ui, theme::BORDER, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("Start with a preset")).strong());
            if let Some(shown) = shown {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            Button::new(RichText::new("×").size(14.0).color(theme::TEXT_DIM))
                                .frame(false),
                        )
                        .on_hover_text(t("close"))
                        .clicked()
                    {
                        *shown = false;
                    }
                });
            }
        });
        ui.label(theme::small_dim(t(
            "a chain with its slots and ends, ready to tweak",
        )));
        ui.add_space(8.0);
        for preset in &PRESETS {
            let missing = preset.missing(graph);
            Frame::new()
                .fill(theme::BACKGROUND)
                .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
                .corner_radius(theme::CORNER_RADIUS)
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    // The kind and Create on the top row, the name in full under the kind,
                    // then the description.
                    ui.horizontal(|ui| {
                        let (bg, fg) = kind_tag_colors(preset.kind);
                        chip(ui, kind_label(preset.kind), bg, fg);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(missing.is_none(), Button::new(t("Create")))
                                .clicked()
                            {
                                commands.extend(presets::apply(preset, graph));
                            }
                        });
                    });
                    ui.label(preset.title());
                    ui.label(
                        RichText::new(preset.about())
                            .size(11.0)
                            .color(theme::TEXT_DIM),
                    );
                    if let Some(missing) = missing {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("⚠ {}", missing.text))
                                    .size(11.0)
                                    .color(theme::ORANGE),
                            );
                            if missing.installable && ui.small_button(t("Get plugins")).clicked() {
                                get_plugins = true;
                            }
                        });
                    }
                });
            ui.add_space(6.0);
        }
    });
    get_plugins
}

/// The add-slot menu's two windows: the plugin catalogue and AutoEQ's headphones; the
/// slot lands in `lane` or `folder` when given.
fn picker_entries(
    ui: &mut Ui,
    picker: &mut Option<Picker>,
    chain: &str,
    lane: Option<(usize, usize)>,
    folder: Option<String>,
) {
    let mut open = |headphones| {
        *picker = Some(Picker {
            chain: chain.to_owned(),
            query: String::new(),
            headphones,
            lane,
            folder: folder.clone(),
        });
    };
    if ui
        .button(t("Plugin…"))
        .on_hover_text(t("a CLAP or LV2 plugin from the catalogue"))
        .clicked()
    {
        open(false);
        ui.close();
    }
    if ui
        .button(t("Headphone correction…"))
        .on_hover_text(t(
            "Claritas EQ with the AutoEQ curve for your headphone model",
        ))
        .clicked()
    {
        open(true);
        ui.close();
    }
}

/// Matches listed at once in the headphone picker; typing more narrows them.
const HEADPHONES_SHOWN: usize = 200;

/// A chain's load shows orange above this share of the cycle, in %.
const LOAD_WARN: f32 = 70.0;
/// Past this a mic chain's delay is felt in a call.
const CALL_LATENCY_MS: f64 = 20.0;

/// The Import chain box: the library folder's chains, the pasted text, the last error.
struct ImportBox {
    text: String,
    error: Option<String>,
    library: Vec<share::Shelved>,
}

impl ImportBox {
    /// Reads the library folder once, when the box opens.
    fn open(graph: &Graph) -> Self {
        ImportBox {
            text: String::new(),
            error: None,
            library: share::library(graph),
        }
    }
}

/// What the file choosers offer for chains.
fn chain_files() -> file_dialog::Filter {
    file_dialog::Filter {
        name: t("Canalis chain"),
        glob: "*.canalis-chain.toml",
    }
}

/// A full-width quiet button under the New chain card.
fn column_button(label: &str) -> Button<'_> {
    Button::new(RichText::new(label).size(12.0).color(theme::TEXT_DIM))
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
        .min_size(vec2(NEW_CHAIN_CARD.x, 26.0))
}

/// A strip-wide panel with a coloured top edge; the strips sit in a horizontal row, so the
/// contents get their own vertical layout instead of inheriting the row's.
fn panel<R>(ui: &mut Ui, top: Color32, add: impl FnOnce(&mut Ui) -> R) -> (R, egui::Response) {
    let response = Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(Margin::same(STRIP_PADDING as i8))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_min_width(STRIP_WIDTH - 2.0 * STRIP_PADDING);
                ui.set_max_width(STRIP_WIDTH - 2.0 * STRIP_PADDING);
                add(ui)
            })
            .inner
        });
    let rect = response.response.rect;
    let edge = Rect::from_min_max(rect.left_top(), pos2(rect.right(), rect.top() + 3.0));
    ui.painter().rect_filled(
        edge,
        egui::CornerRadius {
            nw: 3,
            ne: 3,
            sw: 0,
            se: 0,
        },
        top,
    );
    (response.inner, response.response)
}

fn strip(
    ui: &mut Ui,
    graph: &Graph,
    configs: &[ChainConfig],
    chain: &ChainState,
    editing: &mut Editing,
    commands: &mut Vec<Command>,
) {
    let (_, response) = panel(ui, chain_color(chain), |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        // The fold lives in the config; the state only animates it.
        let open = !chain.collapsed;
        let mut state = CollapsingState::load_with_default_open(
            ui.ctx(),
            egui::Id::unique((&chain.name, "strip-body")),
            open,
        );
        state.set_open(open);
        header(ui, graph, chain, editing, commands);
        if !open {
            folded_summary(ui, chain);
        }
        state.show_body_unindented(ui, |ui| {
            strip_body(ui, graph, configs, chain, editing, commands)
        });
    });
    let pointer_x = ui.input(|i| i.pointer.interact_pos().map(|p| p.x));
    let after = pointer_x.is_some_and(|x| x > response.rect.center().x);
    if let Some(dragged) = response.dnd_hover_payload::<DragChain>()
        && dragged.0 != chain.name
    {
        let x = if after {
            response.rect.right() + 10.0
        } else {
            response.rect.left() - 10.0
        };
        ui.painter().line_segment(
            [
                pos2(x, response.rect.top()),
                pos2(x, response.rect.bottom()),
            ],
            Stroke::new(3.0, theme::ACCENT),
        );
    }
    if let Some(dragged) = response.dnd_release_payload::<DragChain>()
        && dragged.0 != chain.name
    {
        let mut names: Vec<String> = graph
            .chains
            .iter()
            .map(|c| c.name.clone())
            .filter(|n| *n != dragged.0)
            .collect();
        let at = names.iter().position(|n| *n == chain.name).unwrap_or(0) + usize::from(after);
        names.insert(at.min(names.len()), dragged.0.clone());
        commands.push(Command::ReorderChains { names });
    }
}

/// Everything under the header: ends, slots and meters, loudness.
fn strip_body(
    ui: &mut Ui,
    graph: &Graph,
    configs: &[ChainConfig],
    chain: &ChainState,
    editing: &mut Editing,
    commands: &mut Vec<Command>,
) {
    ui.add_space(6.0);
    section_label(
        ui,
        if chain.kind.picks_inputs() && chain.inputs.len() > 1 {
            t("Input · summed")
        } else {
            t("Input")
        },
    );
    if chain.kind.picks_inputs() {
        endpoints(
            ui,
            graph,
            configs,
            chain,
            Direction::Input,
            &mut editing.feedback,
            commands,
        );
    } else {
        apps_end(
            ui,
            graph,
            &chain.name,
            t("Apps · this chain is an output device"),
            commands,
            |_, _| {},
        );
    }
    let keyed = chain.slot_infos.values().any(|i| i.sidechain);
    if keyed || chain.key.is_some() {
        ui.add_space(6.0);
        section_label(ui, t("Key · the sidechain the keyed slots hear"));
        key_box(ui, graph, chain, commands);
    }
    ui.add_space(8.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let slots_height = ui
            .vertical(|ui| {
                ui.set_width(ui.available_width() - METER_COLUMN - 8.0);
                slots(ui, graph, chain, &mut editing.picker, commands);
            })
            .response
            .rect
            .height();
        ui.vertical(|ui| {
            ui.set_width(METER_COLUMN);
            ui.label(RichText::new(t("in")).size(10.0).color(theme::TEXT_DIM));
            meter_vertical(
                ui,
                smoothed(ui, (&chain.name, "in"), chain.meters.input()),
                slots_height.max(METER_MIN_HEIGHT) - 18.0,
            );
        });
    });
    ui.add_space(8.0);
    section_label(ui, t("Output"));
    if chain.kind.picks_outputs() {
        endpoints(
            ui,
            graph,
            configs,
            chain,
            Direction::Output,
            &mut editing.feedback,
            commands,
        );
    } else {
        let source = chain.kind.source_node_name(&chain.name);
        apps_end(
            ui,
            graph,
            &source,
            t("Apps · this chain is a microphone"),
            commands,
            |ui, commands| gain_drag(ui, chain, &source, commands),
        );
        ui.add_space(6.0);
        section_label(ui, t("Listen · hear yourself through"));
        endpoints(
            ui,
            graph,
            configs,
            chain,
            Direction::Output,
            &mut editing.feedback,
            commands,
        );
    }
    ui.add_space(8.0);
    footer(ui, graph, chain, &mut editing.mic_check, commands);
}

/// A folded strip's one line: the output level and what the chain holds.
fn folded_summary(ui: &mut Ui, chain: &ChainState) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let slots = chain.slots.len();
        let text = if slots == 1 {
            tf("{n} slot", &[("n", &slots)])
        } else {
            tf("{n} slots", &[("n", &slots)])
        };
        ui.label(RichText::new(text).size(11.0).color(theme::TEXT_DIM));
        meter_horizontal(
            ui,
            smoothed(ui, (&chain.name, "out"), chain.meters.output()),
            ui.available_width(),
        );
    });
}

fn header(
    ui: &mut Ui,
    graph: &Graph,
    chain: &ChainState,
    editing: &mut Editing,
    commands: &mut Vec<Command>,
) {
    let Editing {
        solo,
        renaming,
        notice,
        export,
        ..
    } = editing;
    // The whole top row is the drag handle. The drag layer sits under the row's widgets
    // (egui gives a click to nothing when a drag-only widget is on top), so the name
    // and the menu keep their clicks and a drag anywhere else picks the strip up.
    let row = ui.scope_builder(egui::UiBuilder::new().sense(Sense::drag()), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let chevron = if chain.collapsed { "▸" } else { "▾" };
            if ui
                .add(Button::new(RichText::new(chevron).size(13.0)).frame(false))
                .on_hover_text(if chain.collapsed {
                    t("show the whole strip")
                } else {
                    t("fold the strip to its header")
                })
                .clicked()
            {
                commands.push(Command::SetChainCollapsed {
                    name: chain.name.clone(),
                    collapsed: !chain.collapsed,
                });
            }
            if let Some(icon) = &chain.icon {
                ui.label(RichText::new(icon).size(16.0).color(chain_color(chain)));
            }
            if let Some((name, text)) = renaming.as_mut().filter(|(n, _)| *n == chain.name) {
                let edit = ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
                autofocus(ui, &edit);
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if enter || (edit.lost_focus() && !escape) {
                    commands.push(Command::RenameChain {
                        name: name.clone(),
                        label: text.clone(),
                    });
                    *renaming = None;
                } else if escape {
                    *renaming = None;
                }
            } else {
                let label = ui
                    .add(
                        egui::Label::new(RichText::new(&chain.label).strong())
                            .selectable(false)
                            .sense(Sense::click()),
                    )
                    .on_hover_text(t("double-click to rename"));
                if label.double_clicked() {
                    *renaming = Some((chain.name.clone(), chain.label.clone()));
                }
            }
            let (bg, fg) = kind_tag_colors(chain.kind);
            chip(ui, kind_label(chain.kind), bg, fg);
            // Shown whenever the chain runs (it reports 0 only while idle), so light chains
            // read "<1%" instead of blinking around a threshold.
            if let Some(load) = chain.load.filter(|l| *l > 0.0) {
                let color = if load > LOAD_WARN {
                    theme::ORANGE
                } else {
                    theme::TEXT_DIM
                };
                let text = if load < 1.0 {
                    "<1%".to_owned()
                } else {
                    format!("{load:.0}%")
                };
                ui.label(RichText::new(text).size(10.5).color(color))
                    .on_hover_text(t(
                        "DSP load: the most of an audio cycle the chain used in the last half \
                         second; past 100% audio drops, so Canalis bypasses the slowest slots",
                    ));
            }
            latency_label(ui, graph, chain);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // It senses drags too, so a drag that starts on the menu is not the strip's.
                let more = Button::new(RichText::new("⋮").size(18.0))
                    .frame(false)
                    .sense(Sense::click_and_drag());
                MenuButton::from_button(more).ui(ui, |ui| {
                    if ui.button(t("Rename…")).clicked() {
                        *renaming = Some((chain.name.clone(), chain.label.clone()));
                        ui.close();
                    }
                    ui.label(theme::small_dim(t("Colour")));
                    ui.horizontal(|ui| {
                        let current = chain.color.as_deref().and_then(parse_hex);
                        for hex in PALETTE {
                            let color = parse_hex(hex).unwrap_or(theme::ACCENT);
                            let (rect, response) =
                                ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
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
                                commands.push(Command::SetChainColor {
                                    name: chain.name.clone(),
                                    color: Some(hex.to_owned()),
                                });
                                ui.close();
                            }
                        }
                    });
                    if chain.color.is_some() && ui.button(t("Kind's colour")).clicked() {
                        commands.push(Command::SetChainColor {
                            name: chain.name.clone(),
                            color: None,
                        });
                        ui.close();
                    }
                    ui.label(theme::small_dim(t("Icon")));
                    ui.horizontal_wrapped(|ui| {
                        ui.set_max_width(6.0 * 26.0);
                        for icon in ICONS {
                            let chosen = chain.icon.as_deref() == Some(icon);
                            if ui
                                .selectable_label(chosen, RichText::new(icon).size(15.0))
                                .clicked()
                            {
                                commands.push(Command::SetChainIcon {
                                    name: chain.name.clone(),
                                    icon: Some(icon.to_owned()),
                                });
                                ui.close();
                            }
                        }
                    });
                    if chain.icon.is_some() && ui.button(t("No icon")).clicked() {
                        commands.push(Command::SetChainIcon {
                            name: chain.name.clone(),
                            icon: None,
                        });
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button(t("Export to file…"))
                        .on_hover_text(t("pick where to save it and its name, to send to someone"))
                        .clicked()
                    {
                        *export = Some(chain.config());
                        ui.close();
                    }
                    if ui
                        .button(t("Copy to clipboard"))
                        .on_hover_text(t("paste it in a chat; Import chain reads it back"))
                        .clicked()
                    {
                        ui.ctx().copy_text(share::export_text(&chain.config()));
                        *notice = Some(tf(
                            "{label} copied to the clipboard",
                            &[("label", &chain.label)],
                        ));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button(RichText::new(t("Delete chain")).color(theme::RED))
                        .clicked()
                    {
                        commands.push(Command::DeleteChain {
                            name: chain.name.clone(),
                        });
                        ui.close();
                    }
                });
            });
        })
    });
    let row = row.response;
    if row.dragged() {
        egui::DragAndDrop::set_payload(ui.ctx(), DragChain(chain.name.clone()));
    }
    let cursor = if row.dragged() {
        egui::CursorIcon::Grabbing
    } else {
        egui::CursorIcon::Grab
    };
    row.on_hover_cursor(cursor)
        .on_hover_text(t("drag to reorder"));
    ui.separator();
    ui.add_space(STATUS_ROW_DROP);
    // Top-aligned: the buttons' top edge lines up with the top of the status text.
    ui.horizontal_top(|ui| {
        let (text, color) = match (chain.status, chain.crashed_slot) {
            (ChainStatus::Running, _) => (t("● running").to_owned(), theme::GREEN),
            (ChainStatus::Starting, _) => (t("● starting").to_owned(), theme::TEXT_DIM),
            (ChainStatus::Degraded, Some(slot)) => (
                tf(
                    "● crashed in slot {n}: it is off, the rest bypassed",
                    &[("n", &(slot + 1))],
                ),
                theme::RED,
            ),
            (ChainStatus::Degraded, None) => {
                (t("● restarted, slots bypassed").to_owned(), theme::RED)
            }
            (ChainStatus::Stopped, _) => (t("● stopped").to_owned(), theme::RED),
        };
        let status =
            ui.add(egui::Label::new(RichText::new(text).size(11.0).color(color)).truncate());
        if let Some(error) = &chain.error {
            status.on_hover_text(error);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            // A few pixels below the status text's top, so they sit with it rather than on it.
            ui.vertical(|ui| {
                ui.add_space(STATUS_BUTTONS_DROP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let all_bypassed = !chain.slots.is_empty()
                        && chain
                            .slots
                            .iter()
                            .all(|s| s.get("bypass").and_then(|b| b.as_bool()) == Some(true));
                    if power_square(ui, vec2(22.0, 22.0), all_bypassed, theme::ORANGE)
                        .on_hover_text(t("bypass every slot, for an A/B"))
                        .clicked()
                    {
                        for index in 0..chain.slots.len() {
                            commands.push(message(
                                &chain.name,
                                &ToChain::Bypass {
                                    slot: index,
                                    bypass: !all_bypassed,
                                },
                            ));
                        }
                    }
                    let soloed = solo.as_ref().is_some_and(|s| s.chain == chain.name);
                    if square(ui, "S", soloed, theme::GREEN)
                        .on_hover_text(t("solo: mute every other chain"))
                        .clicked()
                    {
                        toggle_solo(graph, chain, solo, commands);
                    }
                    let mute =
                        square(ui, "M", chain.mute, theme::RED).on_hover_text(t("mute the output"));
                    if mute.clicked() {
                        commands.push(Command::SetChainMute {
                            name: chain.name.clone(),
                            mute: !chain.mute,
                        });
                    }
                    learn_menu(
                        mute,
                        ControlTarget::Mute {
                            chain: chain.name.clone(),
                        },
                        commands,
                    );
                });
            });
        });
    });
    // On its own line: next to the status, a narrow strip's M, S and B buttons covered it.
    if matches!(chain.status, ChainStatus::Degraded | ChainStatus::Stopped)
        && ui
            .small_button(t("Start normally"))
            .on_hover_text(t(
                "start the chain again with every slot loaded, the one that crashed too",
            ))
            .clicked()
    {
        commands.push(Command::RestartChain {
            name: chain.name.clone(),
        });
    }
    if let Some(error) = &chain.error {
        ui.label(RichText::new(error).size(11.0).color(theme::RED));
        // Killed with no crash report: the kernel's real-time limit, hit when the chain
        // cannot keep up, which efficiency cores make likely.
        if error.starts_with("killed")
            && let Some(confinement) = crate::cpu::confinement()
        {
            ui.label(theme::small_dim(t(
                "efficiency cores only; see Settings › Plugins",
            )))
            .on_hover_text(confinement.explain());
        }
    }
}

/// A small window: a frame with a title bar, centred in `rect`.
fn window_glyph(ui: &Ui, rect: Rect, color: Color32) {
    let painter = ui.painter();
    let frame = Rect::from_center_size(rect.center(), vec2(14.0, 11.0));
    painter.rect_stroke(frame, 1.5, Stroke::new(1.3, color), StrokeKind::Middle);
    painter.line_segment(
        [
            pos2(frame.left(), frame.top() + 3.5),
            pos2(frame.right(), frame.top() + 3.5),
        ],
        Stroke::new(1.3, color),
    );
}

/// The drag handle: four short vertical bars.
fn grip(ui: &mut Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
    let color = if response.hovered() {
        theme::TEXT_DIM
    } else {
        theme::BORDER
    };
    let stroke = Stroke::new(1.5, color);
    for i in 0..4 {
        let x = rect.left() + 1.5 + i as f32 * 3.0;
        ui.painter().line_segment(
            [pos2(x, rect.top() + 1.0), pos2(x, rect.bottom() - 1.0)],
            stroke,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::Grab)
}

/// A bypass button: the power glyph in a square, lit in `color` while bypassed.
fn power_square(ui: &mut Ui, size: Vec2, bypassed: bool, color: Color32) -> egui::Response {
    let (fill, stroke, ink) = if bypassed {
        (color.gamma_multiply(0.25), color, color)
    } else {
        (theme::PANEL, theme::BORDER, theme::TEXT_DIM)
    };
    let button = ui.add(
        Button::new("")
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke))
            .min_size(size),
    );
    let c = button.rect.center();
    let r = 5.0;
    let s = Stroke::new(1.4, ink);
    // A ring open at the top, and the stroke through the gap.
    let gap = 0.55;
    let points: Vec<egui::Pos2> = (0..=24)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2
                + gap
                + (std::f32::consts::TAU - 2.0 * gap) * i as f32 / 24.0;
            c + r * vec2(a.cos(), a.sin())
        })
        .collect();
    ui.painter().add(Shape::line(points, s));
    ui.painter()
        .line_segment([c + vec2(0.0, -r - 1.0), c + vec2(0.0, -1.0)], s);
    button
}

/// A console button: a letter in a small square, lit in `color` when on.
fn square(ui: &mut Ui, letter: &str, on: bool, color: Color32) -> egui::Response {
    let (fill, stroke, text) = if on {
        (color.gamma_multiply(0.25), color, color)
    } else {
        (theme::PANEL, theme::BORDER, theme::TEXT_DIM)
    };
    ui.add_sized(
        vec2(22.0, 22.0),
        Button::new(RichText::new(letter).size(11.0).color(text))
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke)),
    )
}

/// Exclusive solo: mutes every other chain that is not muted yet and unmutes this one;
/// a second click, or a solo on another chain, puts those back.
fn toggle_solo(
    graph: &Graph,
    chain: &ChainState,
    solo: &mut Option<Solo>,
    commands: &mut Vec<Command>,
) {
    if let Some(current) = solo.take() {
        for name in &current.muted {
            commands.push(Command::SetChainMute {
                name: name.clone(),
                mute: false,
            });
        }
        if current.chain == chain.name {
            return;
        }
    }
    let mut muted = Vec::new();
    for other in &graph.chains {
        if other.name != chain.name && !other.mute {
            commands.push(Command::SetChainMute {
                name: other.name.clone(),
                mute: true,
            });
            muted.push(other.name.clone());
        }
    }
    if chain.mute {
        commands.push(Command::SetChainMute {
            name: chain.name.clone(),
            mute: false,
        });
    }
    *solo = Some(Solo {
        chain: chain.name.clone(),
        muted,
    });
}

/// What the chain adds, in ms, with the per-slot breakdown on hover; orange on a mic
/// chain past `CALL_LATENCY_MS`.
fn latency_label(ui: &mut Ui, graph: &Graph, chain: &ChainState) {
    if chain.latency_samples == 0 {
        return;
    }
    let clock = graph.clock;
    let ms = clock.ms(f64::from(chain.latency_samples));
    let slow_call = chain.kind == ChainKind::Mic && ms > CALL_LATENCY_MS;
    let mut lines = vec![tf(
        "Adds {ms} ms: {smp} samples at {rate} Hz.",
        &[
            ("ms", &format!("{ms:.1}")),
            ("smp", &chain.latency_samples),
            ("rate", &clock.rate),
        ],
    )];
    let slots: Vec<SlotConfig> = chain
        .slots
        .iter()
        .filter_map(|v| v.clone().try_into().ok())
        .collect();
    for (i, smp) in chain.slot_latencies.iter().enumerate() {
        if *smp == 0 {
            continue;
        }
        let name = slots
            .get(i)
            .map_or_else(String::new, |s| slot_name(s, chain.slot_infos.get(&i)));
        lines.push(format!(
            "{} · {name}: {:.1} ms",
            i + 1,
            clock.ms(f64::from(*smp))
        ));
    }
    if slow_call {
        lines.push(
            t(
                "A call will feel delayed: a mic chain past 20 ms is heard as a lag. A lighter \
             denoiser or fewer look-ahead plugins bring it down.",
            )
            .to_owned(),
        );
    }
    let (text, color) = if slow_call {
        (format!("⚠ {ms:.0} ms"), theme::ORANGE)
    } else {
        (format!("{ms:.1} ms"), theme::TEXT_DIM)
    };
    ui.label(RichText::new(text).size(10.5).color(color))
        .on_hover_text(lines.join("\n"));
}

fn footer(
    ui: &mut Ui,
    graph: &Graph,
    chain: &ChainState,
    mic_check: &mut Option<MicCheck>,
    commands: &mut Vec<Command>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new(t("out")).size(11.0).color(theme::TEXT_DIM));
        let ms = graph.clock.ms(f64::from(chain.latency_samples));
        let latency = format!("{ms:.1} ms");
        let latency_width = 50.0;
        meter_horizontal(
            ui,
            smoothed(ui, (&chain.name, "out"), chain.meters.output()),
            ui.available_width() - latency_width - 8.0,
        );
        ui.label(RichText::new(latency).size(11.0).color(theme::TEXT_DIM))
            .on_hover_text(tf(
                "the slots' latency: {smp} samples at {rate} Hz",
                &[("smp", &chain.latency_samples), ("rate", &graph.clock.rate)],
            ));
    });
    record_row(ui, chain, commands);
    if chain.kind == ChainKind::Mic {
        mic_check_row(ui, chain, mic_check, commands);
    }
    ui.add_space(4.0);
    loudness(ui, chain, commands);
    spectrum_section(ui, chain, commands);
}

/// The spectrum's level range drawn, in dBFS.
const SPECTRUM_FLOOR_DB: f32 = -90.0;
const SPECTRUM_HEIGHT: f32 = 70.0;
/// How often an open spectrum asks the chain again (idempotent; survives a restart).
const SPECTRUM_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(2);
/// Seconds a band takes to rise and to fall most of the way to a new level: quick up,
/// slow down, as analyzers do, and smooth between the reports.
const SPECTRUM_RISE: f32 = 0.03;
const SPECTRUM_FALL: f32 = 0.25;

/// "Spectrum": input (dim) and output (accent) across 20 Hz–20 kHz, while it is open; the
/// chain analyzes only then.
fn spectrum_section(ui: &mut Ui, chain: &ChainState, commands: &mut Vec<Command>) {
    let id = ui.make_persistent_id(("spectrum", &chain.name));
    let state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let open = state.is_open();
    let asked_id = id.with("asked");
    let asked: Option<std::time::Instant> = ui.data(|d| d.get_temp(asked_id));
    let spectrum = |on| message(&chain.name, &ToChain::Spectrum { on });
    match (open, asked) {
        (true, None) => {
            commands.push(spectrum(true));
            ui.data_mut(|d| d.insert_temp(asked_id, std::time::Instant::now()));
        }
        (true, Some(at)) if at.elapsed() > SPECTRUM_KEEPALIVE => {
            commands.push(spectrum(true));
            ui.data_mut(|d| d.insert_temp(asked_id, std::time::Instant::now()));
        }
        (false, Some(_)) => {
            commands.push(spectrum(false));
            ui.data_mut(|d| d.remove::<std::time::Instant>(asked_id));
        }
        _ => {}
    }
    state
        .show_header(ui, |ui| {
            ui.label(
                RichText::new(t("Spectrum · in / out"))
                    .size(11.0)
                    .color(theme::TEXT_DIM),
            );
        })
        .body(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), SPECTRUM_HEIGHT), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, theme::CORNER_RADIUS, theme::BACKGROUND);
            // 100 Hz, 1 kHz, 10 kHz on the log scale from 20 Hz to 20 kHz.
            for hz in [100.0f32, 1000.0, 10_000.0] {
                let x = rect.left() + rect.width() * (hz / 20.0).log10() / 3.0;
                painter.vline(x, rect.y_range(), Stroke::new(1.0, theme::BORDER_SOFT));
            }
            let Some(bands) = glide_spectrum(ui, id, chain.meters.spectrum()) else {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    t("waiting for audio…"),
                    egui::FontId::proportional(10.5),
                    theme::TEXT_DIM,
                );
                return;
            };
            let half = bands.len() / 2;
            let line = |levels: &[f32]| -> Vec<egui::Pos2> {
                let last = levels.len().saturating_sub(1).max(1) as f32;
                levels
                    .iter()
                    .enumerate()
                    .map(|(i, db)| {
                        let t = ((db - SPECTRUM_FLOOR_DB) / -SPECTRUM_FLOOR_DB).clamp(0.0, 1.0);
                        pos2(
                            rect.left() + rect.width() * i as f32 / last,
                            rect.bottom() - rect.height() * t,
                        )
                    })
                    .collect()
            };
            painter.add(egui::Shape::line(
                line(&bands[..half]),
                Stroke::new(1.0, theme::TEXT_DIM.gamma_multiply(0.6)),
            ));
            painter.add(egui::Shape::line(
                line(&bands[half..]),
                Stroke::new(1.5, chain_color(chain)),
            ));
        });
}

/// The bands as drawn: each moves toward the latest report a little every frame.
fn glide_spectrum(ui: &Ui, id: egui::Id, latest: Option<Vec<f32>>) -> Option<Vec<f32>> {
    let latest = latest?;
    let id = id.with("glide");
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let mut shown: Vec<f32> = ui
        .data(|d| d.get_temp(id))
        .filter(|s: &Vec<f32>| s.len() == latest.len())
        .unwrap_or_else(|| latest.clone());
    for (shown, target) in shown.iter_mut().zip(&latest) {
        let tau = if target > shown {
            SPECTRUM_RISE
        } else {
            SPECTRUM_FALL
        };
        *shown += (target - *shown) * (1.0 - (-dt / tau).exp());
    }
    ui.data_mut(|d| d.insert_temp(id, shown.clone()));
    ui.ctx().request_repaint();
    Some(shown)
}

/// How long a mic check records before playing back.
const MIC_CHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// A mic check under way on one chain: record a few seconds, then hear them back.
pub(super) struct MicCheck {
    chain: String,
    path: std::path::PathBuf,
    phase: MicCheckPhase,
}

enum MicCheckPhase {
    Recording(std::time::Instant),
    /// Recording stopped; waiting for the chain to say the file is complete.
    Saving,
    Playing(std::time::Instant),
}

/// Where a mic check's take goes; the service deletes it once played.
fn mic_check_path() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("canalis").join("mic-check.wav"))
}

/// "Mic check": record the chain's output for a few seconds, then play it to the default
/// output: hear yourself without the live loop Listen can make.
fn mic_check_row(
    ui: &mut Ui,
    chain: &ChainState,
    mic_check: &mut Option<MicCheck>,
    commands: &mut Vec<Command>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        match mic_check.as_ref().filter(|c| c.chain == chain.name) {
            None => {
                let busy = mic_check.is_some() || chain.recording.as_ref().is_some_and(|r| !r.done);
                if ui
                    .add_enabled(
                        !busy,
                        Button::new(
                            RichText::new(t("🎤 Mic check"))
                                .size(11.0)
                                .color(theme::TEXT_DIM),
                        )
                        .frame(false),
                    )
                    .on_hover_text(t(
                        "records 5 seconds of you through this chain, then plays them back",
                    ))
                    .clicked()
                    && let Some(path) = mic_check_path()
                    && path
                        .parent()
                        .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
                {
                    commands.push(message(
                        &chain.name,
                        &ToChain::Record {
                            path: Some(path.display().to_string()),
                        },
                    ));
                    *mic_check = Some(MicCheck {
                        chain: chain.name.clone(),
                        path,
                        phase: MicCheckPhase::Recording(std::time::Instant::now()),
                    });
                }
            }
            Some(check) => {
                let text = match check.phase {
                    MicCheckPhase::Recording(since) => tf(
                        "● speak now… {n}",
                        &[(
                            "n",
                            &(MIC_CHECK.saturating_sub(since.elapsed()).as_secs() + 1),
                        )],
                    ),
                    MicCheckPhase::Saving => "…".to_owned(),
                    MicCheckPhase::Playing(_) => t("▶ playing it back").to_owned(),
                };
                ui.label(RichText::new(text).size(11.0).color(theme::ACCENT));
                if matches!(check.phase, MicCheckPhase::Playing(_))
                    && ui.small_button(t("stop")).clicked()
                {
                    commands.push(Command::StopPlayback);
                }
            }
        }
    });
}

/// ● records the chain's output to a WAV file, ■ stops; then where it went.
fn record_row(ui: &mut Ui, chain: &ChainState, commands: &mut Vec<Command>) {
    let running = chain.recording.as_ref().filter(|r| !r.done);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (face, hover) = match running {
            Some(_) => (t("■ Stop"), t("stop recording; the file is finished")),
            None => (
                t("● Record"),
                t("record what this chain outputs to a WAV file in your Music folder"),
            ),
        };
        let button = Button::new(RichText::new(face).size(11.0).color(if running.is_some() {
            theme::RED_SOFT
        } else {
            theme::TEXT_DIM
        }))
        .frame(false);
        if ui.add(button).on_hover_text(hover).clicked() {
            let path = match running {
                Some(_) => None,
                None => recording_path(&chain.label).map(|p| p.display().to_string()),
            };
            commands.push(message(&chain.name, &ToChain::Record { path }));
        }
        match &chain.recording {
            Some(r) if !r.done => {
                let minutes = r.seconds as u32 / 60;
                let seconds = r.seconds as u32 % 60;
                ui.label(
                    RichText::new(format!("{minutes}:{seconds:02}"))
                        .size(11.0)
                        .color(theme::RED_SOFT),
                );
            }
            Some(r) if Some(std::path::PathBuf::from(&r.path)) == mic_check_path() => {}
            Some(r) => {
                let text = match &r.error {
                    Some(e) => RichText::new(tf("not saved: {error}", &[("error", e)]))
                        .color(theme::RED_SOFT),
                    None => {
                        let path = tilde(&r.path);
                        theme::small_dim(if r.dropped > 0 {
                            tf(
                                "saved {path} (some audio lost: the disk was slow)",
                                &[("path", &path)],
                            )
                        } else {
                            tf("saved {path}", &[("path", &path)])
                        })
                    }
                };
                ui.add(egui::Label::new(text.size(10.5)).truncate())
                    .on_hover_text(&r.path);
                if r.error.is_none() && ui.small_button(t("open folder")).clicked() {
                    open_folder(std::path::Path::new(&r.path).parent());
                }
            }
            None => {}
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            compare_controls(ui, chain, commands);
        });
    });
}

/// A/B: start a comparison (B is a copy of now), switch between the two by ear, keep the
/// one playing.
fn compare_controls(ui: &mut Ui, chain: &ChainState, commands: &mut Vec<Command>) {
    let step = |step| Command::Compare {
        name: chain.name.clone(),
        step,
    };
    let Some(playing) = chain.compare else {
        if ui
            .add(Button::new(RichText::new("A/B").size(11.0).color(theme::TEXT_DIM)).frame(false))
            .on_hover_text(t(
                "compare two versions of this chain: B starts as a copy of what plays now; \
                 change one, switch between them by ear, keep the better",
            ))
            .clicked()
        {
            commands.push(step(CompareStep::Start));
        }
        return;
    };
    if ui
        .small_button(t("keep"))
        .on_hover_text(t("keep the version playing and end the comparison"))
        .clicked()
    {
        commands.push(step(CompareStep::Keep));
    }
    // Right to left: B then A, so they read A B.
    for (side, name) in [(AbSide::B, "B"), (AbSide::A, "A")] {
        let chosen = playing == side;
        if ui
            .selectable_label(chosen, RichText::new(name).size(11.0).strong())
            .on_hover_text(if chosen {
                t("playing; changes go to this version")
            } else {
                t("switch to this version (the chain reloads it)")
            })
            .clicked()
            && !chosen
        {
            commands.push(step(CompareStep::Switch));
        }
    }
}

/// `~/Music/Canalis/<label> <n>.wav`, the first number not taken; the folder is made.
fn recording_path(label: &str) -> Option<std::path::PathBuf> {
    let dir = dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))?
        .join("Canalis");
    std::fs::create_dir_all(&dir).ok()?;
    let stem: String = label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    (1..)
        .map(|n| dir.join(format!("{stem} {n}.wav")))
        .find(|p| !p.exists())
}

fn tilde(path: &str) -> String {
    match dirs::home_dir().and_then(|h| path.strip_prefix(h.to_str()?).map(str::to_owned)) {
        Some(rest) => format!("~{rest}"),
        None => path.to_owned(),
    }
}

fn open_folder(dir: Option<&std::path::Path>) {
    let Some(dir) = dir else {
        return;
    };
    match std::process::Command::new("xdg-open").arg(dir).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => log::warn!("xdg-open {}: {e}", dir.display()),
    }
}

/// "auto": the chain steers its output toward the target by itself; while on, the gain
/// it applies beside it.
fn auto_level_toggle(ui: &mut Ui, chain: &ChainState, commands: &mut Vec<Command>) {
    let on = chain.auto_level;
    let face = if on {
        let gain = chain.meters.auto_gain_db();
        tf("auto {gain} dB", &[("gain", &format!("{gain:+.1}"))])
    } else {
        t("auto").to_owned()
    };
    let color = if on {
        theme::ACCENT
    } else {
        theme::TEXT_DIM.gamma_multiply(0.7)
    };
    if ui
        .add(Button::new(RichText::new(face).size(10.0).color(color)).frame(false))
        .on_hover_text(if on {
            t("automatic level on: the chain raises quiet sources and lowers loud ones toward \
               the target, slowly (up to +12 dB, down to −24 dB); click to turn it off")
        } else {
            t("turn on automatic level: the chain evens out quiet and loud sources toward the target")
        })
        .clicked()
    {
        commands.push(Command::SetAutoLevel {
            name: chain.name.clone(),
            on: !on,
        });
    }
}

/// Time constant of the loudness bar's glide between 100 ms readings, in seconds.
const LOUDNESS_GLIDE_S: f32 = 0.08;
/// The scale the loudness bar spans, in LUFS.
const LOUDNESS_SCALE: std::ops::RangeInclusive<f32> = -40.0..=0.0;

/// Loudness after the mute: a scale with the target band, then the four readings.
fn loudness(ui: &mut Ui, chain: &ChainState, commands: &mut Vec<Command>) {
    let [momentary, short_term, integrated, true_peak] = chain.meters.loudness();
    let silent = |v: f32| v <= canalis_pw::LOUDNESS_SILENCE + 1.0;
    let target = chain.loudness_target.unwrap_or(canalis_pw::LOUDNESS_TARGET);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(t("Loudness · LUFS"))
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
        // As quiet as the label it replaces: a menu only on a closer look.
        let button = Button::new(
            RichText::new(tf(
                "target {target} ▾",
                &[("target", &format!("{target:.0}"))],
            ))
            .size(10.0)
            .color(theme::TEXT_DIM.gamma_multiply(0.7)),
        )
        .frame(false);
        let (response, _) = MenuButton::from_button(button).ui(ui, |ui| {
            for (name, value) in loudness_targets() {
                let chosen = target == value;
                let text = format!("{value:.0} LUFS · {name}");
                if ui.selectable_label(chosen, text).clicked() {
                    let target = (value != canalis_pw::LOUDNESS_TARGET).then_some(value);
                    commands.push(Command::SetLoudnessTarget {
                        name: chain.name.clone(),
                        target,
                    });
                    ui.close();
                }
            }
        });
        response.on_hover_text(t(
            "the integrated loudness to aim at; turns green within 1 LU",
        ));
        auto_level_toggle(ui, chain, commands);
    });

    // The values change every 100 ms block; the bar glides between them at frame rate.
    let glide_id = ui.make_persistent_id(("loudness-glide", &chain.name));
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let k = 1.0 - (-dt / LOUDNESS_GLIDE_S).exp();
    let shown: Option<[f32; 2]> = ui.data(|d| d.get_temp(glide_id));
    let glide = |target: f32, from: Option<f32>| match from {
        Some(prev) if !silent(target) && !silent(prev) => prev + (target - prev) * k,
        _ => target,
    };
    let bar_momentary = glide(momentary, shown.map(|s| s[0]));
    let bar_short = glide(short_term, shown.map(|s| s[1]));
    ui.data_mut(|d| d.insert_temp(glide_id, [bar_momentary, bar_short]));

    // The scale: target band, momentary fill, a tick for short-term, a marker for integrated.
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
    let x = |v: f32| {
        let t = (v - LOUDNESS_SCALE.start()) / (LOUDNESS_SCALE.end() - LOUDNESS_SCALE.start());
        rect.left() + t.clamp(0.0, 1.0) * rect.width()
    };
    let track = Rect::from_min_max(
        pos2(rect.left(), rect.bottom() - 5.0),
        pos2(rect.right(), rect.bottom() - 1.0),
    );
    let painter = ui.painter();
    painter.rect_filled(track, 2.0, theme::BORDER_SOFT);
    let band = Rect::from_x_y_ranges(x(target - 1.0)..=x(target + 1.0), track.y_range());
    painter.rect_filled(band, 0.0, theme::GREEN.gamma_multiply(0.35));
    if !silent(bar_momentary) {
        let fill = Rect::from_min_max(track.min, pos2(x(bar_momentary), track.max.y));
        painter.rect_filled(fill, 2.0, theme::ACCENT.gamma_multiply(0.8));
    }
    if !silent(bar_short) {
        let sx = x(bar_short);
        painter.line_segment(
            [pos2(sx, track.top() - 2.0), pos2(sx, track.bottom() + 1.0)],
            Stroke::new(1.5, theme::TEXT),
        );
    }
    let integrated_color = if silent(integrated) {
        theme::TEXT
    } else if (integrated - target).abs() <= 1.0 {
        // Within 1 LU of the target still reads as on target.
        theme::GREEN
    } else if integrated > target {
        theme::ORANGE
    } else {
        theme::TEXT
    };
    if !silent(integrated) {
        let ix = x(integrated);
        painter.add(Shape::convex_polygon(
            vec![
                pos2(ix - 3.5, rect.top()),
                pos2(ix + 3.5, rect.top()),
                pos2(ix, track.top() - 1.0),
            ],
            integrated_color,
            Stroke::NONE,
        ));
    }

    // The readings, one tile each.
    let tiles = [
        (
            t("Momentary"),
            momentary,
            theme::TEXT,
            t("LUFS over the last 400 ms; click to reset"),
        ),
        (
            t("Short"),
            short_term,
            theme::TEXT,
            t("LUFS over the last 3 s; click to reset"),
        ),
        (
            t("Integrated"),
            integrated,
            integrated_color,
            t("LUFS since the reset, gated as EBU R128; click to reset"),
        ),
        (
            t("True peak"),
            true_peak,
            if true_peak > -1.0 {
                theme::RED
            } else {
                theme::TEXT
            },
            t("the highest true peak since the reset, in dBTP; click to reset"),
        ),
    ];
    let gap = 4.0;
    let width = (ui.available_width() - gap * (tiles.len() as f32 - 1.0)) / tiles.len() as f32;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (caption, v, color, hint) in tiles {
            let (rect, response) = ui.allocate_exact_size(vec2(width, 40.0), Sense::click());
            let painter = ui.painter();
            painter.rect(
                rect,
                theme::CORNER_RADIUS,
                theme::BACKGROUND,
                Stroke::new(
                    1.0,
                    if response.hovered() {
                        theme::BORDER
                    } else {
                        theme::BORDER_SOFT
                    },
                ),
                StrokeKind::Inside,
            );
            painter.text(
                rect.left_top() + vec2(6.0, 4.0),
                egui::Align2::LEFT_TOP,
                caption,
                egui::FontId::proportional(9.5),
                theme::TEXT_DIM,
            );
            let reading = if silent(v) {
                "–".to_owned()
            } else {
                format!("{v:.1}")
            };
            painter.text(
                rect.left_bottom() + vec2(6.0, -4.0),
                egui::Align2::LEFT_BOTTOM,
                reading,
                egui::FontId::monospace(13.0),
                color,
            );
            // Like a console's loudness meter: a click on a reading starts it over.
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(hint)
                .clicked()
            {
                commands.push(message(&chain.name, &ToChain::ResetLoudness));
            }
        }
    });
    balance_meter(ui, chain, silent(momentary));
}

/// Where the output sits between left and right over 400 ms: a fill from the centre
/// mark towards the louder side and the reading at the end, dimmed in silence.
fn balance_meter(ui: &mut Ui, chain: &ChainState, silent: bool) {
    let target = if silent { 0.0 } else { chain.meters.balance() };
    let glide_id = ui.make_persistent_id(("balance-glide", &chain.name));
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let k = 1.0 - (-dt / LOUDNESS_GLIDE_S).exp();
    let shown = ui.data(|d| d.get_temp::<f32>(glide_id)).unwrap_or(0.0);
    let balance = shown + (target - shown) * k;
    ui.data_mut(|d| d.insert_temp(glide_id, balance));

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let ink = if silent {
            theme::TEXT_DIM.gamma_multiply(0.6)
        } else {
            theme::TEXT_DIM
        };
        ui.label(RichText::new("L").size(10.0).color(ink));
        let reading_width = 30.0;
        let width = ui.available_width() - reading_width - 2.0 * 6.0 - 8.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, 12.0), Sense::hover());
        let painter = ui.painter();
        let track = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
        painter.rect_filled(track, 2.0, theme::BORDER_SOFT);
        let centre = track.center().x;
        let end = centre + balance.clamp(-1.0, 1.0) * track.width() / 2.0;
        if !silent {
            let fill = Rect::from_x_y_ranges(centre.min(end)..=centre.max(end), track.y_range());
            painter.rect_filled(fill, 2.0, theme::ACCENT.gamma_multiply(0.8));
            painter.circle_filled(pos2(end, track.center().y), 3.0, theme::ACCENT);
        }
        painter.line_segment(
            [pos2(centre, rect.top()), pos2(centre, rect.bottom())],
            Stroke::new(1.0, theme::TEXT_DIM),
        );
        response.on_hover_text(t("balance of the output over the last 400 ms, by loudness"));
        ui.label(RichText::new("R").size(10.0).color(ink));
        ui.add_sized(
            vec2(reading_width, 12.0),
            egui::Label::new(
                RichText::new(if silent {
                    "–".to_owned()
                } else {
                    pan_label(balance)
                })
                .monospace()
                .size(11.0)
                .color(if silent { ink } else { theme::TEXT }),
            ),
        );
    });
}

fn section_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(theme::TEXT_DIM));
}

/// A small rounded tag with its own colours.
fn chip(ui: &mut Ui, text: &str, bg: Color32, fg: Color32) -> egui::Response {
    Frame::new()
        .fill(bg)
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| ui.label(RichText::new(text).size(11.0).color(fg)))
        .response
}

/// The dashed box that stands where apps connect instead of picked nodes: the apps on
/// `device` right now, with their levels, or `text` while there are none; `extra` goes
/// after.
fn apps_end(
    ui: &mut Ui,
    graph: &Graph,
    device: &str,
    text: &str,
    commands: &mut Vec<Command>,
    extra: impl FnOnce(&mut Ui, &mut Vec<Command>),
) {
    let streams = app_streams(graph, device);
    let response = Frame::new().inner_margin(Margin::same(5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        Frame::new()
            .inner_margin(Margin::symmetric(8, 3))
            .show(ui, |ui| {
                if streams.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(text).size(12.0).color(theme::TEXT_DIM));
                        extra(ui, commands);
                    });
                } else {
                    app_chips(ui, graph, &streams, CHAIN_APP_CHIPS, commands);
                    ui.horizontal(|ui| extra(ui, commands));
                }
            })
            .response
    });
    dashed_rect(ui, response.response.rect, theme::BORDER);
    if streams.is_empty() {
        dashed_rect(ui, response.inner.rect, theme::BORDER);
    }
}

fn dashed_rect(ui: &Ui, rect: Rect, color: Color32) {
    let stroke = Stroke::new(1.0, color);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    ui.painter()
        .extend(Shape::dashed_line(&corners, stroke, DASH.0, DASH.1));
}

/// The chip for the chain's key source, with a picker: sources (a mic, a mic chain's
/// virtual source) and apps' playback streams.
fn key_box(ui: &mut Ui, graph: &Graph, chain: &ChainState, commands: &mut Vec<Command>) {
    let mut next = chain.key.clone();
    Frame::new()
        .fill(theme::BACKGROUND)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(Margin::same(5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            ui.horizontal_wrapped(|ui| {
                if let Some(name) = &chain.key {
                    let node = graph.node_by_name(name);
                    let label = node.map_or(name.as_str(), |n| n.short_label.as_str());
                    let text = if node.is_none() {
                        tf("{label} (absent)", &[("label", &label)])
                    } else {
                        label.to_owned()
                    };
                    let response = Frame::new()
                        .fill(theme::BORDER_SOFT)
                        .corner_radius(theme::CORNER_RADIUS)
                        .inner_margin(Margin {
                            left: 8,
                            right: 4,
                            top: 3,
                            bottom: 3,
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(RichText::new(text).size(12.0).color(theme::TEXT));
                                ui.add(
                                    Button::new(
                                        RichText::new("×").size(12.0).color(theme::TEXT_DIM),
                                    )
                                    .frame(false),
                                )
                                .on_hover_text(t("no key"))
                                .clicked()
                            })
                            .inner
                        });
                    if response.inner {
                        next = None;
                    }
                    response.response.on_hover_text(name);
                }
                let own_ends = [
                    chain.kind.capture_node_name(&chain.name),
                    chain.kind.playback_node_name(&chain.name),
                    chain.kind.key_node_name(&chain.name),
                    chain.name.clone(),
                ];
                let candidates: Vec<&Node> = graph
                    .nodes
                    .iter()
                    .filter(|n| {
                        !own_ends.contains(&n.name) && chain.key.as_deref() != Some(&n.name)
                    })
                    .filter(|n| {
                        n.class == MediaClass::Source
                            || (n.class == MediaClass::PlaybackStream && !n.is_virtual_device())
                    })
                    .collect();
                let plus = Button::new("").frame(false).min_size(vec2(24.0, 22.0));
                let (add, _) = MenuButton::from_button(plus).ui(ui, |ui| {
                    if candidates.is_empty() {
                        ui.label(theme::small_dim(t("nothing to key from")));
                    }
                    for node in candidates {
                        let label = match &node.app {
                            Some(app) => format!("{} · {app}", node.label),
                            None => node.label.clone(),
                        };
                        if ui.selectable_label(false, label).clicked() {
                            next = Some(node.name.clone());
                            ui.close();
                        }
                    }
                });
                plus_face(ui, &add);
            });
        });
    if next != chain.key {
        commands.push(Command::SetChainKey {
            name: chain.name.clone(),
            key: next,
        });
    }
}

/// Chips for the nodes a chain end is linked to, in a box, with a picker for more.
fn endpoints(
    ui: &mut Ui,
    graph: &Graph,
    configs: &[ChainConfig],
    chain: &ChainState,
    direction: Direction,
    feedback: &mut Option<FeedbackRisk>,
    commands: &mut Vec<Command>,
) {
    let current = match direction {
        Direction::Input => &chain.inputs,
        Direction::Output => &chain.outputs,
    };
    let mut next = current.clone();
    Frame::new()
        .fill(theme::BACKGROUND)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(Margin::same(5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            // One end per row, the add button last: a list reads better than a wrap.
            ui.vertical(|ui| {
                for name in current {
                    let node = graph.node_by_name(name);
                    let label = node.map_or(name.as_str(), |n| n.short_label.as_str());
                    let text = if node.is_none() {
                        tf("{label} (absent)", &[("label", &label)])
                    } else {
                        label.to_owned()
                    };
                    let (bg, fg) = if node.is_some_and(Node::is_virtual_device) {
                        (
                            Color32::from_rgb(0x1b, 0x2f, 0x3d),
                            Color32::from_rgb(0x8f, 0xd4, 0xf5),
                        )
                    } else {
                        (theme::BORDER_SOFT, theme::TEXT)
                    };
                    let response = Frame::new()
                        .fill(bg)
                        .corner_radius(theme::CORNER_RADIUS)
                        .inner_margin(Margin {
                            left: 8,
                            right: 4,
                            top: 3,
                            bottom: 3,
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            // Room for the gain and the ×; a long device name truncates
                            // instead of widening the strip.
                            let room = if direction == Direction::Output {
                                96.0
                            } else {
                                40.0
                            };
                            let max = (ui.available_width() - room).max(40.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.scope(|ui| {
                                    ui.set_max_width(max);
                                    ui.add(
                                        egui::Label::new(RichText::new(text).size(12.0).color(fg))
                                            .truncate(),
                                    );
                                });
                                // Gain and × at the row's end, lined up down the list.
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let unlink = ui
                                            .add(
                                                Button::new(
                                                    RichText::new("×")
                                                        .size(12.0)
                                                        .color(theme::TEXT_DIM),
                                                )
                                                .frame(false),
                                            )
                                            .on_hover_text(t("unlink"))
                                            .clicked();
                                        if direction == Direction::Output {
                                            gain_drag(ui, chain, name, commands);
                                        }
                                        unlink
                                    },
                                )
                                .inner
                            })
                            .inner
                        });
                    if response.inner {
                        next.retain(|n| n != name);
                    }
                    response.response.on_hover_text(name);
                }
                let own_ends = [
                    chain.kind.capture_node_name(&chain.name),
                    chain.kind.playback_node_name(&chain.name),
                    chain.name.clone(),
                ];
                let config = chain.config();
                let candidates: Vec<&Node> = graph
                    .nodes
                    .iter()
                    .filter(|n| !own_ends.contains(&n.name) && !current.contains(&n.name))
                    .filter(|n| match direction {
                        Direction::Output => {
                            n.class == MediaClass::Sink
                                || (n.class == MediaClass::CaptureStream && n.is_virtual_device())
                        }
                        Direction::Input => {
                            n.class == MediaClass::Source
                                || (n.class == MediaClass::PlaybackStream && n.is_virtual_device())
                        }
                    })
                    .filter(|n| match direction {
                        Direction::Output => !would_cycle(configs, &config, &n.name),
                        Direction::Input => {
                            config.inputs.is_empty() || !would_cycle(configs, &config, &n.name)
                        }
                    })
                    .collect();
                let plus = Button::new("").frame(false).min_size(vec2(24.0, 22.0));
                let (add, _) = MenuButton::from_button(plus).ui(ui, |ui| {
                    if candidates.is_empty() {
                        ui.label(theme::small_dim(t("nothing else to connect")));
                    }
                    for node in candidates {
                        if ui.selectable_label(false, &node.label).clicked() {
                            next.push(node.name.clone());
                            ui.close();
                        }
                    }
                });
                plus_face(ui, &add);
            });
        });
    if next != *current {
        let (inputs, outputs) = match direction {
            Direction::Input => (next, chain.outputs.clone()),
            Direction::Output => (chain.inputs.clone(), next),
        };
        let config = chain.config();
        let added = feedback::new_speakers(graph, configs, &config, &inputs, &outputs);
        if !added.is_empty() {
            *feedback = Some(FeedbackRisk {
                chain: chain.name.clone(),
                speakers: added
                    .iter()
                    .filter_map(|n| graph.node_by_name(n))
                    .map(|n| n.label.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
                silenced: added,
                inputs,
                outputs,
            });
        } else {
            commands.push(Command::SetChainEndpoints {
                name: chain.name.clone(),
                inputs,
                outputs,
            });
        }
    }
}

/// A right-click on a control offers to learn a MIDI message for it.
fn learn_menu(response: egui::Response, target: ControlTarget, commands: &mut Vec<Command>) {
    response.context_menu(|ui| {
        if ui
            .button(t("Learn MIDI control"))
            .on_hover_text(t(
                "then move a knob or press a key on the MIDI input picked in Devices",
            ))
            .clicked()
        {
            commands.push(Command::Learn {
                target: Some(target.clone()),
            });
            ui.close();
        }
    });
}

/// The send gain of one destination, in dB; released at the slider floor it is off.
fn gain_drag(ui: &mut Ui, chain: &ChainState, target: &str, commands: &mut Vec<Command>) {
    let saved = chain.gains.get(target).copied().unwrap_or(0.0);
    let id = ui.make_persistent_id(("gain", &chain.name, target));
    let mut value: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(saved);
    let response = ui.add(
        DragValue::new(&mut value)
            .range(GAIN_OFF_DB..=12.0)
            .speed(0.1)
            .fixed_decimals(1)
            .suffix(" dB")
            .custom_formatter(|v, _| {
                if v <= f64::from(GAIN_OFF_DB) {
                    t("off").to_owned()
                } else {
                    format!("{v:+.1}")
                }
            }),
    );
    let response = response.on_hover_text(t("send level; drag, or double-click to type"));
    learn_menu(
        response.clone(),
        ControlTarget::Gain {
            chain: chain.name.clone(),
            target: target.to_owned(),
        },
        commands,
    );
    if response.dragged() || response.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, value));
    } else {
        ui.data_mut(|d| d.remove_temp::<f32>(id));
    }
    if commit(response) && (value - saved).abs() > 0.01 {
        commands.push(Command::SetChainGain {
            name: chain.name.clone(),
            target: target.to_owned(),
            gain_db: value,
        });
    }
}

/// The header of a folder of slots: a chevron that folds it, the name (double-click to
/// rename), how many slots, and a menu to dissolve it. Returns whether it is open.
#[allow(clippy::too_many_arguments)]
fn folder_header(
    ui: &mut Ui,
    chain: &ChainState,
    folder: &str,
    count: usize,
    slots: &mut [SlotConfig],
    start: usize,
    end: usize,
    actions: &mut CardActions,
    state: &mut CollapsingState,
) {
    let open = state.is_open();
    let rename_id = state.id().with("rename");
    let mut renaming: Option<String> = ui.data(|d| d.get_temp(rename_id));
    let response = Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let chevron = if open { "▾" } else { "▸" };
                if ui
                    .add(Button::new(RichText::new(chevron).size(12.0)).frame(false))
                    .clicked()
                {
                    state.toggle(ui);
                }
                if let Some(text) = renaming.as_mut() {
                    let edit = ui.add(egui::TextEdit::singleline(text).desired_width(120.0));
                    autofocus(ui, &edit);
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if enter || (edit.lost_focus() && !escape) {
                        let name = text.trim().to_owned();
                        if !name.is_empty() && name != folder {
                            for slot in &mut slots[start..end] {
                                slot.folder = Some(name.clone());
                            }
                            actions.rewrite = true;
                        }
                        renaming = None;
                        ui.data_mut(|d| d.remove_temp::<String>(rename_id));
                    } else if escape {
                        renaming = None;
                        ui.data_mut(|d| d.remove_temp::<String>(rename_id));
                    } else {
                        ui.data_mut(|d| d.insert_temp(rename_id, text.clone()));
                    }
                } else {
                    let label = ui
                        .add(
                            egui::Label::new(RichText::new(folder).size(12.0).strong())
                                .sense(Sense::click()),
                        )
                        .on_hover_text(t("double-click to rename; right-click for more"));
                    if label.double_clicked() {
                        ui.data_mut(|d| d.insert_temp(rename_id, folder.to_owned()));
                    }
                    label.context_menu(|ui| {
                        if ui.button(t("Rename…")).clicked() {
                            ui.data_mut(|d| d.insert_temp(rename_id, folder.to_owned()));
                            ui.close();
                        }
                        if ui.button(t("Dissolve folder")).clicked() {
                            for slot in &mut slots[start..end] {
                                slot.folder = None;
                            }
                            actions.rewrite = true;
                            ui.close();
                        }
                    });
                }
                ui.label(theme::small_dim(if count == 1 {
                    tf("{n} slot", &[("n", &count)])
                } else {
                    tf("{n} slots", &[("n", &count)])
                }));
                if !open {
                    let names: Vec<String> = (start..end)
                        .map(|i| slot_name(&slots[i], chain.slot_infos.get(&i)))
                        .collect();
                    ui.label(theme::small_dim(format!("· {}", names.join(" → "))));
                }
            });
        });
    if response.response.double_clicked() {
        state.toggle(ui);
    }
    let header = response.response;
    // Top half: before the folder, outside it; bottom half: into it, at its end.
    let before = |ui: &Ui| {
        ui.ctx()
            .pointer_latest_pos()
            .is_some_and(|p| p.y < header.rect.center().y)
    };
    if let Some(dragged) = header.dnd_hover_payload::<DragSlot>()
        && dragged.chain == chain.name
    {
        if before(ui) {
            drop_line(ui, header.rect, false);
        } else {
            ui.painter().rect_stroke(
                header.rect,
                theme::CORNER_RADIUS,
                Stroke::new(2.0, theme::ACCENT),
                StrokeKind::Outside,
            );
        }
    }
    if let Some(dragged) = header.dnd_release_payload::<DragSlot>()
        && dragged.chain == chain.name
    {
        if before(ui) {
            actions.before_folder = Some((dragged.index, start));
        } else {
            actions.into_folder = Some((dragged.index, folder.to_owned()));
        }
    }
}

/// The accent line where a dragged slot will land: under `rect` or over it.
fn drop_line(ui: &Ui, rect: Rect, below: bool) {
    let y = if below {
        rect.bottom() + 3.0
    } else {
        rect.top() - 3.0
    };
    ui.painter().line_segment(
        [pos2(rect.left(), y), pos2(rect.right(), y)],
        Stroke::new(2.0, theme::ACCENT),
    );
}

/// What the cards asked for, applied to the slot list after they are drawn.
#[derive(Default)]
struct CardActions {
    rewrite: bool,
    /// A slot id to drop.
    remove: Option<usize>,
    /// A slot (by id) dropped onto another (by id), and whether in its lower half: same
    /// list, it lands before or after that slot and takes its folder.
    moved: Option<(usize, usize, bool)>,
    /// A slot (by id) dropped on a folder's header: it joins the folder at its end.
    into_folder: Option<(usize, String)>,
    /// A slot (by id) dropped on the top of a folder's header (starting at that index):
    /// it lands just before the folder, outside it.
    before_folder: Option<(usize, usize)>,
    /// A slot (by id) dropped on a lane's box, not on a card in it: the end of that lane
    /// (the group's top-level index, the lane).
    into_lane: Option<(usize, usize, usize)>,
    /// A drop target under the pointer already drew its marker this frame.
    hover_taken: bool,
}

/// One slot card; `index` is the slot id, a lane's when it is inside a group.
#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    graph: &Graph,
    chain: &ChainState,
    index: usize,
    key: u64,
    slot: &mut SlotConfig,
    picker: &mut Option<Picker>,
    commands: &mut Vec<Command>,
    actions: &mut CardActions,
    folders: &[String],
) {
    // Folders exist at the top level only.
    let top_level = matches!(SlotPath::from_id(index), SlotPath::Top(_));
    let info: Option<&SlotInfo> = chain.slot_infos.get(&index);
    let crashed = chain.crashed_slot == Some(index);
    let failed = info.is_some_and(|i| i.error.is_some());
    let alarmed = slot.bypass || crashed || failed;
    // Senses clicks under the card's own widgets, so a right-click anywhere opens its menu.
    let card = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        Frame::new()
            .fill(theme::BACKGROUND)
            .stroke(Stroke::new(
                1.0,
                if alarmed {
                    theme::RED
                } else {
                    theme::BORDER_SOFT
                },
            ))
            .corner_radius(theme::CORNER_RADIUS)
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing = vec2(8.0, 2.0);
                let mut lanes_state = matches!(slot.spec, SlotSpec::Group { .. }).then(|| {
                    let id = egui::Id::unique((&chain.name, key, "lanes"));
                    CollapsingState::load_with_default_open(ui.ctx(), id, true)
                });
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - 2.0 * SQUARE_BUTTON.x - 16.0);
                        let name = slot_name(slot, info);
                        let name_color = if slot.bypass {
                            theme::TEXT_DIM
                        } else {
                            theme::TEXT
                        };
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if let Some(lanes) = lanes_state.as_mut() {
                                let chevron = if lanes.is_open() { "▾" } else { "▸" };
                                if ui
                                    .add(
                                        Button::new(RichText::new(chevron).size(12.0)).frame(false),
                                    )
                                    .on_hover_text(t("show or hide the lanes"))
                                    .clicked()
                                {
                                    lanes.toggle(ui);
                                }
                            }
                            ui.dnd_drag_source(
                                ui.make_persistent_id(("drag-slot", &chain.name, index)),
                                DragSlot {
                                    chain: chain.name.clone(),
                                    index,
                                },
                                |ui| {
                                    grip(ui).on_hover_text(t("drag to reorder, or into a folder"));
                                },
                            );
                            let rename_id = egui::Id::unique((&chain.name, key, "rename"));
                            let renaming: Option<String> = ui.data(|d| d.get_temp(rename_id));
                            if let Some(mut text) = renaming {
                                let edit = ui.add(
                                    egui::TextEdit::singleline(&mut text)
                                        .desired_width(ui.available_width()),
                                );
                                if autofocus(ui, &edit) {
                                    // Selected on open, so typing replaces the old name.
                                    let mut state =
                                        egui::text_edit::TextEditState::load(ui.ctx(), edit.id)
                                            .unwrap_or_default();
                                    state.cursor.set_char_range(Some(
                                        egui::text::CCursorRange::select_all_str(&text),
                                    ));
                                    state.store(ui.ctx(), edit.id);
                                }
                                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                if enter || (edit.lost_focus() && !escape) {
                                    let text = text.trim();
                                    let label = (!text.is_empty()
                                        && text != default_name(slot, info))
                                    .then(|| text.to_owned());
                                    if label != slot.label {
                                        slot.label = label;
                                        actions.rewrite = true;
                                    }
                                    ui.data_mut(|d| d.remove_temp::<String>(rename_id));
                                } else if escape {
                                    ui.data_mut(|d| d.remove_temp::<String>(rename_id));
                                } else {
                                    ui.data_mut(|d| d.insert_temp(rename_id, text));
                                }
                            } else {
                                let label = ui
                                    .add(
                                        egui::Label::new(RichText::new(&name).color(name_color))
                                            .selectable(false)
                                            .truncate()
                                            .sense(Sense::click()),
                                    )
                                    .on_hover_text(match &slot.label {
                                        Some(_) => tf(
                                            "{name}: double-click to rename, empty for the plugin's name",
                                            &[("name", &default_name(slot, info))],
                                        ),
                                        None => t("double-click to rename; right-click the card for more")
                                            .to_owned(),
                                    });
                                if label.double_clicked() {
                                    ui.data_mut(|d| d.insert_temp(rename_id, name.clone()));
                                }
                            }
                        });
                        let overload = chain.overloads.get(&index).map(String::as_str);
                        let (subtitle, color) = slot_subtitle(slot, info, crashed, overload);
                        let subtitle = ui.label(RichText::new(subtitle).size(11.0).color(color));
                        if slot.bypass
                            && let Some(times) = overload
                        {
                            let mut why = tf(
                                "The chain kept overrunning its cycle, so it bypassed this \
                                 slot, the slowest (time per cycle: {times}). Turn the bypass \
                                 off to try again.",
                                &[("times", &times)],
                            );
                            if let Some(confinement) = crate::cpu::confinement() {
                                why = format!("{why}\n\n{}", confinement.explain());
                            }
                            subtitle.on_hover_text(why);
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let bypass = power_square(ui, SQUARE_BUTTON, slot.bypass, theme::RED)
                            .on_hover_text(t("bypass"));
                        if bypass.clicked() {
                            slot.bypass = !slot.bypass;
                            commands.push(message(
                                &chain.name,
                                &ToChain::Bypass {
                                    slot: index,
                                    bypass: slot.bypass,
                                },
                            ));
                        }
                        learn_menu(
                            bypass,
                            ControlTarget::Bypass {
                                chain: chain.name.clone(),
                                slot: index,
                            },
                            commands,
                        );
                        let has_gui = info.is_some_and(|i| i.has_gui);
                        let open = chain.gui_open.get(&index).copied().unwrap_or(false);
                        let (fill, stroke, ink) = if open {
                            (theme::SELECTION, theme::ACCENT, theme::ACCENT)
                        } else if has_gui {
                            (theme::PANEL, theme::BORDER, theme::TEXT)
                        } else {
                            (theme::PANEL, theme::BORDER_SOFT, theme::BORDER)
                        };
                        let button = ui.add_enabled(
                            has_gui,
                            Button::new("")
                                .fill(fill)
                                .stroke(Stroke::new(1.0, stroke))
                                .min_size(SQUARE_BUTTON),
                        );
                        window_glyph(ui, button.rect, ink);
                        let button = button
                            .on_hover_text(t("the plugin's own window"))
                            .on_disabled_hover_text(t("this slot has no window of its own"));
                        if button.clicked() {
                            commands.push(message(
                                &chain.name,
                                &ToChain::Gui {
                                    slot: index,
                                    open: !open,
                                },
                            ));
                        }
                        if let Some(presets) = info.map(|i| &i.presets).filter(|p| !p.is_empty()) {
                            let current = match &slot.spec {
                                SlotSpec::Lv2 { preset, .. } => preset.as_deref(),
                                _ => None,
                            };
                            let button = Button::new(RichText::new("P").size(11.0).color(
                                if current.is_some() {
                                    theme::ACCENT
                                } else {
                                    theme::TEXT_DIM
                                },
                            ))
                            .fill(theme::PANEL)
                            .stroke(Stroke::new(1.0, theme::BORDER))
                            .min_size(SQUARE_BUTTON);
                            let (response, _) = MenuButton::from_button(button).ui(ui, |ui| {
                                for preset in presets {
                                    let chosen = current == Some(preset.uri.as_str());
                                    if ui.selectable_label(chosen, &preset.label).clicked() {
                                        commands.push(message(
                                            &chain.name,
                                            &ToChain::Preset {
                                                slot: index,
                                                uri: preset.uri.clone(),
                                            },
                                        ));
                                        ui.close();
                                    }
                                }
                            });
                            response.on_hover_text(t("factory presets"));
                        }
                    });
                });
                if let (Some(mut state), SlotSpec::Group { lanes }) = (lanes_state, &mut slot.spec)
                {
                    state.store(ui.ctx());
                    if !state.is_open() {
                        for (lane, l) in lanes.iter().enumerate() {
                            let names: Vec<String> = (0..l.slots.len())
                                .map(|sub| {
                                    let id = lane_slot_id(index, lane, sub);
                                    slot_name(&l.slots[sub], chain.slot_infos.get(&id))
                                })
                                .collect();
                            let names = if names.is_empty() {
                                t("dry").to_owned()
                            } else {
                                names.join(" → ")
                            };
                            ui.add(
                                egui::Label::new(theme::small_dim(format!(
                                    "{} · {names}",
                                    lane + 1
                                )))
                                .truncate(),
                            );
                        }
                    }
                    // Slides open and shut like a folder.
                    state.show_body_unindented(ui, |ui| {
                        lanes_ui(
                            ui, graph, chain, index, key, lanes, picker, commands, actions,
                        );
                    });
                }
                // A larger chevron than egui's default, to match the 13 px label.
                ui.spacing_mut().icon_width = 16.0;
                egui::CollapsingHeader::new(
                    RichText::new(t("edit")).size(13.0).color(theme::TEXT_DIM),
                )
                .id_salt((&chain.name, key, "edit"))
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::small_dim(t("mix")));
                        ui.spacing_mut().slider_width = (ui.available_width() - 48.0).min(120.0);
                        let mut mix = slot.mix;
                        let response = ui.add(Slider::new(&mut mix, 0.0..=1.0).show_value(false));
                        ui.label(theme::small_dim(format!("{:>3.0}%", mix * 100.0)));
                        if response.changed() {
                            slot.mix = mix;
                            commands.push(message(&chain.name, &ToChain::Mix { slot: index, mix }));
                        }
                    });
                    match &mut slot.spec {
                        SlotSpec::Clap { params, .. } | SlotSpec::Lv2 { params, .. } => {
                            let infos = info.map_or(&[][..], |i| i.params.as_slice());
                            let (commits, learn) = egui::ScrollArea::vertical()
                                .id_salt((&chain.name, index, "params-scroll"))
                                .max_height(PARAMS_MAX_HEIGHT)
                                .show(ui, |ui| {
                                    clap_params(ui, (&chain.name, index), infos, params)
                                })
                                .inner;
                            if let Some(target) = learn {
                                commands.push(Command::Learn {
                                    target: Some(target),
                                });
                            }
                            for (id, value) in commits {
                                commands.push(message(
                                    &chain.name,
                                    &ToChain::Param {
                                        slot: index,
                                        id,
                                        value,
                                    },
                                ));
                            }
                        }
                        SlotSpec::Group { .. } => {}
                        SlotSpec::Hardware { send, ret, latency } => {
                            if hardware_insert(ui, graph, (&chain.name, index), send, ret) {
                                actions.rewrite = true;
                            }
                            ui.horizontal(|ui| {
                                if ui
                                    .add(Button::new(RichText::new(t("Measure")).size(11.0)))
                                    .on_hover_text(t(
                                        "sends a click through the gear and times its return",
                                    ))
                                    .clicked()
                                {
                                    commands.push(message(
                                        &chain.name,
                                        &ToChain::Measure { slot: index },
                                    ));
                                }
                                ui.label(theme::small_dim(if *latency == 0 {
                                    t("not measured").to_owned()
                                } else {
                                    tf(
                                        "{latency} smp · {ms} ms at 48 kHz",
                                        &[
                                            ("latency", &latency),
                                            ("ms", &format!("{:.1}", *latency as f32 / 48.0)),
                                        ],
                                    )
                                }));
                            });
                        }
                    }
                    ui.add_space(4.0);
                    if ui
                        .add(Button::new(RichText::new(t("Remove slot")).size(11.0)))
                        .clicked()
                    {
                        actions.remove = Some(index);
                    }
                });
            })
    });
    let card = card.response;
    let rename_id = egui::Id::unique((&chain.name, key, "rename"));
    let name = slot_name(slot, info);
    card.context_menu(|ui| {
        if ui.button(t("Rename…")).clicked() {
            ui.data_mut(|d| d.insert_temp(rename_id, name.clone()));
            ui.close();
        }
        slot_menu(ui, slot, index, top_level, folders, actions);
    });
    let after = ui
        .ctx()
        .pointer_latest_pos()
        .is_some_and(|p| p.y > card.rect.center().y);
    // A group's card holds its lane cards: those run first and claim the drop.
    if let Some(dragged) = card.dnd_hover_payload::<DragSlot>()
        && dragged.chain == chain.name
        && dragged.index != index
        && !actions.hover_taken
    {
        actions.hover_taken = true;
        drop_line(ui, card.rect, after);
    }
    if let Some(dragged) = card.dnd_release_payload::<DragSlot>()
        && dragged.chain == chain.name
        && dragged.index != index
        && actions.moved.is_none()
        && actions.into_lane.is_none()
    {
        actions.moved = Some((dragged.index, index, after));
    }
}

/// What the card calls the slot: the user's label, else the plugin's own name.
fn slot_name(slot: &SlotConfig, info: Option<&SlotInfo>) -> String {
    slot.label
        .clone()
        .unwrap_or_else(|| default_name(slot, info))
}

fn default_name(slot: &SlotConfig, info: Option<&SlotInfo>) -> String {
    info.map_or_else(|| slot.spec.name(), |i| i.name.clone())
}

/// Folders and remove: a slot card's right-click menu.
fn slot_menu(
    ui: &mut Ui,
    slot: &mut SlotConfig,
    index: usize,
    top_level: bool,
    folders: &[String],
    actions: &mut CardActions,
) {
    if top_level {
        if slot.folder.is_none() && ui.button(t("New folder")).clicked() {
            let mut n = 1;
            while folders.contains(&tf("Folder {n}", &[("n", &n)])) {
                n += 1;
            }
            slot.folder = Some(tf("Folder {n}", &[("n", &n)]));
            actions.rewrite = true;
            ui.close();
        }
        for name in folders {
            if slot.folder.as_deref() != Some(name.as_str())
                && ui.button(tf("Move to {name}", &[("name", name)])).clicked()
            {
                slot.folder = Some(name.clone());
                actions.rewrite = true;
                ui.close();
            }
        }
        if slot.folder.is_some() && ui.button(t("Take out of the folder")).clicked() {
            slot.folder = None;
            actions.rewrite = true;
            ui.close();
        }
        ui.separator();
    }
    if ui
        .button(RichText::new(t("Remove slot")).color(theme::RED))
        .clicked()
    {
        actions.remove = Some(index);
        ui.close();
    }
}

/// The lanes of a parallel group inside its card: a header with gain and mute per
/// lane, its slot cards, and a way to add slots and lanes.
#[allow(clippy::too_many_arguments)]
fn lanes_ui(
    ui: &mut Ui,
    graph: &Graph,
    chain: &ChainState,
    top: usize,
    top_key: u64,
    lanes: &mut Vec<Lane>,
    picker: &mut Option<Picker>,
    commands: &mut Vec<Command>,
    actions: &mut CardActions,
) {
    let mut drop_lane = None;
    for (lane_index, lane) in lanes.iter_mut().enumerate() {
        let lane_box = Frame::new()
            .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
            .corner_radius(theme::CORNER_RADIUS)
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    // Name, gain, pan, M and delete share about 185 px.
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let label = if lane.label.is_empty() {
                        tf("Lane {n}", &[("n", &(lane_index + 1))])
                    } else {
                        lane.label.clone()
                    };
                    ui.label(RichText::new(label).size(11.0).color(theme::TEXT_DIM));
                    let mut gain = lane.gain_db;
                    let response = ui.add(
                        DragValue::new(&mut gain)
                            .range(GAIN_OFF_DB..=12.0)
                            .speed(0.1)
                            .fixed_decimals(1)
                            .suffix(" dB")
                            .custom_formatter(|v, _| {
                                if v <= f64::from(GAIN_OFF_DB) {
                                    t("off").to_owned()
                                } else {
                                    format!("{v:+.1}")
                                }
                            }),
                    );
                    if response.changed() {
                        lane.gain_db = gain;
                        commands.push(message(
                            &chain.name,
                            &ToChain::LaneGain {
                                slot: top,
                                lane: lane_index,
                                gain_db: gain,
                            },
                        ));
                    }
                    let mut pan = lane.pan;
                    let response = ui
                        .add_sized(
                            vec2(PAN_WIDTH, ui.spacing().interact_size.y),
                            DragValue::new(&mut pan)
                                .range(-1.0..=1.0)
                                .speed(0.01)
                                .custom_formatter(|v, _| pan_label(v as f32))
                                .custom_parser(parse_pan),
                        )
                        .on_hover_text(t(
                            "pan: drag sideways or type L30, C, R30; double-click centres",
                        ));
                    if response.double_clicked() {
                        pan = 0.0;
                    }
                    if pan != lane.pan {
                        lane.pan = pan;
                        commands.push(message(
                            &chain.name,
                            &ToChain::LanePan {
                                slot: top,
                                lane: lane_index,
                                pan,
                            },
                        ));
                    }
                    let mute = if lane.mute {
                        Button::new(RichText::new("M").size(10.0).color(theme::RED_SOFT))
                            .fill(theme::RED_FILL)
                            .stroke(Stroke::new(1.0, theme::RED))
                    } else {
                        Button::new(RichText::new("M").size(10.0).color(theme::TEXT_DIM))
                            .fill(theme::PANEL)
                            .stroke(Stroke::new(1.0, theme::BORDER))
                    };
                    let side = ui.spacing().interact_size.y;
                    let mute = ui
                        .add(mute.min_size(vec2(side, side)))
                        .on_hover_text(t("mute this lane"));
                    let side = mute.rect.height();
                    if mute.clicked() {
                        lane.mute = !lane.mute;
                        commands.push(message(
                            &chain.name,
                            &ToChain::LaneMute {
                                slot: top,
                                lane: lane_index,
                                mute: lane.mute,
                            },
                        ));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if super::delete_square(ui, side)
                            .on_hover_text(t("remove this lane"))
                            .clicked()
                        {
                            drop_lane = Some(lane_index);
                        }
                    });
                });
                ui.add_space(4.0);
                for (sub, slot) in lane.slots.iter_mut().enumerate() {
                    card(
                        ui,
                        graph,
                        chain,
                        lane_slot_id(top, lane_index, sub),
                        top_key << 16 | ((lane_index as u64 + 1) << 8) | (sub as u64 + 1),
                        slot,
                        picker,
                        commands,
                        actions,
                        &[],
                    );
                }
                let add = Button::new("")
                    .frame(false)
                    .min_size(vec2(ui.available_width(), SMALL_ADD_HEIGHT));
                let (add, _) = MenuButton::from_button(add).ui(ui, |ui| {
                    if let Some(spec) = claritas_items(ui, graph) {
                        lane.slots.push(SlotConfig::new(spec));
                        actions.rewrite = true;
                    }
                    picker_entries(ui, picker, &chain.name, Some((top, lane_index)), None);
                });
                add_face(ui, &add, t("Add slot"), true);
            })
            .response;
        // A drop on the lane's box, past its cards, lands at the end of the lane.
        if let Some(dragged) = lane_box.dnd_hover_payload::<DragSlot>()
            && dragged.chain == chain.name
            && !actions.hover_taken
        {
            actions.hover_taken = true;
            ui.painter().rect_stroke(
                lane_box.rect,
                theme::CORNER_RADIUS,
                Stroke::new(2.0, theme::ACCENT),
                StrokeKind::Outside,
            );
        }
        if let Some(dragged) = lane_box.dnd_release_payload::<DragSlot>()
            && dragged.chain == chain.name
            && actions.moved.is_none()
            && actions.into_lane.is_none()
        {
            actions.into_lane = Some((dragged.index, top, lane_index));
        }
    }
    if let Some(lane) = drop_lane
        && lane < lanes.len()
    {
        lanes.remove(lane);
        actions.rewrite = true;
    }
    if lanes.len() < MAX_LANES {
        let add = ui.add(
            Button::new("")
                .frame(false)
                .min_size(vec2(ui.available_width(), SMALL_ADD_HEIGHT)),
        );
        add_face(ui, &add, t("Add lane"), true);
        if add.clicked() {
            let label = tf("Lane {n}", &[("n", &(lanes.len() + 1))]);
            lanes.push(Lane::new(&label));
            actions.rewrite = true;
        }
    }
}

fn slots(
    ui: &mut Ui,
    graph: &Graph,
    chain: &ChainState,
    picker: &mut Option<Picker>,
    commands: &mut Vec<Command>,
) {
    let mut slots: Vec<SlotConfig> = chain
        .slots
        .iter()
        .filter_map(|v| v.clone().try_into().ok())
        .collect();
    let keys = slot_keys(ui, &chain.name, &slots);
    let mut actions = CardActions::default();
    ui.spacing_mut().item_spacing.y = 6.0;
    let folders: Vec<String> =
        slots
            .iter()
            .filter_map(|s| s.folder.clone())
            .fold(Vec::new(), |mut names, f| {
                if !names.contains(&f) {
                    names.push(f);
                }
                names
            });
    let mut index = 0;
    while index < slots.len() {
        let Some(folder) = slots[index].folder.clone() else {
            card(
                ui,
                graph,
                chain,
                index,
                keys[index],
                &mut slots[index],
                picker,
                commands,
                &mut actions,
                &folders,
            );
            index += 1;
            continue;
        };
        let end = slots[index..]
            .iter()
            .position(|s| s.folder.as_deref() != Some(folder.as_str()))
            .map_or(slots.len(), |n| index + n);
        let id = ui.make_persistent_id(("folder", &chain.name, &folder));
        let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, true);
        folder_header(
            ui,
            chain,
            &folder,
            end - index,
            &mut slots,
            index,
            end,
            &mut actions,
            &mut state,
        );
        let mut added = None;
        // Slides open and shut; the state remembers the open height for the animation.
        state.show_body_unindented(ui, |ui| {
            ui.indent((&chain.name, &folder), |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for (i, slot) in slots.iter_mut().enumerate().take(end).skip(index) {
                    card(
                        ui,
                        graph,
                        chain,
                        i,
                        keys[i],
                        slot,
                        picker,
                        commands,
                        &mut actions,
                        &folders,
                    );
                }
                let add = Button::new("")
                    .frame(false)
                    .min_size(vec2(ui.available_width(), SMALL_ADD_HEIGHT));
                let (add, _) = MenuButton::from_button(add).ui(ui, |ui| {
                    if let Some(spec) = claritas_items(ui, graph) {
                        added = Some(spec);
                    }
                    picker_entries(ui, picker, &chain.name, None, Some(folder.clone()));
                });
                add_face(ui, &add, t("Add slot"), true);
            });
        });
        index = end;
        if let Some(spec) = added {
            let mut config = SlotConfig::new(spec);
            config.folder = Some(folder.clone());
            slots.insert(end, config);
            actions.rewrite = true;
            index += 1;
        }
    }
    let mut rewrite = actions.rewrite;
    if let Some((from, to, after)) = actions.moved {
        rewrite |= move_slot(&mut slots, from, to, after);
    }
    if let Some((from, top, lane)) = actions.into_lane {
        rewrite |= move_into_lane(&mut slots, from, top, lane);
    }
    if let Some((from, start)) = actions.before_folder
        && let SlotPath::Top(from) = SlotPath::from_id(from)
        && from < slots.len()
    {
        let mut slot = slots.remove(from);
        slot.folder = None;
        slots.insert(landing(from, start, false), slot);
        rewrite = true;
    }
    if let Some((from, folder)) = actions.into_folder
        && let SlotPath::Top(from) = SlotPath::from_id(from)
        && from < slots.len()
    {
        let mut slot = slots.remove(from);
        slot.folder = Some(folder.clone());
        let at = slots
            .iter()
            .rposition(|s| s.folder.as_deref() == Some(folder.as_str()))
            .map_or(slots.len(), |i| i + 1);
        slots.insert(at, slot);
        rewrite = true;
    }
    if let Some(id) = actions.remove {
        match SlotPath::from_id(id) {
            SlotPath::Top(top) if top < slots.len() => {
                slots.remove(top);
                rewrite = true;
            }
            SlotPath::Lane { top, lane, sub } => {
                if let Some(SlotSpec::Group { lanes }) = slots.get_mut(top).map(|s| &mut s.spec)
                    && let Some(l) = lanes.get_mut(lane)
                    && sub < l.slots.len()
                {
                    l.slots.remove(sub);
                    rewrite = true;
                }
            }
            SlotPath::Top(_) => {}
        }
    }
    // Painted below once its hover state is known; the button only takes the click.
    let add_button = Button::new("")
        .frame(false)
        .min_size(vec2(ui.available_width(), 38.0));
    let (add, _) = MenuButton::from_button(add_button).ui(ui, |ui| {
        if let Some(spec) = claritas_items(ui, graph) {
            slots.push(SlotConfig::new(spec));
            rewrite = true;
        }
        let hardware = |class: MediaClass, default: Option<&str>| -> Option<String> {
            default
                .and_then(|d| graph.node_by_name(d))
                .filter(|n| n.class == class && n.hardware)
                .or_else(|| graph.nodes.iter().find(|n| n.class == class && n.hardware))
                .map(|n| n.name.clone())
        };
        if let (Some(send), Some(ret)) = (
            hardware(MediaClass::Sink, graph.defaults.sink.as_deref()),
            hardware(MediaClass::Source, graph.defaults.source.as_deref()),
        ) && ui
            .button(t("Hardware insert"))
            .on_hover_text(t(
                "external gear: out through an interface output pair, back through an input pair",
            ))
            .clicked()
        {
            slots.push(SlotConfig::new(SlotSpec::Hardware {
                send,
                ret,
                latency: 0,
            }));
            rewrite = true;
            ui.close();
        }
        if ui
            .button(t("Parallel lanes"))
            .on_hover_text(t(
                "two lanes on a copy of the signal, summed back with a gain each",
            ))
            .clicked()
        {
            slots.push(SlotConfig::new(SlotSpec::Group {
                lanes: vec![
                    Lane::new(&tf("Lane {n}", &[("n", &1)])),
                    Lane::new(&tf("Lane {n}", &[("n", &2)])),
                ],
            }));
            rewrite = true;
            ui.close();
        }
        ui.separator();
        picker_entries(ui, picker, &chain.name, None, None);
    });
    add_slot_face(ui, &add);
    if let Some(dragged) = add.dnd_hover_payload::<DragSlot>()
        && dragged.chain == chain.name
    {
        drop_line(ui, add.rect, false);
    }
    if let Some(dragged) = add.dnd_release_payload::<DragSlot>()
        && dragged.chain == chain.name
        && let SlotPath::Top(from) = SlotPath::from_id(dragged.index)
        && from < slots.len()
    {
        let mut slot = slots.remove(from);
        slot.folder = None;
        slots.push(slot);
        rewrite = true;
    }
    if rewrite {
        let values: Vec<toml::Value> = slots
            .iter()
            .filter_map(|s| toml::Value::try_from(s).ok())
            .collect();
        commands.push(Command::SetChainSlots {
            name: chain.name.clone(),
            slots: values,
        });
    }
}

/// The Claritas entries of an add-slot menu: the CLAPs that ship with Canalis, greyed
/// with a note when this install lacks them.
fn claritas_items(ui: &mut Ui, graph: &Graph) -> Option<SlotSpec> {
    let missing = t("not found: the Claritas plugins ship with Canalis; reinstall it");
    let mut chosen = None;
    for (label, spec) in [
        ("Claritas EQ", presets::claritas_eq(graph)),
        ("Claritas Comp", presets::claritas_comp(graph)),
    ] {
        let item = ui.add_enabled(spec.is_some(), Button::new(label));
        if item.on_disabled_hover_text(missing).clicked() {
            chosen = spec;
        }
    }
    if chosen.is_some() {
        ui.close();
    }
    chosen
}

/// How far a hover has faded in, 0..1, eased over `HOVER_FADE` both ways.
fn hover_fade(ui: &Ui, response: &egui::Response) -> f32 {
    let hot = response.hovered() || response.is_pointer_button_down_on();
    ui.ctx()
        .animate_bool_with_time(response.id.with("hover-fade"), hot, HOVER_FADE)
}

/// The small dashed "+" that adds an end: the dash and the plus brighten on hover.
fn plus_face(ui: &Ui, plus: &egui::Response) {
    let t = hover_fade(ui, plus);
    let edge = theme::BORDER.lerp_to_gamma(theme::ACCENT, t);
    let ink = theme::TEXT_DIM.lerp_to_gamma(theme::ACCENT, t);
    if t > 0.0 {
        ui.painter().rect_filled(
            plus.rect,
            theme::CORNER_RADIUS,
            theme::ACCENT.gamma_multiply(0.08 * t),
        );
    }
    dashed_rect(ui, plus.rect, edge);
    // The plus grows a touch with the hover.
    let c = plus.rect.center();
    let arm = 3.5 + t;
    let s = Stroke::new(1.3, ink);
    ui.painter()
        .line_segment([c - vec2(arm, 0.0), c + vec2(arm, 0.0)], s);
    ui.painter()
        .line_segment([c - vec2(0.0, arm), c + vec2(0.0, arm)], s);
}

/// "+ Add slot": a dashed box with a ringed plus and the words, centred; accent on hover.
fn add_slot_face(ui: &Ui, add: &egui::Response) {
    add_face(ui, add, t("Add slot"), false);
}

/// The dashed add box with `label`; `compact` for the small ones inside lanes and folders.
fn add_face(ui: &Ui, add: &egui::Response, label: &str, compact: bool) {
    let t = hover_fade(ui, add);
    let edge = theme::BORDER.lerp_to_gamma(theme::ACCENT, t);
    let ink = theme::TEXT_DIM.lerp_to_gamma(theme::TEXT, t);
    let painter = ui.painter();
    if t > 0.0 {
        painter.rect_filled(
            add.rect,
            theme::CORNER_RADIUS,
            theme::ACCENT.gamma_multiply(0.06 * t),
        );
    }
    dashed_rect(ui, add.rect, edge);
    let (size, ring, arm, gap) = if compact {
        (11.5, 6.0, 2.5, 5.0)
    } else {
        (12.5, 8.0, 3.5, 7.0)
    };
    let galley = painter.layout_no_wrap(label.to_owned(), egui::FontId::proportional(size), ink);
    let total = 2.0 * ring + gap + galley.size().x;
    let left = add.rect.center().x - total / 2.0;
    let c = pos2(left + ring, add.rect.center().y);
    let plus = ink.lerp_to_gamma(theme::ACCENT, t);
    painter.circle_stroke(c, ring, Stroke::new(1.2, plus));
    painter.line_segment(
        [c - vec2(arm, 0.0), c + vec2(arm, 0.0)],
        Stroke::new(1.4, plus),
    );
    painter.line_segment(
        [c - vec2(0.0, arm), c + vec2(0.0, arm)],
        Stroke::new(1.4, plus),
    );
    painter.galley(
        pos2(
            c.x + ring + gap,
            add.rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        ink,
    );
}

/// A key per top-level slot that follows it when slots move, so a card's folds stay
/// with the card: slots are matched to last frame's by kind or plugin, in order.
fn slot_keys(ui: &Ui, chain: &str, slots: &[SlotConfig]) -> Vec<u64> {
    let memory = egui::Id::unique((chain, "slot-keys"));
    let (old, old_keys, mut next): (Vec<String>, Vec<u64>, u64) =
        ui.data(|d| d.get_temp(memory)).unwrap_or_default();
    let kinds: Vec<String> = slots
        .iter()
        .map(|s| match &s.spec {
            SlotSpec::Clap { id, .. } => id.clone(),
            SlotSpec::Lv2 { uri, .. } => uri.clone(),
            SlotSpec::Group { .. } => "group".to_owned(),
            other => other.name(),
        })
        .collect();
    if kinds == old {
        return old_keys;
    }
    let mut used = vec![false; old.len()];
    let keys: Vec<u64> = kinds
        .iter()
        .map(
            |kind| match (0..old.len()).find(|&o| !used[o] && old[o] == *kind) {
                Some(o) => {
                    used[o] = true;
                    old_keys[o]
                }
                None => {
                    next += 1;
                    next
                }
            },
        )
        .collect();
    ui.data_mut(|d| d.insert_temp(memory, (kinds, keys.clone(), next)));
    keys
}

/// "C" at the centre, else the side and how far: "L30", "R100".
fn pan_label(pan: f32) -> String {
    let amount = (pan.abs() * 100.0).round();
    if amount == 0.0 {
        "C".to_owned()
    } else if pan < 0.0 {
        format!("L{amount}")
    } else {
        format!("R{amount}")
    }
}

/// Reads what `pan_label` writes, plus a bare number from −100 to 100.
fn parse_pan(text: &str) -> Option<f64> {
    let text = text.trim().to_ascii_uppercase();
    let (sign, number) = match text.chars().next()? {
        'C' if text.len() == 1 => return Some(0.0),
        'L' => (-1.0, &text[1..]),
        'R' => (1.0, &text[1..]),
        _ => (1.0, text.as_str()),
    };
    let amount: f64 = number.trim().parse().ok()?;
    Some((sign * amount / 100.0).clamp(-1.0, 1.0))
}

/// The lane `lane` of the group at top-level `top`.
fn lane_of(slots: &mut [SlotConfig], top: usize, lane: usize) -> Option<&mut Lane> {
    match slots.get_mut(top).map(|s| &mut s.spec) {
        Some(SlotSpec::Group { lanes }) => lanes.get_mut(lane),
        _ => None,
    }
}

/// Whether the slot at `from` may go into lane `lane` of group `top`: groups do not
/// nest, a group does not go into itself, and a lane has room for `LANE_SLOTS`.
fn fits_lane(slots: &mut [SlotConfig], from: SlotPath, top: usize, lane: usize) -> bool {
    let moving_group = match from {
        SlotPath::Top(f) => {
            f == top || matches!(slots.get(f).map(|s| &s.spec), Some(SlotSpec::Group { .. }))
        }
        SlotPath::Lane { .. } => false,
    };
    let same_lane = matches!(from, SlotPath::Lane { top: t, lane: l, .. } if t == top && l == lane);
    !moving_group
        && lane_of(slots, top, lane).is_some_and(|l| same_lane || l.slots.len() < LANE_SLOTS)
}

/// Takes the slot at `path` out of the list, from the top level or from a lane.
fn take_slot(slots: &mut Vec<SlotConfig>, path: SlotPath) -> Option<SlotConfig> {
    match path {
        SlotPath::Top(i) => (i < slots.len()).then(|| slots.remove(i)),
        SlotPath::Lane { top, lane, sub } => {
            let lane = lane_of(slots, top, lane)?;
            (sub < lane.slots.len()).then(|| lane.slots.remove(sub))
        }
    }
}

/// A top-level index after the slot at `from` was taken out.
fn shifted(top: usize, from: SlotPath) -> usize {
    match from {
        SlotPath::Top(f) if f < top => top - 1,
        _ => top,
    }
}

/// A slot dropped on the card at `to`, before it or `after` it: within a list, between
/// lanes, into a lane from the top level or out of one. True when the list changed.
fn move_slot(slots: &mut Vec<SlotConfig>, from: usize, to: usize, after: bool) -> bool {
    let (from, to) = (SlotPath::from_id(from), SlotPath::from_id(to));
    match to {
        SlotPath::Top(t) => {
            if matches!(from, SlotPath::Lane { top, .. } if top == t) && t >= slots.len() {
                return false;
            }
            let Some(mut slot) = take_slot(slots, from) else {
                return false;
            };
            let at = match from {
                SlotPath::Top(f) => landing(f, t, after),
                SlotPath::Lane { .. } => t + usize::from(after),
            };
            let at = at.min(slots.len());
            // It takes the folder of the card it was dropped on.
            let neighbour = if after { at.checked_sub(1) } else { Some(at) };
            slot.folder = neighbour
                .and_then(|i| slots.get(i))
                .and_then(|s| s.folder.clone());
            slots.insert(at, slot);
            true
        }
        SlotPath::Lane { top, lane, sub } => {
            if !fits_lane(slots, from, top, lane) {
                return false;
            }
            let Some(mut slot) = take_slot(slots, from) else {
                return false;
            };
            slot.folder = None;
            let top = shifted(top, from);
            let at = match from {
                SlotPath::Lane {
                    top: ft,
                    lane: fl,
                    sub: fs,
                } if ft == top && fl == lane => landing(fs, sub, after),
                _ => sub + usize::from(after),
            };
            let Some(target) = lane_of(slots, top, lane) else {
                return false;
            };
            target.slots.insert(at.min(target.slots.len()), slot);
            true
        }
    }
}

/// A slot dropped on a lane's box: the end of that lane. True when the list changed.
fn move_into_lane(slots: &mut Vec<SlotConfig>, from: usize, top: usize, lane: usize) -> bool {
    let from = SlotPath::from_id(from);
    if !fits_lane(slots, from, top, lane) {
        return false;
    }
    let Some(mut slot) = take_slot(slots, from) else {
        return false;
    };
    slot.folder = None;
    let Some(target) = lane_of(slots, shifted(top, from), lane) else {
        return false;
    };
    target.slots.push(slot);
    true
}

/// Where a slot taken out at `from` goes to land before (or `after`) the one at `to`,
/// counted in the list without it.
fn landing(from: usize, to: usize, after: bool) -> usize {
    let at = to + usize::from(after);
    if from < at { at - 1 } else { at }
}

/// Pickers for the hardware insert's send and return devices; `true` when one changed.
fn hardware_insert(
    ui: &mut Ui,
    graph: &Graph,
    id: (&str, usize),
    send: &mut String,
    ret: &mut String,
) -> bool {
    let mut changed = false;
    for (caption, class, chosen) in [
        (t("Send to"), MediaClass::Sink, &mut *send),
        (t("Return from"), MediaClass::Source, &mut *ret),
    ] {
        let devices: Vec<&Node> = graph
            .nodes
            .iter()
            .filter(|n| n.class == class && n.hardware)
            .collect();
        let label = graph.node_by_name(chosen).map_or_else(
            || tf("{label} (absent)", &[("label", &chosen)]),
            |n| n.label.clone(),
        );
        ui.horizontal(|ui| {
            ui.label(theme::small_dim(caption));
            ComboBox::from_id_salt((id, caption))
                .selected_text(RichText::new(label).size(11.0))
                .width(180.0)
                .show_ui(ui, |ui| {
                    for device in devices {
                        if ui
                            .selectable_label(*chosen == device.name, &device.label)
                            .clicked()
                            && *chosen != device.name
                        {
                            *chosen = device.name.clone();
                            changed = true;
                        }
                    }
                });
        });
    }
    changed
}

/// The grey line under a slot's name: what it is and how it is doing.
fn slot_subtitle(
    slot: &SlotConfig,
    info: Option<&SlotInfo>,
    crashed: bool,
    overload: Option<&str>,
) -> (String, Color32) {
    if crashed {
        return (t("active at the crash").into(), theme::RED);
    }
    if let Some(error) = info.and_then(|i| i.error.as_deref()) {
        return (error.to_owned(), theme::RED);
    }
    if slot.bypass {
        // Shown only while bypassed: un-bypassing by hand clears it from view.
        return match overload {
            Some(times) => (
                tf("too slow · {times}", &[("times", &times)]),
                theme::ORANGE,
            ),
            None => (t("bypassed").into(), theme::RED),
        };
    }
    let what = match &slot.spec {
        SlotSpec::Group { lanes } => {
            if lanes.len() == 1 {
                tf("parallel · {n} lane", &[("n", &lanes.len())])
            } else {
                tf("parallel · {n} lanes", &[("n", &lanes.len())])
            }
        }
        SlotSpec::Hardware { latency, .. } if *latency == 0 => {
            t("hardware · round trip not measured").to_owned()
        }
        SlotSpec::Hardware { latency, .. } => tf(
            "hardware · round trip {latency} smp",
            &[("latency", latency)],
        ),
        SlotSpec::Clap { .. } => match info {
            Some(i) if i.params.len() == 1 => tf("CLAP · {n} parameter", &[("n", &i.params.len())]),
            Some(i) => tf("CLAP · {n} parameters", &[("n", &i.params.len())]),
            None => t("CLAP · loading").to_owned(),
        },
        SlotSpec::Lv2 { preset, .. } => match info {
            Some(i) => {
                let preset = preset.as_deref().map(|uri| {
                    i.presets
                        .iter()
                        .find(|p| p.uri == uri)
                        .map_or_else(
                            || uri.rsplit(['/', '#']).next().unwrap_or(uri),
                            |p| &p.label,
                        )
                        .to_owned()
                });
                let n = i.params.len();
                match (preset, n == 1) {
                    (Some(label), true) => tf(
                        "LV2 · {n} control · {label}",
                        &[("n", &n), ("label", &label)],
                    ),
                    (Some(label), false) => tf(
                        "LV2 · {n} controls · {label}",
                        &[("n", &n), ("label", &label)],
                    ),
                    (None, true) => tf("LV2 · {n} control", &[("n", &n)]),
                    (None, false) => tf("LV2 · {n} controls", &[("n", &n)]),
                }
            }
            None => t("LV2 · loading").to_owned(),
        },
    };
    let text = if slot.mix < 1.0 {
        tf(
            "{what} · wet {percent} %",
            &[
                ("what", &what),
                ("percent", &format!("{:.0}", slot.mix * 100.0)),
            ],
        )
    } else {
        what
    };
    (text, theme::TEXT_DIM)
}

/// One row per plugin parameter in its own range; the saved value wins over the plugin's,
/// so what the user set shows even before the chain confirms it. Returns the commits, and
/// the parameter a right-click asked to learn a MIDI control for.
fn clap_params(
    ui: &mut Ui,
    id: (&str, usize),
    infos: &[ParamInfo],
    saved: &mut std::collections::BTreeMap<String, f64>,
) -> (Vec<(u32, f64)>, Option<ControlTarget>) {
    let mut commits = Vec::new();
    let mut learn = None;
    if infos.is_empty() {
        ui.label(theme::small_dim(t("no parameters reported")));
        return (commits, learn);
    }
    egui::Grid::new((id, "clap-params"))
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            for info in infos {
                let key = info.id.to_string();
                let mut value = saved.get(&key).copied().unwrap_or(info.value);
                let label = if info.module.is_empty() {
                    info.name.clone()
                } else {
                    format!("{} › {}", info.module, info.name)
                };
                let name = ui.label(theme::small_dim(&label));
                let span = (info.max - info.min).max(f64::EPSILON);
                let response = ui.add(
                    DragValue::new(&mut value)
                        .range(info.min..=info.max)
                        .speed(span / 300.0)
                        .max_decimals(if info.stepped { 0 } else { 3 }),
                );
                if commit(response.clone()) {
                    saved.insert(key, value);
                    commits.push((info.id, value));
                }
                for row in [name, response] {
                    row.context_menu(|ui| {
                        if ui
                            .button(t("Learn MIDI control"))
                            .on_hover_text(t("then turn a knob or press a pad on the MIDI input"))
                            .clicked()
                        {
                            learn = Some(ControlTarget::Param {
                                chain: id.0.to_owned(),
                                slot: id.1,
                                id: info.id,
                                name: label.clone(),
                                min: info.min,
                                max: info.max,
                                stepped: info.stepped,
                            });
                            ui.close();
                        }
                    });
                }
                ui.end_row();
            }
        });
    (commits, learn)
}

/// A drag value counts as committed on release or when typed, not on every drag frame.
fn commit(response: egui::Response) -> bool {
    response.drag_stopped() || (response.changed() && !response.dragged())
}

/// The chain's slot list with `config` appended, to the chain, one of a group's lanes or
/// the end of a folder's run, as the control plane stores it.
fn add_slot(
    chain: &ChainState,
    config: SlotConfig,
    lane: Option<(usize, usize)>,
    folder: Option<String>,
) -> Command {
    let mut slots: Vec<SlotConfig> = chain
        .slots
        .iter()
        .filter_map(|v| v.clone().try_into().ok())
        .collect();
    match (lane, folder) {
        (Some((top, lane)), _) => {
            if let Some(SlotSpec::Group { lanes }) = slots.get_mut(top).map(|s| &mut s.spec)
                && let Some(l) = lanes.get_mut(lane)
            {
                l.slots.push(config);
            }
        }
        (None, Some(folder)) => {
            let mut config = config;
            config.folder = Some(folder.clone());
            let at = slots
                .iter()
                .rposition(|s| s.folder.as_deref() == Some(folder.as_str()))
                .map_or(slots.len(), |i| i + 1);
            slots.insert(at, config);
        }
        (None, None) => slots.push(config),
    }
    Command::SetChainSlots {
        name: chain.name.clone(),
        slots: slots
            .iter()
            .filter_map(|s| toml::Value::try_from(s).ok())
            .collect(),
    }
}

fn message(chain: &str, to_chain: &ToChain) -> Command {
    Command::ChainMessage {
        name: chain.to_owned(),
        message: toml::Value::try_from(to_chain).unwrap_or(toml::Value::Table(Default::default())),
    }
}

fn level(peak: f32) -> (f32, Color32) {
    let db = 20.0 * peak.max(1e-6).log10();
    let fill = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
    let color = if db > -3.0 {
        theme::ORANGE
    } else {
        theme::GREEN
    };
    (fill, color)
}

/// Two thin bars side by side, filling upwards.
fn meter_vertical(ui: &mut Ui, peaks: [f32; 2], height: f32) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(METER_BAR * 2.0 + METER_GAP, height), Sense::hover());
    let painter = ui.painter();
    for (c, peak) in peaks.iter().enumerate() {
        let x = rect.left() + c as f32 * (METER_BAR + METER_GAP);
        let bar = Rect::from_min_size(pos2(x, rect.top()), vec2(METER_BAR, height));
        painter.rect(
            bar,
            2.0,
            theme::BORDER_SOFT,
            Stroke::NONE,
            StrokeKind::Inside,
        );
        let (fill, color) = level(*peak);
        if fill > 0.0 {
            let top = bar.bottom() - fill * height;
            painter.rect_filled(
                Rect::from_min_max(pos2(bar.left(), top), bar.right_bottom()),
                2.0,
                color,
            );
        }
    }
}

/// Two thin bars stacked, filling to the right.
fn meter_horizontal(ui: &mut Ui, peaks: [f32; 2], width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 5.0 * 2.0 + METER_GAP), Sense::hover());
    let painter = ui.painter();
    for (c, peak) in peaks.iter().enumerate() {
        let y = rect.top() + c as f32 * (5.0 + METER_GAP);
        let bar = Rect::from_min_size(pos2(rect.left(), y), vec2(width, 5.0));
        painter.rect(
            bar,
            2.0,
            theme::BORDER_SOFT,
            Stroke::NONE,
            StrokeKind::Inside,
        );
        let (fill, color) = level(*peak);
        if fill > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(bar.left_top(), vec2(fill * width, 5.0)),
                2.0,
                color,
            );
        }
    }
}

fn kind_label(kind: ChainKind) -> &'static str {
    match kind {
        ChainKind::Output => t("Output"),
        ChainKind::Mic => t("Mic"),
        ChainKind::Cable => t("Cable"),
        ChainKind::Insert => t("Insert"),
    }
}

fn kind_help(kind: ChainKind) -> &'static str {
    match kind {
        ChainKind::Output => t("apps play into it, you pick where it goes"),
        ChainKind::Mic => t("you pick what feeds it, apps record from it"),
        ChainKind::Cable => t("apps play into it and record from it"),
        ChainKind::Insert => t("you pick both ends; a chain between chains"),
    }
}

/// How to get plugins: install a family, or drop files on the window; Settings has more.
fn get_more_plugins(ui: &mut Ui, lv2_off: bool) {
    egui::CollapsingHeader::new(RichText::new(t("Get more plugins")).size(12.0))
        .id_salt("get-more-plugins")
        .show(ui, |ui| {
            super::settings::get_more_compact(ui, lv2_off);
            ui.label(theme::small_dim(t(
                "or drop a .clap file or an .lv2 folder on this window; \
                 Settings › Plugins manages what is installed",
            )));
        });
}

#[cfg(test)]
mod tests {
    use super::{Lane, SlotConfig, SlotSpec, landing, move_into_lane, move_slot};
    use canalis_pw::lane_slot_id;

    fn comp() -> SlotConfig {
        SlotConfig::new(SlotSpec::Clap {
            path: String::new(),
            id: "audio.claritas.comp".into(),
            params: Default::default(),
            state: None,
        })
    }

    fn group() -> SlotConfig {
        SlotConfig::new(SlotSpec::Group {
            lanes: vec![Lane::new("Lane 1"), Lane::new("Lane 2")],
        })
    }

    fn lane_len(slots: &[SlotConfig], top: usize, lane: usize) -> usize {
        match &slots[top].spec {
            SlotSpec::Group { lanes } => lanes[lane].slots.len(),
            _ => panic!("not a group"),
        }
    }

    #[test]
    fn slots_move_into_lanes_between_them_and_back_out() {
        let mut slots = vec![comp(), group(), comp()];
        // The first comp into lane 1: the group shifts up to index 0.
        assert!(move_into_lane(&mut slots, 0, 1, 0));
        assert_eq!(slots.len(), 2);
        assert_eq!(lane_len(&slots, 0, 0), 1);
        // From lane 1 to lane 2, dropped on nothing but the box.
        assert!(move_into_lane(&mut slots, lane_slot_id(0, 0, 0), 0, 1));
        assert_eq!((lane_len(&slots, 0, 0), lane_len(&slots, 0, 1)), (0, 1));
        // Out of the lane, after the last top-level card.
        assert!(move_slot(&mut slots, lane_slot_id(0, 1, 0), 1, true));
        assert_eq!(slots.len(), 3);
        assert_eq!(lane_len(&slots, 0, 1), 0);
        // A top-level comp dropped before a card inside a lane.
        assert!(move_into_lane(&mut slots, 1, 0, 0));
        assert!(move_slot(&mut slots, 1, lane_slot_id(0, 0, 0), false));
        assert_eq!(lane_len(&slots, 0, 0), 2);
        assert_eq!(slots.len(), 1);
    }

    #[test]
    fn pan_labels_read_back() {
        for pan in [-1.0f32, -0.3, 0.0, 0.25, 1.0] {
            let back = super::parse_pan(&super::pan_label(pan)).unwrap() as f32;
            assert!((back - pan).abs() < 1e-6, "{pan} came back as {back}");
        }
        assert_eq!(super::parse_pan("-50"), Some(-0.5));
        assert_eq!(super::parse_pan("l 20"), Some(-0.2));
        assert_eq!(super::parse_pan("x"), None);
    }

    #[test]
    fn a_group_does_not_go_into_a_lane() {
        let mut slots = vec![group(), group()];
        assert!(!move_into_lane(&mut slots, 0, 0, 0), "not into itself");
        assert!(!move_into_lane(&mut slots, 1, 0, 0), "groups do not nest");
        assert_eq!(slots.len(), 2);
    }

    fn moved(from: usize, to: usize, after: bool) -> Vec<char> {
        let mut list: Vec<char> = "abcde".chars().collect();
        let item = list.remove(from);
        list.insert(landing(from, to, after), item);
        list
    }

    #[test]
    fn a_dropped_slot_lands_on_the_side_of_the_target_it_was_dropped_on() {
        assert_eq!(moved(0, 2, false), "bacde".chars().collect::<Vec<_>>());
        assert_eq!(moved(0, 2, true), "bcade".chars().collect::<Vec<_>>());
        assert_eq!(moved(4, 1, false), "aebcd".chars().collect::<Vec<_>>());
        assert_eq!(moved(4, 1, true), "abecd".chars().collect::<Vec<_>>());
        assert_eq!(moved(1, 0, true), "abcde".chars().collect::<Vec<_>>());
    }
}
