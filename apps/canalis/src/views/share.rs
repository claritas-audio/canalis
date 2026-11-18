//! Chains passed between people: a `.canalis-chain.toml` is `version = N` and one
//! `[[chain]]` table in the config's own format (ADR 0005), so an import goes through
//! the same migration as `config.toml`. What only makes sense on one machine (the ends,
//! the send gains, the key device) stays home; the importer's defaults take its place.

use std::path::{Path, PathBuf};

use canalis_chain::{SlotConfig, SlotSpec};
use canalis_pw::{CONFIG_VERSION, ChainConfig, ChainKind, Command, Config, Graph};

use super::presets;
use crate::i18n::{t, tf};

pub const EXTENSION: &str = ".canalis-chain.toml";
/// Stands for "the importer's default mic" in a shared chain's `key`.
const DEFAULT_KEY: &str = "default-mic";

/// The text to hand over for `chain`.
pub fn export_text(chain: &ChainConfig) -> String {
    let shared = ChainConfig {
        inputs: Vec::new(),
        outputs: Vec::new(),
        gains: Default::default(),
        key: chain.key.as_ref().map(|_| DEFAULT_KEY.to_owned()),
        mute: false,
        collapsed: false,
        ..chain.clone()
    };
    let mut file = toml::Table::new();
    if let Ok(table) = toml::Value::try_from(&shared) {
        file.insert("chain".to_owned(), toml::Value::Array(vec![table]));
    }
    let body = toml::to_string_pretty(&file).unwrap_or_default();
    format!(
        "# A Canalis chain: drop this file on the Canalis window, or paste it in\n\
         # Chains › Import chain.\nversion = {CONFIG_VERSION}\n\n{body}"
    )
}

/// Where an export starts: `~/Documents/Canalis`, made if missing.
pub fn default_folder() -> Option<PathBuf> {
    let dir = dirs::document_dir()
        .or_else(dirs::home_dir)?
        .join("Canalis");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// The file name an export suggests: the label, made safe for a file name.
pub fn default_name(chain: &ChainConfig) -> String {
    let stem: String = chain
        .label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{stem}{EXTENSION}")
}

/// Writes `chain` to `path` (adding the extension when the chosen name has none, so a
/// drop on the window recognises it); where it went.
pub fn write(chain: &ChainConfig, path: &Path) -> Result<PathBuf, String> {
    let path = if is_shared_chain(path) {
        path.to_path_buf()
    } else {
        let mut name = path.as_os_str().to_owned();
        name.push(EXTENSION);
        PathBuf::from(name)
    };
    std::fs::write(&path, export_text(chain)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Without a file chooser: the default folder and name, numbered rather than overwriting.
pub fn export_file(chain: &ChainConfig) -> Result<PathBuf, String> {
    let dir = default_folder().ok_or(t("no Documents folder"))?;
    let name = default_name(chain);
    let stem = name.trim_end_matches(EXTENSION);
    let path = (1..)
        .map(|n| match n {
            1 => dir.join(&name),
            n => dir.join(format!("{stem} {n}{EXTENSION}")),
        })
        .find(|p| !p.exists())
        .expect("a free name");
    write(chain, &path)
}

/// A chain kept in the library folder (`~/Documents/Canalis`), for Import chain's list.
#[derive(Debug)]
pub struct Shelved {
    pub path: PathBuf,
    pub label: String,
    pub kind: ChainKind,
    /// What its slots hold, by the names this machine knows them by.
    pub plugins: Vec<String>,
}

/// The shared chains in the library folder, by label; files that do not read are skipped.
pub fn library(graph: &Graph) -> Vec<Shelved> {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .map_or_else(Vec::new, |d| library_in(&d.join("Canalis"), graph))
}

fn library_in(dir: &Path, graph: &Graph) -> Vec<Shelved> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut shelf: Vec<Shelved> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_shared_chain(p))
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let (config, _) = Config::parse(&text).ok()?;
            let chain = config.chains.into_iter().next()?;
            let plugins = chain
                .slots
                .iter()
                .filter_map(|s| s.clone().try_into::<SlotConfig>().ok())
                .flat_map(|s| slot_names(&s.spec, graph))
                .collect();
            Some(Shelved {
                path,
                label: chain.label,
                kind: chain.kind,
                plugins,
            })
        })
        .collect();
    shelf.sort_by_key(|s| s.label.to_lowercase());
    shelf
}

