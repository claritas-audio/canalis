#![forbid(unsafe_code)]
//! The Canalis window: four views over one PipeWire graph.

mod autoeq;
mod background;
mod cpu;
mod file_dialog;
mod i18n;
mod intro;
mod notify;
mod packages;
mod plugin_install;
mod service;
mod theme;
mod tray;
mod undo;
mod views;

use std::sync::Arc;

use canalis_pw::{CONFIG_VERSION, Command, ConfigProblem, Graph};
use eframe::egui;
use i18n::{t, tf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum View {
    #[default]
    Devices,
    Apps,
    Chains,
    Graph,
    /// The soundboard; a tab only while it is switched on in Settings.
    Pads,
}

impl View {
    const ALL: [View; 5] = [
        View::Devices,
        View::Apps,
        View::Chains,
        View::Graph,
        View::Pads,
    ];

    /// `CANALIS_VIEW=devices|apps|chains|graph|pads` picks the tab the window opens on.
    fn from_env() -> View {
        std::env::var("CANALIS_VIEW")
            .ok()
            .and_then(|v| View::parse(&v))
            .unwrap_or_default()
    }

    fn parse(name: &str) -> Option<View> {
        View::ALL
            .into_iter()
            .find(|v| v.name().eq_ignore_ascii_case(name))
    }

    fn name(self) -> &'static str {
        match self {
            View::Devices => "Devices",
            View::Apps => "Apps",
            View::Chains => "Chains",
            View::Graph => "Graph",
            View::Pads => "Pads",
        }
    }

    fn title(self) -> &'static str {
        match self {
            View::Devices => t("Devices"),
            View::Apps => t("Apps"),
            View::Chains => t("Chains"),
            View::Graph => t("Graph"),
            View::Pads => t("Pads"),
        }
    }
}

struct App {
    view: View,
    pw: service::Plane,
    graph: Arc<Graph>,
    devices_view: views::DevicesView,
    apps_view: views::AppsView,
    graph_view: views::GraphView,
    chains_view: views::ChainsView,
    pads_view: views::PadsView,
    settings_view: views::SettingsView,
    /// The settings page covers the tabs' content while open.
    settings_open: bool,
    /// `packages::Snapshot::changes` last seen: a new install or removal means a rescan.
    package_changes: u64,
    /// `plugin_install::Fetching::changes` last seen: a finished download means a rescan.
    download_changes: u64,
    /// `CANALIS_SCREENSHOT=<file.png>` saves the window after a few frames and quits:
    /// how a session without a display tool checks what a view looks like.
    screenshot: Option<std::path::PathBuf>,
    frames: u32,
    undo: undo::UndoStack,
    /// The tab change under way: the view leaving and when it started.
    transition: Option<Transition>,
    /// The "Save scene" prompt's text, while it is open.
    scene_prompt: Option<String>,
    /// `CANALIS_REDUCED_MOTION` turns the slides and fades off.
    motion: bool,
    /// The logo plugging itself in over the window, until it is over or skipped.
    intro: Option<intro::Intro>,
    /// A message for the status bar (a plugin dropped on the window), and since when.
    notice: Option<(String, std::time::Instant)>,
}

struct Transition {
    from: View,
    started: std::time::Instant,
    /// +1 when the new tab is to the right of the old one, -1 otherwise.
    direction: f32,
}

/// How long a tab change takes, eased.
const TRANSITION: std::time::Duration = std::time::Duration::from_millis(200);
/// The tab highlight slides in this long.
const TAB_SLIDE: f32 = 0.18;
/// Room the tab highlight keeps around its text.
const TAB_PADDING: egui::Vec2 = egui::vec2(10.0, 4.0);

