//! Settings: a page of its own behind the gear, with a sidebar of sections and grouped
//! rows (what it is, one line on what it does, the control on the right), the layout
//! GNOME Settings and most desktop apps use, so new sections slot in without crowding.

use std::collections::BTreeMap;

use canalis_pw::{Command, ControlTrigger, Graph, MIDI_INPUT_NAME, MediaClass, Node};
use egui::{Align, Button, Color32, Frame, Layout, Margin, RichText, Stroke, Ui, vec2};

use crate::i18n::{self, LANGUAGES, t, tf};
use crate::packages::{self, Backend, FAMILIES, Family};
use crate::plugin_install::{self, DOWNLOADS};
use crate::service::{self, ServiceState};
use crate::theme;

/// How often the systemd state is asked for while General is shown.
const SERVICE_POLL: std::time::Duration = std::time::Duration::from_secs(3);
const SIDEBAR_WIDTH: f32 = 196.0;
const PAGE_MAX_WIDTH: f32 = 720.0;
const ROW_CONTROL_WIDTH: f32 = 230.0;
const GROUP_RADIUS: u8 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    General,
    Plugins,
    Midi,
    About,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::General,
        Section::Plugins,
        Section::Midi,
        Section::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Plugins => "Plugins",
            Section::Midi => "MIDI control",
            Section::About => "About",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Section::General => t("General"),
            Section::Plugins => t("Plugins"),
            Section::Midi => t("MIDI control"),
            Section::About => t("About"),
        }
    }

    fn about(self) -> &'static str {
        match self {
            Section::General => t("how Canalis starts and where it lives on your desktop"),
            Section::Plugins => t("the CLAP and LV2 plugins your chains can use"),
            Section::Midi => t("a controller for mutes, bypasses, levels and scenes"),
            Section::About => t("version, files and who made it"),
        }
    }

    /// `CANALIS_SETTINGS=<section>` opens the page there, for screenshots.
    pub fn parse(name: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| {
            s.title()
                .split_whitespace()
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case(name))
        })
    }
}

/// Where a plugin came from, which decides how it can be removed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    /// Shipped with Canalis (ADR 0007): not removable.
    Bundled(String),
    /// A file or bundle in `~/.clap` or `~/.lv2`: removing deletes it.
    Folder(String),
    /// Installed by a system package: removing goes through PackageKit.
    Package { name: String, id: String },
    /// Somewhere else on the system, owner unknown.
    System(String),
}

/// What the confirmation dialog would remove.
#[derive(Clone, Debug)]
enum Removal {
    Folder {
        path: String,
        plugins: usize,
    },
    /// A source's package, or every installed package of a family.
    Package {
        name: String,
        ids: Vec<String>,
        /// How many catalogue plugins it carries, when known.
        plugins: Option<usize>,
    },
}

#[derive(Default)]
pub struct SettingsView {
    section: Section,
    service: ServiceState,
    service_checked: Option<std::time::Instant>,
    query: String,
    /// The plugin paths whose owners were asked for, so they are asked once per catalogue.
    owners_asked: Vec<String>,
    removal: Option<Removal>,
    /// The last folder removal that failed, to say why.
    folder_error: Option<String>,
}

impl SettingsView {
    pub fn at(section: Section) -> SettingsView {
        SettingsView {
            section,
            ..Default::default()
        }
    }

    pub fn show_section(&mut self, section: Section) {
        self.section = section;
    }

