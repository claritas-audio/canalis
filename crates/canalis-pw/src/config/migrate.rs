//! `config.toml` from any released version to the current one, one step per version, on
//! the raw table before serde sees it. Steps are never removed (ADR 0005).

use toml::{Table, Value};

/// Step `n` turns format `n` into `n + 1`; the array length is the current version.
const STEPS: [fn(&mut Table); 2] = [v0_drop_plugin_catalogue, v1_claritas_as_clap];

pub const VERSION: i64 = STEPS.len() as i64;

/// Brings `table` up to `VERSION` in place; a file from a newer version is left as it is
/// and its version returned.
pub fn migrate(table: &mut Table) -> Result<Option<i64>, String> {
    let version = match table.get("version") {
        None => 0,
        Some(Value::Integer(v)) if *v >= 0 => *v,
        Some(other) => return Err(format!("`version` is not a format number: {other}")),
    };
    if version > VERSION {
        return Ok(Some(version));
    }
    for step in &STEPS[version as usize..] {
        step(table);
    }
    table.insert("version".to_owned(), Value::Integer(VERSION));
    Ok(None)
}

/// The CLAP catalogue (`[[plugin]]`) moved to `$XDG_CACHE_HOME/canalis/plugins.toml`.
fn v0_drop_plugin_catalogue(table: &mut Table) {
    table.remove("plugin");
}

/// The built-in Claritas EQ and Comp became the Claritas CLAPs Canalis ships (ADR 0007):
/// an `eq` slot turns into a Claritas EQ CLAP slot drawing the same bands, a `comp` slot
/// into a Claritas Comp one. The path stays empty: the chain finds the bundled plugin.
fn v1_claritas_as_clap(table: &mut Table) {
    let Some(Value::Array(chains)) = table.get_mut("chain") else {
        return;
    };
    for chain in chains.iter_mut() {
        if let Some(Value::Array(slots)) = chain.get_mut("slots") {
            slots.iter_mut().for_each(claritas_slot_as_clap);
        }
    }
}

fn claritas_slot_as_clap(slot: &mut Value) {
    let Some(Value::Table(spec)) = slot.get_mut("spec") else {
        return;
    };
    let clap = |id: &str, params: Table| {
        Table::from_iter([
            ("kind".to_owned(), Value::from("clap")),
            ("path".to_owned(), Value::from("")),
            ("id".to_owned(), Value::from(id)),
            ("params".to_owned(), Value::Table(params)),
        ])
    };
    match spec.get("kind").and_then(Value::as_str) {
        Some("eq") => {
            let bands: Vec<crate::claritas::EqBand> = spec
                .get("bands")
                .cloned()
                .and_then(|b| b.try_into().ok())
                .unwrap_or_default();
            let params = crate::claritas::eq_params(&bands)
                .into_iter()
                .map(|(id, value)| (id, Value::Float(value)))
                .collect();
            *spec = clap(crate::claritas::EQ_ID, params);
        }
        Some("comp") => *spec = clap(crate::claritas::COMP_ID, Table::new()),
        Some("group") => {
            if let Some(Value::Array(lanes)) = spec.get_mut("lanes") {
                for lane in lanes.iter_mut() {
                    if let Some(Value::Array(slots)) = lane.get_mut("slots") {
                        slots.iter_mut().for_each(claritas_slot_as_clap);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tests/configs/migrate/v<n>.in.toml` migrated must equal `v<n>.out.toml`.
    const GOLDEN: [(&str, &str); 2] = [
        (
            include_str!("../../tests/configs/migrate/v0.in.toml"),
            include_str!("../../tests/configs/migrate/v0.out.toml"),
        ),
        (
            include_str!("../../tests/configs/migrate/v1.in.toml"),
            include_str!("../../tests/configs/migrate/v1.out.toml"),
        ),
    ];

    #[test]
    fn every_step_matches_its_golden_files() {
        assert_eq!(GOLDEN.len(), STEPS.len(), "one golden pair per step");
        for (n, (input, output)) in GOLDEN.iter().enumerate() {
            let mut table: Table = input.parse().expect("golden input parses");
            table.insert("version".to_owned(), Value::Integer(n as i64));
            STEPS[n](&mut table);
            table.insert("version".to_owned(), Value::Integer(n as i64 + 1));
            let expected: Table = output.parse().expect("golden output parses");
            assert_eq!(table, expected, "step {n}");
        }
    }

    #[test]
    fn a_file_without_version_is_format_zero() {
        let mut table: Table = "[[plugin]]\npath = \"/x.clap\"\nid = \"x\"\nname = \"X\"\n"
            .parse()
            .unwrap();
        assert_eq!(migrate(&mut table), Ok(None));
        assert_eq!(table.get("version"), Some(&Value::Integer(VERSION)));
        assert!(!table.contains_key("plugin"));
    }

    #[test]
    fn a_newer_file_is_left_untouched() {
        let text = format!("version = {}\n[[plugin]]\nid = \"x\"\n", VERSION + 1);
        let mut table: Table = text.parse().unwrap();
        let before = table.clone();
        assert_eq!(migrate(&mut table), Ok(Some(VERSION + 1)));
        assert_eq!(table, before);
    }

    #[test]
    fn a_version_that_is_not_a_number_is_an_error() {
        let mut table: Table = "version = \"1\"".parse().unwrap();
        assert!(migrate(&mut table).is_err());
    }
}