/// Smooth start and stop: cubic in-out.
fn ease(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// How long a status bar notice stays.
const NOTICE_FOR: std::time::Duration = std::time::Duration::from_secs(8);
/// Frames to wait before the screenshot, so the graph and meters are in.
const SCREENSHOT_AFTER: u32 = 90;
/// The canal mark and the wordmark in the tab bar, at their SVGs' aspect ratios.
const MARK_SIZE: egui::Vec2 = egui::vec2(27.0, 20.0);
const WORDMARK_SIZE: egui::Vec2 = egui::vec2(76.0, 20.0);
/// The tab bar's content height; the pill and the buttons fit inside with room.
const BAR_HEIGHT: f32 = 36.0;
const WINDOW_ICON_PX: u32 = 256;

/// The desktop tile from `assets/`, rasterized for the window manager.
fn window_icon() -> Option<egui::IconData> {
    let svg = include_bytes!("../../../assets/canalis-app-icon.svg");
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(WINDOW_ICON_PX, WINDOW_ICON_PX)?;
    let scale = WINDOW_ICON_PX as f32 / size.width().max(size.height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let p = p.demultiply();
            [p.red(), p.green(), p.blue(), p.alpha()]
        })
        .collect();
    Some(egui::IconData {
        rgba,
        width: WINDOW_ICON_PX,
        height: WINDOW_ICON_PX,
    })
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> std::io::Result<Self> {
        let ctx = cc.egui_ctx.clone();
        let wake = move || ctx.request_repaint();
        let pw = service::Plane::open(wake)?;
        let ctx = cc.egui_ctx.clone();
        packages::start(move || ctx.request_repaint());
        let settings_section = std::env::var("CANALIS_SETTINGS")
            .ok()
            .and_then(|s| views::Section::parse(&s));
        let screenshot: Option<std::path::PathBuf> =
            std::env::var_os("CANALIS_SCREENSHOT").map(Into::into);
        let motion = std::env::var_os("CANALIS_REDUCED_MOTION").is_none();
        // Screenshots skip the intro unless `CANALIS_SCREENSHOT_INTRO=<seconds>` asks for a moment of it.
        let intro_at = std::env::var("CANALIS_SCREENSHOT_INTRO")
            .ok()
            .and_then(|v| v.parse().ok());
        let intro = match screenshot {
            Some(_) => intro_at.and_then(|at| intro::Intro::new(Some(at))),
            None if motion && std::env::var_os(intro::FROM_TRAY).is_none() => {
                intro::Intro::new(None)
            }
            None => None,
        };
        Ok(App {
            view: View::from_env(),
            pw,
            graph: Arc::default(),
            devices_view: views::DevicesView::default(),
            apps_view: views::AppsView::default(),
            graph_view: views::GraphView::restored(),
            chains_view: views::ChainsView::default(),
            pads_view: views::PadsView::default(),
            settings_view: views::SettingsView::at(settings_section.unwrap_or_default()),
            settings_open: settings_section.is_some(),
            package_changes: 0,
            download_changes: 0,
            screenshot,
            frames: 0,
            undo: undo::UndoStack::default(),
            transition: None,
            scene_prompt: None,
            motion,
            intro,
            notice: None,
        })
    }

    fn status(&self) -> String {
        if let Some((notice, since)) = &self.notice
            && since.elapsed() < NOTICE_FOR
        {
            return notice.clone();
        }
        if let Some(waiting) = self.pw.waiting() {
            return waiting.to_owned();
        }
        if !self.graph.connected {
            return t("PipeWire unreachable, retrying").to_owned();
        }
        tf(
            "{nodes} nodes · {ports} ports · {links} links",
            &[
                ("nodes", &self.graph.nodes.len()),
                ("ports", &self.graph.ports.len()),
                ("links", &self.graph.links.len()),
            ],
        )
    }
}

impl App {
    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        self.frames += 1;
        ctx.request_repaint();
        // `CANALIS_SCREENSHOT_SWITCH=<view>` changes tab a few frames before the shot,
        // to catch a transition half way.
        // `CANALIS_SCREENSHOT_FRAMES` waits longer, for audio to reach the meters.
        let after = std::env::var("CANALIS_SCREENSHOT_FRAMES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(SCREENSHOT_AFTER);
        if self.frames == after - 5
            && let Some(target) = std::env::var("CANALIS_SCREENSHOT_SWITCH")
                .ok()
                .and_then(|v| View::parse(&v))
        {
            self.switch_to(target);
        }
        if self.frames != after {
            return;
        }
        let close = ctx.clone();
        ctx.request_screenshot(move |image| {
            let [w, h] = image.size;
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, pixels) {
                Some(rgba) => match rgba.save(&path) {
                    Ok(()) => log::info!("screenshot saved to {}", path.display()),
                    Err(e) => log::warn!("screenshot not saved: {e}"),
                },
                None => log::warn!("screenshot has an unexpected size"),
            }
            close.send_viewport_cmd(egui::ViewportCommand::Close);
            close.request_repaint();
        });
    }
}