    pub fn page(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            self.sidebar(ui);
            ui.add_space(1.0);
            let rect = ui.available_rect_before_wrap();
            ui.painter().vline(
                rect.left() - 1.0,
                rect.y_range(),
                Stroke::new(1.0, theme::BORDER_SOFT),
            );
            egui::ScrollArea::vertical()
                .id_salt(("settings", self.section.title()))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let width = (ui.available_width() - 64.0).min(PAGE_MAX_WIDTH);
                    let left = ((ui.available_width() - width) / 2.0).max(32.0);
                    ui.add_space(24.0);
                    ui.horizontal_top(|ui| {
                        ui.add_space(left);
                        ui.vertical(|ui| {
                            ui.set_width(width);
                            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                            ui.label(RichText::new(self.section.label()).size(20.0).strong());
                            ui.label(theme::small_dim(self.section.about()));
                            ui.add_space(12.0);
                            match self.section {
                                Section::General => self.general(ui, graph, commands),
                                Section::Plugins => self.plugins(ui, graph, commands),
                                Section::Midi => midi(ui, graph, commands),
                                Section::About => about(ui),
                            }
                            ui.add_space(32.0);
                        });
                    });
                });
        });
        self.removal_dialog(ui, commands);
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.vertical(|ui| {
            ui.set_width(SIDEBAR_WIDTH);
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(t("Settings"))
                        .size(17.0)
                        .strong()
                        .color(theme::TEXT),
                );
            });
            ui.add_space(14.0);
            for section in Section::ALL {
                let selected = section == self.section;
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let (rect, item) = ui.allocate_exact_size(
                        vec2(SIDEBAR_WIDTH - 16.0, 32.0),
                        egui::Sense::click(),
                    );
                    let fill = if selected {
                        theme::SELECTION
                    } else if item.hovered() {
                        theme::PANEL
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, GROUP_RADIUS, fill);
                    ui.painter().text(
                        rect.left_center() + vec2(12.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        section.label(),
                        egui::FontId::proportional(13.5),
                        if selected {
                            theme::TEXT
                        } else {
                            theme::TEXT_DIM
                        },
                    );
                    if item.clicked() {
                        self.section = section;
                    }
                });
            }
        });
    }

    fn general(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        if self
            .service_checked
            .is_none_or(|t| t.elapsed() > SERVICE_POLL)
        {
            self.service = service::state();
            self.service_checked = Some(std::time::Instant::now());
        }
        group(ui, Some(t("Language")), |g| {
            g.row(
                t("Interface language"),
                t("the language of this window and the tray menu"),
                language_picker,
            );
        });
        group(ui, Some(t("Features")), |g| {
            g.row(
                t("Soundboard"),
                t("a Pads tab of sounds you play into calls and streams, by click, key or MIDI controller"),
                |ui| {
                    let mut enabled = graph.soundboard.enabled;
                    if toggle(ui, &mut enabled).changed() {
                        commands.push(Command::SetSoundboard {
                            board: switched(graph, enabled),
                        });
                    }
                },
            );
        });
        group(ui, Some(t("Notifications")), |g| {
            let current = graph.notifications;
            type Field = fn(&mut canalis_pw::Notifications) -> &mut bool;
            let rows: [(&str, &str, Field); 3] = [
                (
                    t("Crashes"),
                    t(
                        "a chain crashed and restarted bypassed, or was stopped after crashing again and again",
                    ),
                    |n| &mut n.crashes,
                ),
                (
                    t("Clipping"),
                    t("a chain's output keeps hitting full scale"),
                    |n| &mut n.clipping,
                ),
                (
                    t("Lost devices"),
                    t("a device a running chain plays into or listens to went away"),
                    |n| &mut n.lost_devices,
                ),
            ];
            for (title, help, field) in rows {
                g.row(title, help, |ui| {
                    let mut next = current;
                    if toggle(ui, field(&mut next)).changed() {
                        commands.push(Command::SetNotifications {
                            notifications: next,
                        });
                    }
                });
            }
        });
        group(ui, Some(t("Startup")), |g| {
            g.row(
                t("Run at login"),
                t("start the chains when you log in, before any window opens"),
                |ui| {
                    let mut enabled = self.service.enabled;
                    if toggle(ui, &mut enabled).changed() {
                        service::set_enabled(enabled);
                        self.service_checked = None;
                    }
                },
            );
            g.row(
                t("Background service"),
                t("runs the chains; closing this window leaves them running. \
                 Quit it from the tray icon or with canalis quit"),
                |ui| {
                    ui.label(RichText::new(t("running")).size(12.5).color(theme::GREEN));
                },
            );
        });
        // The Flatpak exports its own launcher entry.
        if service::in_flatpak() {
            return;
        }
        group(ui, Some(t("Desktop")), |g| {
            if self.service.installed {
                g.row(
                    t("App launcher entry"),
                    t("Canalis is in your desktop's app list"),
                    |ui| {
                        if outline(ui, t("Remove"))
                            .on_hover_text(t("removes the launcher entry and the login start; \
                                 the running service stays until you quit it"))
                            .clicked()
                        {
                            if let Err(e) = service::uninstall() {
                                log::warn!("uninstall failed: {e}");
                            }
                            self.service_checked = None;
                        }
                    },
                );
            } else {
                g.row(
                    t("App launcher entry"),
                    t("put Canalis in your desktop's app list, with its icon"),
                    |ui| {
                        if outline(ui, t("Add")).clicked() {
                            if let Err(e) = service::install() {
                                log::warn!("install failed: {e}");
                            }
                            self.service_checked = None;
                        }
                    },
                );
            }
        });
    }

    fn plugins(&mut self, ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
        let kit = packages::get().map(|p| p.snapshot());
        let paths = plugin_paths(graph);
        if let Some(kit_handle) = packages::get()
            && !graph.scanning
            && paths != self.owners_asked
        {
            self.owners_asked = paths.clone();
            kit_handle.find_owners(paths);
        }
        let clap = graph.plugins.iter().filter(|p| p.format == "clap").count();
        let lv2 = graph.plugins.len() - clap;
        group(ui, Some(t("Library")), |g| {
            g.row(
                t("Plugins found"),
                &if graph.scanning {
                    t("scanning…").to_owned()
                } else {
                    tf(
                        "{clap} CLAP and {lv2} LV2, the Claritas pair that comes with Canalis among them",
                        &[("clap", &clap), ("lv2", &lv2)],
                    )
                },
                |ui| {
                    if ui
                        .add_enabled(!graph.scanning, outline_button(t("Rescan")))
                        .on_hover_text(t("look through the plugin folders again"))
                        .clicked()
                    {
                        commands.push(Command::ScanPlugins);
                    }
                },
            );
            g.row(
                t("Your plugin folders"),
                t(
                    "drop a .clap file or an .lv2 folder on this window to copy it into \
                 ~/.clap or ~/.lv2 for you",
                ),
                |ui| {
                    // Right to left: the LV2 button lands right of the CLAP one.
                    if outline(ui, t("Open ~/.lv2")).clicked() {
                        crate::plugin_install::open_folder(".lv2");
                    }
                    if outline(ui, t("Open ~/.clap")).clicked() {
                        crate::plugin_install::open_folder(".clap");
                    }
                },
            );
            if let Some(why) = &graph.lv2_off {
                g.row(t("LV2 is off"), why, |ui| {
                    let support = FAMILIES.iter().find(|f| f.lv2_support);
                    let installable = kit
                        .as_ref()
                        .filter(|k| matches!(k.backend, Backend::Ready))
                        .zip(support)
                        .and_then(|(k, f)| Some((k, f, k.families.get(f.name)?)))
                        .filter(|(_, _, state)| state.carried());
                    match installable {
                        Some((kit, family, state)) => {
                            install_control(ui, kit, family, state, None);
                        }
                        None => {
                            ui.label(RichText::new(t("off")).size(12.5).color(theme::ORANGE));
                        }
                    }
                });
            }
            if let Some(confinement) = crate::cpu::confinement() {
                g.row(t("Efficiency cores only"), &confinement.explain(), |ui| {
                    ui.label(RichText::new(t("slow")).size(12.5).color(theme::ORANGE));
                });
            }
        });
        group(ui, Some(t("Get more plugins")), |g| {
            // The LV2 is off row above offers lilv and suil already.
            get_more(
                g,
                kit.as_ref(),
                graph.lv2_off.is_some(),
                false,
                Some(&mut self.removal),
            );
        });
        self.installed(ui, graph, kit.as_ref());
    }

    /// The catalogue by where each plugin came from, searchable, with Remove.
    fn installed(&mut self, ui: &mut Ui, graph: &Graph, kit: Option<&packages::Snapshot>) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("Installed"))
                    .size(13.0)
                    .color(theme::TEXT_DIM),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text(t("search name, vendor or id"))
                        .desired_width(240.0),
                );
            });
        });
        let query = self.query.to_lowercase();
        let home = std::env::var("HOME").unwrap_or_default();
        let owners = kit.map(|k| &k.owners);
        let bundled = std::env::current_exe().ok().and_then(|exe| {
            Some(
                exe.parent()?
                    .parent()?
                    .join("lib/clap")
                    .display()
                    .to_string(),
            )
        });
        let mut sources: BTreeMap<Source, Vec<&canalis_pw::PluginEntry>> = BTreeMap::new();
        for plugin in &graph.plugins {
            let path = plugin.path.trim_end_matches('/').to_owned();
            let source = if bundled
                .as_ref()
                .is_some_and(|d| path.starts_with(d.as_str()))
            {
                Source::Bundled(path)
            } else if user_folder(&path, &home) {
                Source::Folder(path)
            } else if let Some(owner) = owners.and_then(|o| o.get(&path)) {
                Source::Package {
                    name: owner.name().to_owned(),
                    id: owner.id.clone(),
                }
            } else {
                Source::System(path)
            };
            sources.entry(source).or_default().push(plugin);
        }
        let busy = kit.is_some_and(|k| k.busy.is_some());
        let can_remove_packages = kit.is_some_and(|k| matches!(k.backend, Backend::Ready));
        group(ui, None, |g| {
            for (source, plugins) in &sources {
                let matching: Vec<&&canalis_pw::PluginEntry> = plugins
                    .iter()
                    .filter(|p| {
                        query.is_empty()
                            || p.name.to_lowercase().contains(&query)
                            || p.vendor.to_lowercase().contains(&query)
                            || p.id.to_lowercase().contains(&query)
                    })
                    .collect();
                if matching.is_empty() {
                    continue;
                }
                let formats = format_list(plugins);
                let (title, about) = match source {
                    Source::Bundled(path) => (
                        file_name(path),
                        tf(
                            "{count} · {formats} · comes with Canalis",
                            &[("count", &count(plugins.len())), ("formats", &formats)],
                        ),
                    ),
                    Source::Folder(path) => (
                        file_name(path),
                        format!(
                            "{} · {formats} · {}",
                            count(plugins.len()),
                            tilde(path, &home)
                        ),
                    ),
                    Source::Package { name, .. } => (
                        name.clone(),
                        tf(
                            "{count} · {formats} · system package",
                            &[("count", &count(plugins.len())), ("formats", &formats)],
                        ),
                    ),
                    Source::System(path) => (
                        file_name(path),
                        format!("{} · {formats} · {path}", count(plugins.len())),
                    ),
                };
                let id = ui_id(g.ui, source);
                g.expandable(
                    id,
                    &title,
                    &about,
                    !query.is_empty(),
                    |ui| match source {
                        Source::Folder(path) => {
                            if outline(ui, t("Remove")).clicked() {
                                self.removal = Some(Removal::Folder {
                                    path: path.clone(),
                                    plugins: plugins.len(),
                                });
                            }
                        }
                        Source::Package { name, id } => {
                            if ui
                                .add_enabled(
                                    can_remove_packages && !busy,
                                    outline_button(t("Uninstall")),
                                )
                                .clicked()
                            {
                                self.removal = Some(Removal::Package {
                                    name: name.clone(),
                                    ids: vec![id.clone()],
                                    plugins: Some(plugins.len()),
                                });
                            }
                        }
                        Source::System(_) => {
                            ui.label(theme::small_dim(t("installed by the system")));
                        }
                        Source::Bundled(_) => {
                            ui.label(theme::small_dim(t("comes with Canalis")));
                        }
                    },
                    |ui| {
                        for plugin in &matching {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&plugin.name).size(12.5));
                                if !plugin.vendor.is_empty() {
                                    ui.label(theme::small_dim(&plugin.vendor));
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(theme::small_dim(plugin.format.to_uppercase()));
                                });
                            });
                        }
                    },
                );
            }
        });
        if let Some(error) = &self.folder_error {
            ui.label(RichText::new(error).size(12.0).color(theme::RED_SOFT));
        }
    }

    fn removal_dialog(&mut self, ui: &mut Ui, commands: &mut Vec<Command>) {
        let Some(removal) = self.removal.clone() else {
            return;
        };
        let (title, body, action) = match &removal {
            Removal::Folder { path, plugins } => (
                tf("Remove {name}?", &[("name", &file_name(path))]),
                tf(
                    "Deletes {path} ({plugins}). Chains that use it show the slot as missing.",
                    &[("path", path), ("plugins", &count(*plugins))],
                ),
                t("Remove"),
            ),
            Removal::Package { name, ids, plugins } => {
                let packages = ids
                    .iter()
                    .map(|id| id.split(';').next().unwrap_or(id))
                    .collect::<Vec<_>>()
                    .join(", ");
                let body = match plugins {
                    Some(n) => tf(
                        "Uninstalls {packages} for every user of this computer ({plugins}). You \
                         may be asked for your password. Chains that use it show the slot as \
                         missing.",
                        &[("packages", &packages), ("plugins", &count(*n))],
                    ),
                    None => tf(
                        "Uninstalls {packages} for every user of this computer. You may be \
                         asked for your password. Chains that use it show the slot as missing.",
                        &[("packages", &packages)],
                    ),
                };
                (
                    tf("Uninstall {name}?", &[("name", name)]),
                    body,
                    t("Uninstall"),
                )
            }
        };
        let mut close = false;
        let modal = egui::Modal::new(ui.scope_id().with("plugin-removal")).show(ui.ctx(), |ui| {
            ui.set_width(380.0);
            ui.label(RichText::new(title).size(15.0).strong());
            ui.add_space(4.0);
            ui.label(RichText::new(body).size(12.5).color(theme::TEXT_DIM));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let danger = Button::new(RichText::new(action).color(theme::RED_SOFT))
                        .fill(theme::RED_FILL)
                        .stroke(Stroke::new(1.0, theme::RED))
                        .min_size(vec2(0.0, 30.0));
                    if ui.add(danger).clicked() {
                        match &removal {
                            Removal::Folder { path, .. } => {
                                self.folder_error = crate::plugin_install::remove(path).err();
                                commands.push(Command::ScanPlugins);
                            }
                            Removal::Package { name, ids, .. } => {
                                if let Some(kit) = packages::get() {
                                    kit.remove(name.clone(), ids.clone());
                                }
                            }
                        }
                        close = true;
                    }
                    if outline(ui, t("Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        });
        if close || modal.should_close() {
            self.removal = None;
        }
    }
}

/// The families this system can install, as rows: Install, Installed or the command.
/// The families this system can install, as rows: Install, or installed with Remove when
/// `removal` can take the request (the settings page; the picker has no dialog).
fn get_more(
    g: &mut Group<'_>,
    kit: Option<&packages::Snapshot>,
    lv2_off: bool,
    lv2_support_row: bool,
    mut removal: Option<&mut Option<Removal>>,
) {
    downloads(g, lv2_off);
    let shown = FAMILIES
        .iter()
        .filter(|f| !f.lv2_support || (lv2_off && lv2_support_row));
    match kit.map(|k| &k.backend) {
        Some(Backend::Ready) => {
            let kit = kit.expect("matched Some");
            let mut any = false;
            for family in shown {
                let Some(state) = kit.families.get(family.name).filter(|s| s.carried()) else {
                    continue;
                };
                any = true;
                g.row(family.label(), family.blurb(), |ui| {
                    install_control(ui, kit, family, state, removal.as_deref_mut());
                });
            }
            if !any {
                g.note(t("your distribution's repositories carry none of the plugin families Canalis knows; drop plugin files on the window instead"));
            }
            if let Some(busy) = &kit.busy {
                g.note_colored(busy, theme::ACCENT);
            }
            if let Some(error) = &kit.error {
                g.note_colored(error, theme::RED_SOFT);
            }
        }
        Some(Backend::Checking) | None => g.note(t("asking PackageKit what your system has…")),
        Some(Backend::Off(why)) => {
            g.note(why);
            for family in shown {
                if let Some(command) = packages::command_for(family) {
                    // Under the blurb: a flatpak or apt line is wider than the control column.
                    let about = format!("{}\n{command}", family.blurb());
                    g.row(family.label(), &about, |ui| {
                        if ui
                            .small_button(t("copy"))
                            .on_hover_text(command.as_str())
                            .clicked()
                        {
                            ui.ctx().copy_text(command.clone());
                        }
                    });
                }
            }
        }
    }
}

/// Plugins no distribution carries, downloaded into `~/.lv2`; not inside the Flatpak,
/// whose sandbox has its own plugin folders.
fn downloads(g: &mut Group<'_>, lv2_off: bool) {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return;
    }
    let state = plugin_install::fetching();
    for download in DOWNLOADS {
        g.row(download.label(), download.blurb(), |ui| {
            if state.busy == Some(download.name) {
                ui.add(egui::Spinner::new().size(16.0));
            } else if plugin_install::downloaded(download) {
                ui.label(
                    RichText::new(t("✓ installed"))
                        .size(12.5)
                        .color(theme::GREEN),
                );
            } else if lv2_off {
                ui.label(
                    RichText::new(t("needs LV2"))
                        .size(12.5)
                        .color(theme::ORANGE),
                )
                .on_hover_text(t("turn LV2 on first: Settings › Plugins, LV2 is off"));
            } else if ui
                .add_enabled(state.busy.is_none(), outline_button(t("Download")))
                .on_hover_text(t(
                    "downloads the release from GitHub into ~/.lv2; no password",
                ))
                .clicked()
            {
                let ctx = ui.ctx().clone();
                plugin_install::fetch(download, move || ctx.request_repaint());
            }
        });
    }
    if let Some(error) = &state.error {
        g.note_colored(error, theme::RED_SOFT);
    }
}

/// Right to left: Remove (when something is installed and a dialog can ask), then
/// Install for what is missing, or the installed mark.
fn install_control(
    ui: &mut Ui,
    kit: &packages::Snapshot,
    family: &Family,
    state: &packages::FamilyState,
    removal: Option<&mut Option<Removal>>,
) {
    if kit
        .busy
        .as_deref()
        .is_some_and(|b| b.contains(family.label()))
    {
        ui.add(egui::Spinner::new().size(16.0));
        return;
    }
    let idle = kit.busy.is_none();
    if let Some(removal) = removal
        && !state.installed.is_empty()
        && ui
            .add_enabled(idle, outline_button("Remove"))
            .on_hover_text(t(
                "uninstalls the family's packages; you may be asked for your password",
            ))
            .clicked()
    {
        *removal = Some(Removal::Package {
            name: family.label().to_owned(),
            ids: state.installed.clone(),
            plugins: None,
        });
    }
    if state.available.is_empty() {
        ui.label(
            RichText::new(t("✓ installed"))
                .size(12.5)
                .color(theme::GREEN),
        );
    } else if ui
        .add_enabled(idle, outline_button(t("Install")))
        .on_hover_text(t(
            "installs the system package; you may be asked for your password",
        ))
        .clicked()
        && let Some(packages) = packages::get()
    {
        packages.install(family.name);
    }
}

/// The plugin families as compact rows, for the slot picker.
pub fn get_more_compact(ui: &mut Ui, lv2_off: bool) {
    let kit = packages::get().map(|p| p.snapshot());
    let mut g = Group {
        ui,
        first: true,
        compact: true,
    };
    get_more(&mut g, kit.as_ref(), lv2_off, true, None);
}

fn midi(ui: &mut Ui, graph: &Graph, commands: &mut Vec<Command>) {
    let candidates: Vec<&Node> = graph
        .nodes
        .iter()
        .filter(|n| n.class == MediaClass::Other && n.name != MIDI_INPUT_NAME)
        .filter(|n| {
            graph
                .ports_of(n.id)
                .any(|p| p.direction == canalis_pw::Direction::Output)
        })
        .collect();
    let selected = graph
        .midi_input
        .as_deref()
        .map(|name| {
            graph.node_by_name(name).map_or_else(
                || tf("{name} (absent)", &[("name", &name)]),
                |n| n.label.clone(),
            )
        })
        .unwrap_or_else(|| t("none").to_owned());
    group(ui, Some(t("Input")), |g| {
        g.row(
            t("Controller"),
            t("the MIDI device Canalis listens to"),
            |ui| {
                egui::ComboBox::from_id_salt("midi-input")
                    .selected_text(RichText::new(selected).size(13.0))
                    .width(ROW_CONTROL_WIDTH - 10.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(graph.midi_input.is_none(), t("none"))
                            .clicked()
                            && graph.midi_input.is_some()
                        {
                            commands.push(Command::SetMidiInput { node: None });
                        }
                        for node in &candidates {
                            let current = graph.midi_input.as_deref() == Some(node.name.as_str());
                            if ui.selectable_label(current, &node.label).clicked() && !current {
                                commands.push(Command::SetMidiInput {
                                    node: Some(node.name.clone()),
                                });
                            }
                        }
                    });
            },
        );
        if let Some(target) = &graph.learning {
            g.row(
                &tf("Learning: {target}", &[("target", &target.label())]),
                t("move a knob or press a pad on the controller"),
                |ui| {
                    if outline(ui, t("Cancel")).clicked() {
                        commands.push(Command::Learn { target: None });
                    }
                },
            );
        }
    });
    group(ui, Some(t("Learned controls")), |g| {
        if graph.controls.is_empty() {
            g.note(
                t("right-click a mute, a bypass, a send level, a scene or a plugin parameter to learn a control"),
            );
        }
        for (index, control) in graph.controls.iter().enumerate() {
            let trigger = match &control.trigger {
                ControlTrigger::Cc { channel, cc, .. } => tf(
                    "CC {cc} · channel {channel}",
                    &[("cc", cc), ("channel", &(channel + 1))],
                ),
                ControlTrigger::Note { channel, note, .. } => tf(
                    "note {note} · channel {channel}",
                    &[("note", note), ("channel", &(channel + 1))],
                ),
            };
            let inert = graph.midi_input.as_deref() != Some(control.trigger.port());
            let about = if inert {
                tf(
                    "{trigger} · inert: learned on {port}",
                    &[("trigger", &trigger), ("port", &control.trigger.port())],
                )
            } else {
                trigger
            };
            g.row(&control.target.label(), &about, |ui| {
                if outline(ui, t("Forget")).clicked() {
                    commands.push(Command::ForgetControl { index });
                }
            });
        }
    });
}

/// System default first, then every language by its own name; switches at once.
fn language_picker(ui: &mut Ui) {
    let chosen = i18n::chosen();
    let system = i18n::system().map_or("English", |l| l.name);
    let follow_system = tf("System ({language})", &[("language", &system)]);
    let selected = chosen.map_or(follow_system.clone(), |l| l.name.to_owned());
    egui::ComboBox::from_id_salt("language")
        .selected_text(RichText::new(selected).size(13.0))
        .width(ROW_CONTROL_WIDTH - 10.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(chosen.is_none(), follow_system)
                .clicked()
            {
                i18n::choose(None);
            }
            for language in LANGUAGES {
                let current = chosen.is_some_and(|l| l.code == language.code);
                if ui.selectable_label(current, language.name).clicked() {
                    i18n::choose(Some(language.code));
                }
            }
        });
}