/// The plugin names in a slot (a group's lanes too): the catalogue's name when this
/// machine has the plugin, else the end of its id.
fn slot_names(spec: &SlotSpec, graph: &Graph) -> Vec<String> {
    let named = |id: &str| {
        graph.plugins.iter().find(|p| p.id == id).map_or_else(
            || id.rsplit(['.', '/']).next().unwrap_or(id).to_owned(),
            |p| p.name.clone(),
        )
    };
    match spec {
        SlotSpec::Clap { id, .. } => vec![named(id)],
        SlotSpec::Lv2 { uri, .. } => vec![named(uri)],
        SlotSpec::Hardware { .. } => vec![t("hardware insert").to_owned()],
        SlotSpec::Group { lanes } => lanes
            .iter()
            .flat_map(|l| &l.slots)
            .flat_map(|s| slot_names(&s.spec, graph))
            .collect(),
    }
}

/// What an import does: the commands that build the chains, their labels, and the
/// plugins this machine lacks.
#[derive(Debug)]
pub struct Imported {
    pub commands: Vec<Command>,
    pub labels: Vec<String>,
    pub missing: Vec<String>,
}

impl Imported {
    /// One line for the status bar.
    pub fn summary(&self) -> String {
        let labels = self.labels.join(", ");
        if self.missing.is_empty() {
            tf("imported {labels}", &[("labels", &labels)])
        } else {
            tf(
                "imported {labels}; not installed here: {missing}",
                &[("labels", &labels), ("missing", &self.missing.join(", "))],
            )
        }
    }
}

/// A dropped or chosen file to import: any `.toml` (the name is the user's to pick).
pub fn is_shared_chain(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "toml")
}

/// Reads and imports the chain file at `path`.
pub fn import_file(path: &Path, graph: &Graph) -> Result<Imported, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    import(&text, graph)
}

/// Builds the chains in `text` on this machine: numbered labels when taken, the default
/// devices as ends, CLAP slots pointed at this machine's copy of each plugin.
pub fn import(text: &str, graph: &Graph) -> Result<Imported, String> {
    let (config, newer) =
        Config::parse(text).map_err(|e| tf("not a Canalis chain: {error}", &[("error", &e)]))?;
    if let Some(version) = newer {
        return Err(tf(
            "made by a newer Canalis (format {version}, this one reads {current}); update Canalis",
            &[("version", &version), ("current", &CONFIG_VERSION)],
        ));
    }
    if config.chains.is_empty() {
        return Err(t("no chain in it").into());
    }
    let mut imported = Imported {
        commands: Vec::new(),
        labels: Vec::new(),
        missing: Vec::new(),
    };
    for chain in config.chains {
        let label = presets::free_label(&chain.label, graph, &imported.labels);
        let name = canalis_pw::virtual_node_name(&label);
        let slots = chain
            .slots
            .iter()
            .map(|slot| relocate(slot, graph, &mut imported.missing))
            .collect();
        let (inputs, outputs) = presets::default_ends(graph, chain.kind);
        imported.commands.push(Command::CreateChain {
            label: label.clone(),
            kind: chain.kind,
        });
        imported.commands.push(Command::SetChainSlots {
            name: name.clone(),
            slots,
        });
        imported.commands.push(Command::SetChainEndpoints {
            name: name.clone(),
            inputs,
            outputs,
        });
        if chain.color.is_some() {
            imported.commands.push(Command::SetChainColor {
                name: name.clone(),
                color: chain.color,
            });
        }
        if chain.icon.is_some() {
            imported.commands.push(Command::SetChainIcon {
                name: name.clone(),
                icon: chain.icon,
            });
        }
        if chain.loudness_target.is_some() {
            imported.commands.push(Command::SetLoudnessTarget {
                name: name.clone(),
                target: chain.loudness_target,
            });
        }
        if chain.key.is_some()
            && let Some(mic) = presets::default_ends(graph, ChainKind::Mic)
                .0
                .into_iter()
                .next()
        {
            imported.commands.push(Command::SetChainKey {
                name: name.clone(),
                key: Some(mic),
            });
        }
        imported.labels.push(label);
    }
    imported.missing.sort();
    imported.missing.dedup();
    Ok(imported)
}

/// `slot` with its CLAP bundles found on this machine by plugin id (they live in other
/// folders on other distributions); plugins not found are added to `missing`.
fn relocate(slot: &toml::Value, graph: &Graph, missing: &mut Vec<String>) -> toml::Value {
    let Ok(mut config) = slot.clone().try_into::<SlotConfig>() else {
        return slot.clone();
    };
    relocate_spec(&mut config.spec, graph, missing);
    toml::Value::try_from(&config).unwrap_or_else(|_| slot.clone())
}