/// `CANALIS_SCREENSHOT_CLICK=x,y[;x,y>x2,y2;dbl:x,y;rmb:x,y;text:…]` clicks those points, drags from one
/// to the other or types and presses Enter, ten frames apart, before the screenshot: a
/// way to check a fold, a menu, a drop or a rename without a display tool.
fn screenshot_click(frame: u32, raw_input: &mut egui::RawInput) {
    let Ok(actions) = std::env::var("CANALIS_SCREENSHOT_CLICK") else {
        return;
    };
    let Some(step) = frame.checked_sub(30) else {
        return;
    };
    let Some(action) = actions.split(';').nth((step / 10) as usize) else {
        return;
    };
    if let Some(text) = action.strip_prefix("text:") {
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        match step % 10 {
            0 => raw_input.events.push(egui::Event::Text(text.to_owned())),
            1 => raw_input.events.push(key(true)),
            2 => raw_input.events.push(key(false)),
            _ => {}
        }
        return;
    }
    let point = |text: &str| {
        let (x, y) = text.split_once(',')?;
        Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
    };
    let (double, action) = match action.strip_prefix("dbl:") {
        Some(rest) => (true, rest),
        None => (false, action),
    };
    let (pointer, action) = match action.strip_prefix("rmb:") {
        Some(rest) => (egui::PointerButton::Secondary, rest),
        None => (egui::PointerButton::Primary, action),
    };
    let (Some(from), Some(to)) = (match action.split_once('>') {
        Some((a, b)) => (point(a), point(b)),
        None => (point(action), point(action)),
    }) else {
        return;
    };
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: pointer,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let events = &mut raw_input.events;
    match step % 10 {
        0 => events.push(egui::Event::PointerMoved(from)),
        1 => events.push(button(from, true)),
        2 if double => events.push(button(from, false)),
        3 if double => events.push(button(from, true)),
        4..=7 if double => {}
        n @ 2..=7 => events.push(egui::Event::PointerMoved(
            from.lerp(to, (n - 1) as f32 / 6.0),
        )),
        8 => events.push(button(to, false)),
        _ => {}
    }
}