fn about(ui: &mut Ui) {
    let home = std::env::var("HOME").unwrap_or_default();
    let config = dirs::config_dir()
        .map(|d| tilde(&d.join("canalis/config.toml").display().to_string(), &home))
        .unwrap_or_default();
    group(ui, None, |g| {
        g.row(
            "Canalis",
            &tf(
                "version {version}",
                &[("version", &env!("CARGO_PKG_VERSION"))],
            ),
            |_| {},
        );
        g.row(t("Made by"), "Luca Giulianini (meltingbots)", |ui| {
            ui.hyperlink_to(
                RichText::new("GitHub").size(12.5),
                "https://github.com/meltingbots",
            );
        });
        g.row(t("Settings file"), &config, |ui| {
            if outline(ui, t("Copy path")).clicked() {
                ui.ctx().copy_text(config.clone());
            }
        });
    });
}

/// A card of rows with thin rules between them, under an optional caption.
fn group(ui: &mut Ui, caption: Option<&str>, add: impl FnOnce(&mut Group<'_>)) {
    if let Some(caption) = caption {
        ui.add_space(8.0);
        ui.label(RichText::new(caption).size(13.0).color(theme::TEXT_DIM));
    }
    Frame::new()
        .fill(theme::PANEL)
        .stroke(Stroke::new(1.0, theme::BORDER_SOFT))
        .corner_radius(GROUP_RADIUS)
        .inner_margin(Margin::symmetric(16, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(&mut Group {
                ui,
                first: true,
                compact: false,
            });
        });
}

struct Group<'a> {
    ui: &'a mut Ui,
    first: bool,
    /// The picker's version: no card, tighter rows.
    compact: bool,
}

