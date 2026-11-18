//! Interface languages. The English text in the code is the key: every other language is a
//! TOML file in `apps/canalis/i18n/` mapping each English string to its translation,
//! embedded at build time. A string missing from a catalog shows in English.
//!
//! Adding a language: copy `i18n/it.toml` to `i18n/<code>.toml`, translate the values,
//! add a line to `LANGUAGES`; the tests check it covers every string the code uses.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Language {
    /// ISO 639-1, as in `LANG=it_IT.UTF-8`.
    pub code: &'static str,
    /// The language's own name for itself.
    pub name: &'static str,
    catalog: &'static str,
}

pub const LANGUAGES: &[Language] = &[
    Language {
        code: "en",
        name: "English",
        catalog: "",
    },
    Language {
        code: "it",
        name: "Italiano",
        catalog: include_str!("../i18n/it.toml"),
    },
    Language {
        code: "fr",
        name: "Français",
        catalog: include_str!("../i18n/fr.toml"),
    },
    Language {
        code: "de",
        name: "Deutsch",
        catalog: include_str!("../i18n/de.toml"),
    },
];

/// Index into `LANGUAGES`.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static CATALOGS: OnceLock<Vec<HashMap<String, String>>> = OnceLock::new();

fn catalogs() -> &'static [HashMap<String, String>] {
    CATALOGS.get_or_init(|| {
        LANGUAGES
            .iter()
            .map(|language| parse(language.catalog).unwrap_or_default())
            .collect()
    })
}

fn parse(catalog: &str) -> Option<HashMap<String, String>> {
    let table: toml::Table = catalog.parse().ok()?;
    Some(
        table
            .into_iter()
            .filter_map(|(key, value)| Some((key, value.as_str()?.to_owned())))
            .collect(),
    )
}

/// The current language's text for `english`, or `english` itself.
pub fn t(english: &str) -> &str {
    catalogs()[CURRENT.load(Ordering::Relaxed)]
        .get(english)
        .map_or(english, String::as_str)
}

/// `t`, then each `{name}` filled from `args`.
pub fn tf(english: &str, args: &[(&str, &dyn Display)]) -> String {
    fill(t(english), args)
}

fn fill(text: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = text.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

/// The language chosen in Settings, if any; `None` follows the system.
pub fn chosen() -> Option<&'static Language> {
    let text = std::fs::read_to_string(preference_file()?).ok()?;
    let table: toml::Table = text.parse().ok()?;
    find(table.get("language")?.as_str()?)
}

/// Picks the language at startup: the one chosen in Settings, else the system's.
pub fn init() {
    let language = chosen().or_else(system).map_or("en", |l| l.code);
    select(language);
}

/// Picks the language again, as `init`; whether it changed.
pub fn reload() -> bool {
    let before = CURRENT.load(Ordering::Relaxed);
    init();
    CURRENT.load(Ordering::Relaxed) != before
}

/// Switches the interface now and remembers the choice; `None` goes back to the system's.
pub fn choose(code: Option<&str>) {
    select(code.or_else(|| system().map(|l| l.code)).unwrap_or("en"));
    let Some(path) = preference_file() else {
        return;
    };
    let written = match code {
        Some(code) => path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, format!("language = \"{code}\"\n"))),
        None => std::fs::remove_file(&path).or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(e)
            }
        }),
    };
    if let Err(e) = written {
        log::warn!("cannot keep the language in {}: {e}", path.display());
    }
}

fn select(code: &str) {
    if let Some(index) = LANGUAGES.iter().position(|l| l.code == code) {
        CURRENT.store(index, Ordering::Relaxed);
    }
}

fn find(code: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|l| l.code == code)
}

/// The system's language, as gettext reads it: `LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`.
pub fn system() -> Option<&'static Language> {
    ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.is_empty())
        .and_then(|value| from_locale(&value))
}

/// `it_IT.UTF-8` → Italian; `LANGUAGE` lists may hold several, `de:en`.
fn from_locale(value: &str) -> Option<&'static Language> {
    value.split(':').find_map(|locale| {
        let code = locale.split(['_', '.', '@']).next()?;
        find(code)
    })
}

/// `~/.config/canalis/gui.toml`: preferences of the window only, not of the service.
fn preference_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("canalis").join("gui.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn a_locale_picks_its_language_and_unknown_ones_none() {
        assert_eq!(from_locale("it_IT.UTF-8").map(|l| l.code), Some("it"));
        assert_eq!(from_locale("de_AT@euro").map(|l| l.code), Some("de"));
        assert_eq!(from_locale("pt_BR:fr").map(|l| l.code), Some("fr"));
        assert!(from_locale("C").is_none());
    }

    #[test]
    fn placeholders_are_filled_by_name() {
        let text = fill("{a} → {b}, {a}", &[("a", &"mic"), ("b", &3)]);
        assert_eq!(text, "mic → 3, mic");
    }

    /// Every `t("…")` and `tf("…"` literal in the sources, unescaped.
    fn used_strings() -> BTreeSet<String> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![root.clone()];
        let mut found = BTreeSet::new();
        while let Some(path) = files.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).expect("a source dir") {
                    files.push(entry.expect("an entry").path());
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || path.ends_with("i18n.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("a source file");
            found.extend(literals(&source));
        }
        found
    }

    fn literals(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        for opener in ["t(", "tf("] {
            let mut rest = source;
            while let Some(at) = rest.find(opener) {
                let before = rest[..at].chars().next_back();
                rest = &rest[at + opener.len()..];
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                // rustfmt puts a long literal on the next line.
                let Some(literal) = rest.trim_start().strip_prefix('"') else {
                    continue;
                };
                let (text, after) = unescape(literal);
                out.push(text);
                rest = after;
            }
        }
        out
    }

    /// A Rust string literal's text up to its closing quote, and what follows.
    fn unescape(literal: &str) -> (String, &str) {
        let mut text = String::new();
        let mut chars = literal.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return (text, &literal[i + 1..]),
                '\\' => match chars.next().map(|(_, c)| c) {
                    Some('n') => text.push('\n'),
                    Some('\n') => {
                        let rest = &literal[i + 2..];
                        let skip = rest.len() - rest.trim_start().len();
                        for _ in 0..skip {
                            chars.next();
                        }
                    }
                    Some(other) => text.push(other),
                    None => {}
                },
                c => text.push(c),
            }
        }
        (text, "")
    }

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name))
            .collect()
    }

    #[test]
    fn the_source_scanner_reads_escapes_and_continuations() {
        let source = "t(\"a \\\"b\\\"\") tf(\n    \"one \\\n        two\", &[]) not_t(\"x\") t(x)";
        assert_eq!(literals(source), ["a \"b\"", "one two"]);
    }

    #[test]
    fn every_language_translates_every_string_and_nothing_else() {
        let used = used_strings();
        assert!(used.len() > 100, "the scanner found {} strings", used.len());
        for language in LANGUAGES.iter().filter(|l| !l.catalog.is_empty()) {
            let catalog = parse(language.catalog)
                .unwrap_or_else(|| panic!("i18n/{}.toml does not parse", language.code));
            let missing: Vec<&String> = used.iter().filter(|s| !catalog.contains_key(*s)).collect();
            let unused: Vec<&String> = catalog.keys().filter(|k| !used.contains(*k)).collect();
            assert!(
                missing.is_empty() && unused.is_empty(),
                "{}: missing {missing:#?}, unused {unused:#?}",
                language.code
            );
            for (english, translated) in &catalog {
                assert_eq!(
                    placeholders(english),
                    placeholders(translated),
                    "{}: placeholders of {english:?}",
                    language.code
                );
            }
        }
    }
}