/// Why nothing is being saved, and the one way out that keeps a copy of the file.
fn config_banner(ui: &mut egui::Ui, problem: &ConfigProblem, commands: &mut Vec<Command>) {
    let (text, button, hover) = match problem {
        ConfigProblem::Unreadable { error, backup } => (
            match backup {
                Some(backup) => tf(
                    "config.toml could not be read, so your settings are not loaded and nothing \
                     is saved. The file is untouched and a copy is at {backup}. Fix it and \
                     restart Canalis, or start fresh.\n{error}",
                    &[
                        ("backup", &backup.display()),
                        ("error", &error.lines().next().unwrap_or_default()),
                    ],
                ),
                None => tf(
                    "config.toml could not be read, so your settings are not loaded and nothing \
                     is saved. The file is untouched. Fix it and restart Canalis, or start \
                     fresh.\n{error}",
                    &[("error", &error.lines().next().unwrap_or_default())],
                ),
            },
            t("Start fresh"),
            t("Save from now on, over the unreadable file").to_owned(),
        ),
        ConfigProblem::Newer { version } => (
            tf(
                "config.toml was written by a newer Canalis (format {version}, this one knows \
                 {known}). Changes are not saved, so its settings stay intact.",
                &[("version", version), ("known", &CONFIG_VERSION)],
            ),
            t("Use anyway"),
            tf(
                "Copy the file to config.toml.v{version}, then save this version's settings",
                &[("version", version)],
            ),
        ),
    };
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("⚠").color(theme::ORANGE).size(16.0));
        if ui.button(button).on_hover_text(hover).clicked() {
            commands.push(Command::ReplaceConfig);
        }
        ui.add(egui::Label::new(egui::RichText::new(text).color(theme::ORANGE)).wrap());
    });
    ui.add_space(6.0);
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.screenshot.is_some() {
            screenshot_click(self.frames, raw_input);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(graph) = self.pw.latest() {
            self.graph = graph;
        }
        if self.pw.stopped() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.pw.take_raise() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.screenshot_step(ui.ctx());
        let mut scene_commands: Vec<Command> = Vec::new();
        egui::Panel::top("tabs").show(ui, |ui| {
            // A row of known height centres everything in it; a plain `horizontal` only
            // knows its height after the fact and leaves the children top-aligned.
            let row = egui::vec2(ui.available_width(), BAR_HEIGHT);
            let layout = egui::Layout::left_to_right(egui::Align::Center);
            ui.allocate_ui_with_layout(row, layout, |ui| {
                ui.add(
                    egui::Image::new(egui::include_image!("../../../assets/canalis-icon.svg"))
                        .fit_to_exact_size(MARK_SIZE),
                );
                ui.add(
                    egui::Image::new(egui::include_image!("../../../assets/canalis-wordmark.svg"))
                        .fit_to_exact_size(WORDMARK_SIZE),
                )
                .on_hover_text(concat!("Canalis ", env!("CARGO_PKG_VERSION")));
                ui.separator();
                ui.add_space(10.0);
                ui.scope(|ui| self.tabs(ui));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let gear = ui
                        .add(
                            egui::Button::new(egui::RichText::new("⚙").size(15.0))
                                .fill(if self.settings_open {
                                    theme::SELECTION
                                } else {
                                    theme::PANEL
                                })
                                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                                .min_size(egui::vec2(28.0, 26.0)),
                        )
                        .on_hover_text(if self.settings_open {
                            t("close settings (Esc)")
                        } else {
                            t("settings")
                        });
                    if gear.clicked() {
                        self.settings_open = !self.settings_open;
                    }
                    ui.add_space(8.0);
                    self.scene_menu(ui, &mut scene_commands);
                    ui.add_space(8.0);
                    let history = |label: &str| {
                        egui::Button::new(egui::RichText::new(label).size(14.0))
                            .fill(theme::PANEL)
                            .stroke(egui::Stroke::new(1.0, theme::BORDER))
                            .min_size(egui::vec2(28.0, 26.0))
                    };
                    let redo = ui
                        .add_enabled(self.undo.can_redo(), history("↷"))
                        .on_hover_text(t("redo (Ctrl+Shift+Z)"));
                    let undo = ui
                        .add_enabled(self.undo.can_undo(), history("↶"))
                        .on_hover_text(t(
                            "undo the last change to routing, a chain or the Graph layout (Ctrl+Z)",
                        ));
                    ui.add_space(4.0);
                    ui.separator();
                    if undo.clicked() {
                        self.history_step(false);
                    }
                    if redo.clicked() {
                        self.history_step(true);
                    }
                });
            });
        });
        if !ui.ctx().egui_wants_keyboard_input() {
            let (undo, redo) = ui.input(|i| {
                let z = i.key_pressed(egui::Key::Z) && i.modifiers.command;
                (z && !i.modifiers.shift, z && i.modifiers.shift)
            });
            if undo {
                self.history_step(false);
            }
            if redo {
                self.history_step(true);
            }
        }
        let mut commands: Vec<Command> = scene_commands;
        if self.view == View::Pads && !self.graph.soundboard.enabled && self.graph.connected {
            self.view = View::Devices;
        }
        self.install_dropped(ui.ctx(), &mut commands);
        if self.settings_open
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.settings_open = false;
        }
        if let Some(kit) = packages::get() {
            let changes = kit.snapshot().changes;
            if changes != self.package_changes {
                self.package_changes = changes;
                commands.push(Command::ScanPlugins);
            }
        }
        let downloads = plugin_install::fetching().changes;
        if downloads != self.download_changes {
            self.download_changes = downloads;
            commands.push(Command::ScanPlugins);
        }
        if let Some(problem) = &self.graph.config_problem {
            egui::Panel::top("config_problem")
                .show(ui, |ui| config_banner(ui, problem, &mut commands));
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(theme::small_dim(self.status()));
            });
        });
        egui::CentralPanel::default().show(ui, |ui| self.content(ui, &mut commands));
        self.scene_prompt_ui(ui, &mut commands);
        if let Some(intro) = &mut self.intro
            && !intro.show(ui.ctx(), self.graph.connected)
        {
            self.intro = None;
        }
        for command in commands {
            self.undo.record(&self.graph, &command);
            self.pw.send(command);
        }
    }
}