fn relocate_spec(spec: &mut SlotSpec, graph: &Graph, missing: &mut Vec<String>) {
    match spec {
        SlotSpec::Clap { path, id, .. } => {
            if !path.is_empty() && Path::new(path).exists() {
                return;
            }
            match graph.plugins.iter().find(|p| p.id == *id && !p.is_lv2()) {
                Some(entry) => *path = entry.path.clone(),
                None if path.is_empty() => {}
                None => missing.push(id.clone()),
            }
        }
        SlotSpec::Lv2 { uri, .. } => {
            if !graph.plugins.iter().any(|p| p.id == *uri) {
                missing.push(uri.rsplit('/').next().unwrap_or(uri).to_owned());
            }
        }
        SlotSpec::Group { lanes } => {
            for slot in lanes.iter_mut().flat_map(|l| l.slots.iter_mut()) {
                relocate_spec(&mut slot.spec, graph, missing);
            }
        }
        SlotSpec::Hardware { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canalis_pw::PluginEntry;

    fn chain() -> ChainConfig {
        toml::from_str(
            r#"
            name = "canalis.voice"
            label = "Voice"
            kind = "mic"
            inputs = ["alsa_input.usb-mine"]
            key = "alsa_input.usb-mine"
            icon = "🎙"
            gains = { "alsa_output.mine" = -6.0 }
            [[slots]]
            [slots.spec]
            kind = "clap"
            path = "/opt/a-colleague/clap/lsp-plugins.clap"
            id = "in.lsp-plug.gate_stereo"
            state = "AAEC"
            [[slots]]
            [slots.spec]
            kind = "lv2"
            uri = "http://example.org/verb"
            "#,
        )
        .expect("a chain")
    }

    #[test]
    fn an_exported_chain_leaves_the_machine_behind() {
        let text = export_text(&chain());
        assert!(text.contains(&format!("version = {CONFIG_VERSION}")));
        assert!(
            !text.contains("alsa_input.usb-mine"),
            "no device names:\n{text}"
        );
        assert!(!text.contains("alsa_output.mine"), "no gains:\n{text}");
        assert!(text.contains("AAEC"), "the plugin state travels");
        assert!(text.contains(DEFAULT_KEY), "keyed stays keyed");
    }

    #[test]
    fn an_import_finds_this_machines_plugins_and_names_what_is_missing() {
        let mut graph = Graph::default();
        graph.plugins.push(PluginEntry {
            path: "/usr/lib/clap/lsp-plugins.clap".into(),
            id: "in.lsp-plug.gate_stereo".into(),
            name: "Gate".into(),
            vendor: String::new(),
            modified: 0,
            format: "clap".into(),
        });
        let imported = import(&export_text(&chain()), &graph).expect("imports");
        assert_eq!(imported.labels, ["Voice"]);
        assert_eq!(imported.missing, ["verb"], "the LV2 plugin is not here");
        let Command::SetChainSlots { slots, .. } = &imported.commands[1] else {
            panic!("slots second");
        };
        assert_eq!(
            slots[0]["spec"]["path"].as_str(),
            Some("/usr/lib/clap/lsp-plugins.clap"),
            "the gate found where this distribution keeps it"
        );
        assert_eq!(slots[0]["spec"]["state"].as_str(), Some("AAEC"));
        assert!(
            imported
                .commands
                .iter()
                .any(|c| matches!(c, Command::SetChainIcon { icon: Some(i), .. } if i == "🎙"))
        );
    }

    #[test]
    fn the_library_lists_shared_chains_with_their_plugins() {
        let dir = std::env::temp_dir().join(format!("canalis-library-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Voice.canalis-chain.toml"), export_text(&chain())).unwrap();
        std::fs::write(dir.join("notes.toml"), "not = \"a chain\"").unwrap();
        let mut graph = Graph::default();
        graph.plugins.push(PluginEntry {
            path: "/usr/lib/clap/lsp-plugins.clap".into(),
            id: "in.lsp-plug.gate_stereo".into(),
            name: "Gate Stereo".into(),
            vendor: String::new(),
            modified: 0,
            format: "clap".into(),
        });
        let shelf = library_in(&dir, &graph);
        assert_eq!(shelf.len(), 1, "a .toml that is not a chain is skipped");
        assert_eq!(shelf[0].label, "Voice");
        assert_eq!(shelf[0].kind, ChainKind::Mic);
        assert_eq!(
            shelf[0].plugins,
            ["Gate Stereo", "verb"],
            "the catalogue's name, else the end of the id"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_newer_or_empty_file_is_refused() {
        let newer = format!(
            "version = {}\n[[chain]]\nname = \"x\"\nlabel = \"x\"\nkind = \"mic\"\n",
            CONFIG_VERSION + 1
        );
        assert!(
            import(&newer, &Graph::default())
                .unwrap_err()
                .contains("newer")
        );
        assert!(import("version = 1\n", &Graph::default()).is_err());
        assert!(import("not toml at all [", &Graph::default()).is_err());
    }
}