impl Group<'_> {
    fn rule(&mut self) {
        if !self.first && !self.compact {
            let rect = self.ui.available_rect_before_wrap();
            self.ui.painter().hline(
                rect.x_range(),
                rect.top(),
                Stroke::new(1.0, theme::BORDER_SOFT),
            );
        }
        self.first = false;
    }

    /// "Title / one line about it" on the left, the control on the right.
    fn row(&mut self, title: &str, about: &str, control: impl FnOnce(&mut Ui)) {
        self.rule();
        let pad = if self.compact { 3.0 } else { 10.0 };
        let ui = &mut *self.ui;
        ui.add_space(pad);
        ui.horizontal(|ui| {
            let text_width = (ui.available_width() - ROW_CONTROL_WIDTH - 12.0).max(140.0);
            ui.vertical(|ui| {
                ui.set_width(text_width);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).size(if self.compact { 12.5 } else { 13.5 }));
                if !about.is_empty() {
                    ui.add(egui::Label::new(theme::small_dim(about)).wrap());
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), control);
        });
        ui.add_space(pad);
    }

    /// A row that folds open to show `body`, for a plugin source and its plugins.
    fn expandable(
        &mut self,
        id: egui::Id,
        title: &str,
        about: &str,
        open: bool,
        control: impl FnOnce(&mut Ui),
        body: impl FnOnce(&mut Ui),
    ) {
        self.rule();
        let ui = &mut *self.ui;
        ui.add_space(10.0);
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        if open {
            state.set_open(true);
        }
        let header = state.show_header(ui, |ui| {
            let text_width = (ui.available_width() - ROW_CONTROL_WIDTH - 12.0).max(140.0);
            ui.vertical(|ui| {
                ui.set_width(text_width);
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).size(13.5));
                ui.add(egui::Label::new(theme::small_dim(about)).wrap());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), control);
        });
        header.body(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add_space(4.0);
            body(ui);
        });
        ui.add_space(10.0);
    }

    fn note(&mut self, text: &str) {
        self.note_colored(text, theme::TEXT_DIM);
    }

    fn note_colored(&mut self, text: &str, color: Color32) {
        self.rule();
        let pad = if self.compact { 3.0 } else { 10.0 };
        self.ui.add_space(pad);
        self.ui
            .add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
        self.ui.add_space(pad);
    }
}

