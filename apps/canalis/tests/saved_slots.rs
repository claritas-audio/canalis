//! Slot tables are `canalis-chain`'s, saved inside `config.toml`, and share its version
//! (ADR 0005): every slot in canalis-pw's saved configs, once loaded (and so migrated),
//! must parse in the chain the way the control plane hands it over (as JSON) and come
//! back unchanged.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use canalis_chain::{SlotConfig, SlotSpec};
use canalis_pw::Config;

fn kinds(spec: &SlotSpec, found: &mut BTreeSet<&'static str>) {
    let kind = match spec {
        SlotSpec::Clap { .. } => "clap",
        SlotSpec::Lv2 { .. } => "lv2",
        SlotSpec::Hardware { .. } => "hardware",
        SlotSpec::Group { lanes } => {
            for slot in lanes.iter().flat_map(|l| &l.slots) {
                kinds(&slot.spec, found);
            }
            "group"
        }
    };
    found.insert(kind);
}

#[test]
fn every_saved_slot_parses_and_round_trips() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/canalis-pw/tests/configs");
    let mut found = BTreeSet::new();
    for entry in fs::read_dir(&dir).expect("canalis-pw's tests/configs") {
        let path = entry.expect("a dir entry").path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let (config, problem) = Config::load(&path);
        assert_eq!(problem, None, "{}", path.display());
        for slot in config.chains.iter().flat_map(|c| &c.slots) {
            let json = serde_json::to_value(slot).expect("TOML to JSON");
            let parsed: SlotConfig = serde_json::from_value(json)
                .unwrap_or_else(|e| panic!("{}: {e}\n{slot}", path.display()));
            let back = toml::Value::try_from(&parsed).expect("serializes");
            let again: SlotConfig = back.try_into().expect("parses again");
            assert_eq!(again, parsed, "{}", path.display());
            kinds(&parsed.spec, &mut found);
        }
    }
    assert_eq!(
        found,
        BTreeSet::from(["clap", "group", "hardware", "lv2"]),
        "a slot kind with no saved example"
    );
}
