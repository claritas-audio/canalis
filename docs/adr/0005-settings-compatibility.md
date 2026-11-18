# ADR 0005: settings compatibility

Date: 2026-11-12. Status: accepted.

## Context
From 1.0, `config.toml` holds hours of a user's work: chains, plugin state, scenes, learned controls. Up to now an unreadable file was replaced with an empty config on the next save, the file had no version, a save dropped every field the binary did not know, and the CLAP catalogue (thousands of lines that can be rebuilt) lived in the same file.

## Decision
- **A config written by any released version loads in every later one.** The file starts with `version = N`; a file without it is format 0. On load the raw TOML table goes through `config::migrate`, one pure step per version (`STEPS[n]` turns `n` into `n + 1`), before serde sees it. The writer always writes the current version.
- **Nothing is ever saved over a file this binary cannot fully understand.** An unreadable file is copied to `config.toml.broken-<unix time>`, left in place, and the session saves nothing until the user picks "Start fresh". A file of a newer format is read as far as it goes and never saved over; "Use anyway" first copies it to `config.toml.v<N>`. The window says which case applies. We chose this over keeping unknown fields in a flattened table per struct: that adds a field to every struct and still cannot keep a new enum variant (a new slot kind fails the whole parse), so it would not actually keep the promise.
- **One version for the whole file, slot tables included.** Slot tables are `canalis-chain`'s types (`SlotConfig`, `SlotSpec`), but `canalis-pw` already edits them by message type, so their migrations are steps in the same chain. `canalis-chain` checks the promise from its side (`tests/saved_slots.rs`). CLAP and LV2 `state` blobs belong to their plugins and are never migrated.
- **Caches are not settings.** The plugin catalogue lives in `$XDG_CACHE_HOME/canalis/plugins.toml` with its own `format`. A cache of another format, or one that is missing or unreadable, is dropped and rebuilt by a rescan. Step 0 drops `[[plugin]]` from old configs.

## Rules for changing the format
- **May change without a step:** adding a field with a default that means "as before"; adding a new enum variant; changing a cache.
- **Needs a step and a bumped version:** renaming or removing a field, changing a field's type or meaning, changing a default that old files rely on, moving data between files. The step rewrites the table; the old name is not kept as a serde alias.
- **Each step** has a golden pair, `crates/canalis-pw/tests/configs/migrate/v<n>.in.toml` and `v<n>.out.toml`. Steps are kept forever: they are small, pure, and the only way an old file comes forward.
- **Every release** adds a real config it wrote to `crates/canalis-pw/tests/configs/`, with personal data removed and never edited afterwards. Each one must load without a problem, keep its chains and survive a save and a reload unchanged (`tests/config_files.rs`).

## Consequences
A downgrade is safe but read-only until the user decides. A migrated file is written only on the next change, so an old file stays readable by an old binary until then. The first start after the upgrade rescans the plugins once.