impl App {
    /// Plugins dropped on the window go into the user's plugin folders, then a rescan; a
    /// shared chain file is imported.
    fn install_dropped(&mut self, ctx: &egui::Context, commands: &mut Vec<Command>) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        let (sounds, dropped): (Vec<_>, Vec<_>) = dropped
            .into_iter()
            .partition(|p| self.view == View::Pads && views::is_sound(p));
        if !sounds.is_empty() {
            match self.pads_view.drop_files(ctx, &sounds, &self.graph) {
                Some(command) => commands.push(command),
                None => {
                    self.notice = Some((
                        t("every pad is taken; remove one first").to_owned(),
                        std::time::Instant::now(),
                    ));
                }
            }
        }
        let mut installed = Vec::new();
        for path in dropped {
            if views::share::is_shared_chain(&path) {
                let imported = views::share::import_file(&path, &self.graph);
                let text = match imported {
                    Ok(imported) => {
                        let summary = imported.summary();
                        commands.extend(imported.commands);
                        summary
                    }
                    Err(e) => format!("{}: {e}", path.display()),
                };
                self.notice = Some((text, std::time::Instant::now()));
                continue;
            }
            match plugin_install::install(&path) {
                Ok(target) => installed.push(target.display().to_string()),
                Err(e) => {
                    log::warn!("{e}");
                    self.notice = Some((e, std::time::Instant::now()));
                }
            }
        }
        if !installed.is_empty() {
            let text = tf(
                "installed {plugins}; rescanning plugins",
                &[("plugins", &installed.join(", "))],
            );
            self.notice = Some((text, std::time::Instant::now()));
            commands.push(Command::ScanPlugins);
        }
    }

    /// "Scene ▾": recall a saved routing state, delete one, or save the current one.
    fn scene_menu(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
        let button = egui::Button::new(egui::RichText::new(t("Scene ▾")).size(12.0))
            .fill(theme::PANEL)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .min_size(egui::vec2(0.0, 26.0));
        let scenes = self.graph.scenes.clone();
        let mut open_prompt = false;
        egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
            ui.set_min_width(200.0);
            if scenes.is_empty() {
                ui.label(theme::small_dim(t("no scenes yet")));
            }
            for scene in &scenes {
                ui.horizontal(|ui| {
                    if ui
                        .button(&scene.name)
                        .on_hover_text(t("recall: only what differs is changed"))
                        .clicked()
                    {
                        commands.push(Command::ApplyScene {
                            scene: scene.clone(),
                        });
                        ui.close();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(theme::small_dim("×")).frame(false))
                            .on_hover_text(t("delete this scene"))
                            .clicked()
                        {
                            commands.push(Command::DeleteScene {
                                name: scene.name.clone(),
                            });
                            ui.close();
                        }
                        scene_trigger_menu(ui, scene, &self.graph, commands);
                        if ui
                            .add(egui::Button::new(theme::small_dim(t("learn"))).frame(false))
                            .on_hover_text(t("map a MIDI message to this scene"))
                            .clicked()
                        {
                            commands.push(Command::Learn {
                                target: Some(canalis_pw::ControlTarget::Scene {
                                    name: scene.name.clone(),
                                }),
                            });
                            ui.close();
                        }
                    });
                });
            }
            ui.separator();
            if ui.button(t("Save current as…")).clicked() {
                open_prompt = true;
                ui.close();
            }
        });
        if open_prompt {
            self.scene_prompt = Some(String::new());
        }
    }

    fn scene_prompt_ui(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
        let Some(name) = &mut self.scene_prompt else {
            return;
        };
        let mut done = false;
        let modal = egui::Modal::new(ui.scope_id().with("save-scene")).show(ui.ctx(), |ui| {
            ui.set_width(320.0);
            ui.label(egui::RichText::new(t("Save scene")).strong());
            ui.label(theme::small_dim(t(
                "defaults, app targets, chain ends, mutes and bypasses, as they are now",
            )));
            ui.add_space(6.0);
            let edit = ui.text_edit_singleline(name);
            views::autofocus(ui, &edit);
            let valid = !name.trim().is_empty();
            let submitted = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(valid, egui::Button::new(t("Save")))
                    .clicked()
                    || (submitted && valid)
                {
                    commands.push(Command::SaveScene {
                        scene: canalis_pw::Scene::capture(name.trim(), &self.graph),
                    });
                    done = true;
                }
                if ui.button(t("Cancel")).clicked() {
                    done = true;
                }
            });
        });
        if done || modal.should_close() {
            self.scene_prompt = None;
        }
    }

    /// The tab buttons with a highlight that slides to the chosen one.
    fn tabs(&mut self, ui: &mut egui::Ui) {
        // Frameless buttons hug their text; the pill gets its padding here and the
        // spacing leaves room for it.
        ui.spacing_mut().item_spacing.x = 2.0 * TAB_PADDING.x + 6.0;
        ui.add_space(TAB_PADDING.x);
        let pill = ui.painter().add(egui::Shape::Noop);
        let mut active_rect = None;
        for view in View::ALL {
            if view == View::Pads && !self.graph.soundboard.enabled {
                continue;
            }
            let selected = view == self.view && !self.settings_open;
            let response = ui.add(
                egui::Button::new(egui::RichText::new(view.title()).color(if selected {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                }))
                .frame(false)
                .min_size(egui::vec2(0.0, 24.0)),
            );
            if selected {
                active_rect = Some(response.rect);
            }
            if response.clicked() && !selected {
                if self.settings_open && view == self.view {
                    self.settings_open = false;
                } else {
                    self.settings_open = false;
                    self.switch_to(view);
                }
            }
        }
        if let Some(target) = active_rect {
            let ctx = ui.ctx().clone();
            let slide = if self.motion { TAB_SLIDE } else { 0.0 };
            let left =
                ctx.animate_value_with_time(ui.scope_id().with("tab-left"), target.left(), slide);
            let right =
                ctx.animate_value_with_time(ui.scope_id().with("tab-right"), target.right(), slide);
            let rect = egui::Rect::from_min_max(
                egui::pos2(left, target.top()),
                egui::pos2(right, target.bottom()),
            )
            .expand2(TAB_PADDING);
            ui.painter().set(
                pill,
                egui::Shape::rect_filled(rect, theme::CORNER_RADIUS, theme::SELECTION),
            );
        }
    }

    fn switch_to(&mut self, view: View) {
        let from_index = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
        let to_index = View::ALL.iter().position(|v| *v == view).unwrap_or(0);
        self.transition = self.motion.then(|| Transition {
            from: self.view,
            started: std::time::Instant::now(),
            direction: if to_index > from_index { 1.0 } else { -1.0 },
        });
        self.view = view;
    }

    /// The current view, or both views sliding during a tab change.
    fn content(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
        if self.settings_open {
            self.settings_view.page(ui, &self.graph, commands);
            return;
        }
        let Some(transition) = &self.transition else {
            let view = self.view;
            self.view_ui(view, ui, commands);
            return;
        };
        let t = transition.started.elapsed().as_secs_f32() / TRANSITION.as_secs_f32();
        if t >= 1.0 {
            self.transition = None;
            let view = self.view;
            self.view_ui(view, ui, commands);
            return;
        }
        let (from, direction) = (transition.from, transition.direction);
        let eased = ease(t.clamp(0.0, 1.0));
        let rect = ui.max_rect();
        let width = rect.width();
        let outgoing = rect.translate(egui::vec2(-direction * eased * width, 0.0));
        let incoming = rect.translate(egui::vec2(direction * (1.0 - eased) * width, 0.0));
        let mut leaving =
            ui.new_child(egui::UiBuilder::new().max_rect(outgoing).id_salt("leaving"));
        leaving.set_clip_rect(rect);
        leaving.set_opacity(1.0 - eased);
        self.view_ui(from, &mut leaving, &mut Vec::new());
        let mut entering = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(incoming)
                .id_salt("entering"),
        );
        entering.set_clip_rect(rect);
        entering.set_opacity(eased);
        let view = self.view;
        self.view_ui(view, &mut entering, commands);
        ui.ctx().request_repaint();
    }

    fn view_ui(&mut self, view: View, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
        match view {
            View::Devices => self.devices_view.show(ui, &self.graph, commands),
            View::Apps => self.apps_view.show(ui, &self.graph, commands),
            View::Graph => {
                self.graph_view.show(ui, &self.graph, commands);
                if let Some((before, after)) = self.graph_view.take_layout_change() {
                    self.undo.record_layout(before, after);
                }
                if let Some(opens) = self.graph_view.take_open() {
                    self.switch_to(match opens {
                        views::Opens::Devices => View::Devices,
                        views::Opens::Apps => View::Apps,
                        views::Opens::Chains => View::Chains,
                    });
                }
            }
            View::Pads => self.pads_view.show(ui, &self.graph, commands),
            View::Chains => {
                self.chains_view.show(ui, &self.graph, commands);
                if let Some(text) = self.chains_view.take_notice() {
                    self.notice = Some((text, std::time::Instant::now()));
                }
                if self.chains_view.take_open_plugins() {
                    self.settings_view.show_section(views::Section::Plugins);
                    self.settings_open = true;
                }
            }
        }
    }

    /// Sends the undo or redo of the last routing change, if there is one.
    fn history_step(&mut self, redo: bool) {
        let step = if redo {
            self.undo.redo(&self.graph)
        } else {
            self.undo.undo(&self.graph)
        };
        match step {
            Some(undo::Step::Command(command)) => self.pw.send(command),
            Some(undo::Step::Layout(layout)) => self.graph_view.set_layout(layout),
            None => {}
        }
    }
}

