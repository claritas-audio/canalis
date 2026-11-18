//! Every config in `tests/configs/` (real files from released versions, never edited
//! afterwards) must load in this build and survive a save and a reload (ADR 0005).

use std::fs;
use std::path::{Path, PathBuf};

use canalis_pw::{CONFIG_VERSION, Config, ConfigProblem};

fn configs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/configs");
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .expect("tests/configs exists")
        .map(|e| e.expect("a dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    files
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir.join("config.toml")
}

#[test]
fn every_saved_config_loads_and_round_trips() {
    for file in configs() {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let (config, problem) = Config::load(&file);
        assert_eq!(problem, None, "{name}");
        assert!(!config.chains.is_empty(), "{name} lost its chains");
        let path = scratch(&format!("round-trip-{name}"));
        config.save(&path);
        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.starts_with(&format!("version = {CONFIG_VERSION}\n")),
            "{name}"
        );
        assert!(!written.contains("[[plugin]]"), "{name} kept the catalogue");
        let (again, problem) = Config::load(&path);
        assert_eq!(problem, None, "{name}");
        assert_eq!(again, config, "{name}");
        again.save(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), written, "{name}");
    }
}

#[test]
fn an_unreadable_config_is_kept_and_backed_up() {
    let path = scratch("broken");
    let text = "[[chain]]\nname = \"canalis.eq\"\nkind = \n";
    fs::write(&path, text).unwrap();
    let (config, problem) = Config::load(&path);
    assert_eq!(config, Config::default());
    let Some(ConfigProblem::Unreadable {
        backup: Some(backup),
        ..
    }) = problem
    else {
        panic!("not reported as unreadable: {problem:?}");
    };
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("config.toml.broken-")
    );
    assert_eq!(fs::read_to_string(&backup).unwrap(), text);
    assert_eq!(fs::read_to_string(&path).unwrap(), text, "original touched");
}

#[test]
fn a_newer_config_is_read_as_far_as_known() {
    let path = scratch("newer");
    let text = format!(
        "version = {}\nfuture = true\n\n[[chain]]\nname = \"canalis.eq\"\nlabel = \"EQ\"\nkind = \"output\"\nsparkle = 3\n",
        CONFIG_VERSION + 1
    );
    fs::write(&path, &text).unwrap();
    let (config, problem) = Config::load(&path);
    assert_eq!(
        problem,
        Some(ConfigProblem::Newer {
            version: CONFIG_VERSION + 1
        })
    );
    assert_eq!(config.chains[0].name, "canalis.eq");
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn a_missing_config_is_empty_and_fine() {
    let path = scratch("missing");
    assert_eq!(Config::load(&path), (Config::default(), None));
}
