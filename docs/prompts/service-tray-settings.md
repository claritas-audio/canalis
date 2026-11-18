# Prompt: Canalis as a service, in the top bar, with settings that never break

Paste the block below as the first message of a Claude Code session opened in the `canalis` checkout on the home PC (Fedora 44, niri, Focusrite 2i2). Decision behind it: before 1.0 the settings file becomes a contract, and the service, not the window, becomes the thing that owns the audio.

---

You are working on Canalis, the PipeWire router of Claritas. I am Luca, the owner. Read `CLAUDE.md`, `docs/README.md`, `docs/HANDOFF.md` (start with "Next, in order"), `docs/architecture.md` and `apps/canalis/src/service.rs`, `crates/canalis-pw/src/config.rs` before changing anything. Rules from `CLAUDE.md` apply as always: real-time safety in the chain callback, work with WirePlumber, plugins only in `canalis-chain` processes, the three lenses in every commit body, plan mode per issue and my go before implementing, commits only when I ask.

Three topics, in this order, each one an issue on the board (create them under a new milestone "M8 Service and 1.0 settings" if none fits), each with a short plan and my go:

1. **Settings that survive versions.** After 1.0 a config written by any released version must load in every later one, and a newer file must never be destroyed by an older binary. Today:
   - `Config::load` replaces an unreadable file with `Config::default()`, and the next save overwrites the user's file: close this first (keep the file, back it up as `config.toml.broken-<time>`, refuse to save over it, tell the user in the window).
   - There is no version: add `version = N` at the top, a migration chain `vN → vN+1` applied on load (pure functions on `toml::Table`, each with a golden-file test), and the rule that the writer always writes the current version.
   - Unknown fields are dropped on save: decide with me between preserving them (a flattened `toml::Table` per struct) and refusing to save a file from a newer version; whichever we pick, an older Canalis must not silently lose a newer one's settings.
   - The plugin catalogue (`[[plugin]]`, thousands of lines) is a cache, not a setting: move it to `$XDG_CACHE_HOME/canalis/plugins.toml`, with a migration that drops it from old files.
   - Slot specs are TOML tables owned by `canalis-chain` (`SlotSpec`, CLAP/LV2 `state` blobs): they need the same promise; decide where their version lives.
   - Write the policy as an ADR (`docs/adr/0005-settings-compatibility.md`): what is stable, what may change, how a field is renamed or removed, how long a migration is kept. Add a `tests/configs/` folder with a real config from today (mine, anonymised) and a test that every file there loads and round-trips.

2. **Canalis as a service.** Today `canalis --daemon` runs as a systemd user unit (`ExecStart` is the absolute path at install time), and the window stops the service when it opens and starts it again when it closes, so every chain restarts around the window. Make the service own the audio and the window a client:
   - an IPC between window and service (a Unix socket in `$XDG_RUNTIME_DIR/canalis`, versioned JSON lines like the chain protocol, or D-Bus if you can argue it), carrying the `Command`s down and the `Graph` snapshots up;
   - the window starts the service if it is not running and never stops it; the service survives the window; one writer of `config.toml` (the service);
   - `canalis scene <name>` and similar CLI calls through the same IPC (the handoff lists this as missing from #32);
   - a unit that does not break when the binary moves or updates (and the Flatpak case: `flatpak run`), install and uninstall from the window, and the login round trip proven by hand: log out, log in, chains up before any window.

3. **Canalis in the system top bar.** A tray icon owned by the service (it is the process that lives), through StatusNotifierItem (the `ksni` crate, pure Rust over D-Bus): the canal mark from `assets/`, a menu with Open Canalis, the default output and input pickers, scenes, mute all, and Quit (stops the service, says so). Check first what my desktop offers: niri has no tray of its own, so find out which bar I run (waybar has a tray module) and test there; note what GNOME needs (the AppIndicator extension) and what KDE does. If no host is present the service must run fine without the icon. The meter idea from the website (the c of the logo moving like a level meter) is welcome as the icon's live state if the tray host allows it cheaply.

Verify every step on this machine: `cargo test --workspace`, clippy, and for the service and tray, by running them, not only by compiling. Keep `docs/HANDOFF.md` current and tell me what is proven by hand and what is not. Be terse, match my language, no emojis. If a design choice is not obvious, show me two options and recommend one.