/// An on/off switch, for settings that apply at once.
fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = vec2(38.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let radius = rect.height() / 2.0;
    let track = if *on { theme::ACCENT } else { theme::BORDER };
    ui.painter().rect_filled(rect, radius, track);
    let x = egui::lerp(rect.left() + radius..=rect.right() - radius, t);
    let knob = if *on { theme::BACKGROUND } else { theme::TEXT };
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, knob);
    response
}

fn outline_button(text: &str) -> Button<'_> {
    Button::new(RichText::new(text).size(12.5))
        .fill(theme::BACKGROUND)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .min_size(vec2(0.0, 28.0))
}

fn outline(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(outline_button(text))
}

/// The distinct plugin paths of the catalogue, without trailing slashes.
fn plugin_paths(graph: &Graph) -> Vec<String> {
    let mut paths: Vec<String> = graph
        .plugins
        .iter()
        .map(|p| p.path.trim_end_matches('/').to_owned())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Directly inside `~/.clap` or `~/.lv2`, where Canalis may delete it.
fn user_folder(path: &str, home: &str) -> bool {
    let path = std::path::Path::new(path);
    !home.is_empty()
        && [".clap", ".lv2"]
            .iter()
            .any(|dir| path.parent() == Some(&std::path::Path::new(home).join(dir)))
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

fn tilde(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

fn count(n: usize) -> String {
    if n == 1 {
        tf("{n} plugin", &[("n", &n)])
    } else {
        tf("{n} plugins", &[("n", &n)])
    }
}

fn format_list(plugins: &[&canalis_pw::PluginEntry]) -> String {
    let clap = plugins.iter().any(|p| p.format == "clap");
    let lv2 = plugins.iter().any(|p| p.format != "clap");
    match (clap, lv2) {
        (true, true) => t("CLAP and LV2"),
        (true, false) => "CLAP",
        _ => "LV2",
    }
    .to_owned()
}

fn ui_id(ui: &Ui, source: &Source) -> egui::Id {
    ui.scope_id().with(("plugin-source", format!("{source:?}")))
}

/// The board switched on or off; the first time on, it plays into the first mic chain's
/// virtual mic and your own output.
fn switched(graph: &Graph, enabled: bool) -> canalis_pw::Soundboard {
    let mut board = graph.soundboard.clone();
    board.enabled = enabled;
    let untouched = board.outputs.is_empty() && !board.monitor && board.pads.is_empty();
    if enabled && untouched {
        board.outputs = graph
            .chains
            .iter()
            .find(|c| c.kind == canalis_pw::ChainKind::Mic)
            .map(|c| c.kind.source_node_name(&c.name))
            .into_iter()
            .collect();
        board.monitor = true;
    }
    board
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_direct_children_of_the_user_folders_are_removable() {
        let home = "/home/u";
        assert!(user_folder("/home/u/.clap/Delay.clap", home));
        assert!(user_folder("/home/u/.lv2/verb.lv2", home));
        assert!(!user_folder("/home/u/.clap", home));
        assert!(!user_folder("/home/u/.clap/sub/Delay.clap", home));
        assert!(!user_folder("/usr/lib64/clap/lsp-plugins.clap", home));
        assert!(!user_folder("/home/u/.clap/x.clap", ""));
    }

    #[test]
    fn sections_parse_by_their_first_word() {
        assert_eq!(Section::parse("plugins"), Some(Section::Plugins));
        assert_eq!(Section::parse("midi"), Some(Section::Midi));
        assert_eq!(Section::parse("nope"), None);
    }
}