/// "when…" on a scene: recall it by itself when a device appears or an app starts.
fn scene_trigger_menu(
    ui: &mut egui::Ui,
    scene: &canalis_pw::Scene,
    graph: &Graph,
    commands: &mut Vec<Command>,
) {
    use canalis_pw::{MediaClass, SceneTrigger};
    let device_label = |name: &str| {
        graph
            .node_by_name(name)
            .map_or_else(|| name.to_owned(), |n| n.label.clone())
    };
    let face = match &scene.when {
        Some(SceneTrigger::Device { name }) => {
            tf("auto: {trigger}", &[("trigger", &device_label(name))])
        }
        Some(SceneTrigger::App { name }) => tf("auto: {trigger}", &[("trigger", name)]),
        None => t("when…").to_owned(),
    };
    let mut set = |when: Option<SceneTrigger>| {
        commands.push(Command::SetSceneTrigger {
            name: scene.name.clone(),
            when,
        });
    };
    egui::containers::menu::MenuButton::from_button(
        egui::Button::new(theme::small_dim(face)).frame(false),
    )
    .ui(ui, |ui| {
        if ui
            .selectable_label(scene.when.is_none(), t("Only by hand"))
            .clicked()
        {
            set(None);
            ui.close();
        }
        ui.menu_button(t("When a device appears"), |ui| {
            let devices = graph.nodes.iter().filter(|n| {
                matches!(n.class, MediaClass::Sink | MediaClass::Source)
                    && !n.is_virtual_device()
                    && !graph.is_hidden(&n.name)
            });
            for node in devices {
                let trigger = SceneTrigger::Device {
                    name: node.name.clone(),
                };
                if ui
                    .selectable_label(scene.when.as_ref() == Some(&trigger), &node.label)
                    .on_hover_text(t(
                        "recalls the scene each time it is plugged in or switched on",
                    ))
                    .clicked()
                {
                    set(Some(trigger));
                    ui.close();
                }
            }
        });
        ui.menu_button(t("When an app starts"), |ui| {
            let mut apps: Vec<&str> = graph
                .nodes
                .iter()
                .filter_map(|n| n.app.as_deref())
                .chain(graph.apps.keys().map(String::as_str))
                .collect();
            apps.sort_unstable();
            apps.dedup();
            if apps.is_empty() {
                ui.label(theme::small_dim(t(
                    "start the app once so Canalis knows it",
                )));
            }
            for app in apps {
                let trigger = SceneTrigger::App {
                    name: app.to_owned(),
                };
                if ui
                    .selectable_label(scene.when.as_ref() == Some(&trigger), app)
                    .on_hover_text(t(
                        "recalls the scene each time the app starts playing or recording",
                    ))
                    .clicked()
                {
                    set(Some(trigger));
                    ui.close();
                }
            }
        });
    });
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,symphonia=warn"),
    )
    .init();
    i18n::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headless = match args.first().map(String::as_str) {
        Some("--daemon") => Some(service::daemon()),
        Some("--install") => Some(service::install()),
        Some("--uninstall") => Some(service::uninstall()),
        Some("--version") => {
            println!("canalis {}", env!("CARGO_PKG_VERSION"));
            Some(Ok(()))
        }
        Some(_) => Some(service::cli(&args)),
        None => None,
    };
    if let Some(result) = headless {
        if let Err(e) = result {
            eprintln!("canalis: {e}");
            let usage = e.kind() == std::io::ErrorKind::InvalidInput;
            std::process::exit(if usage { 2 } else { 1 });
        }
        return Ok(());
    }
    if service::raise_open_window() {
        log::info!("Canalis is already open; raised its window");
        return Ok(());
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Canalis")
        .with_app_id(service::APP_ID)
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([720.0, 480.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "canalis",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(App::new(cc)?))
        }),
    )
}
