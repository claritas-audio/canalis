# Prompt: start developing Canalis issues

Paste the block below as the first message of a Claude Code session (Fable 5.1) opened in the `canalis` checkout. Replace `#1` with the issue you want when you are past M1.

---

You are working on Canalis, a PipeWire router for the Linux desktop written in Rust, in the repo `claritas-audio/canalis` (private). I am Luca, the owner. Read, in this order, before doing anything: `CLAUDE.md`, `docs/README.md`, `docs/HANDOFF.md`, `docs/architecture.md`, `docs/roadmap.md`. Code conventions are Claritas's `docs/code-standard.md`; the Claritas checkout is at `../claritas` (if it is not, ask me for the path).

Then work issue **#1** (`gh issue view 1`). The working loop for every issue:

1. **Plan mode first.** Read the issue and the issues it depends on, look at the code it touches, and give me a short plan: what changes, in which files, how you will prove it works against a live PipeWire (the `pw-*` tools in `docs/DEVELOPMENT.md`). Wait for my go.
2. **Branch** `issue/<n>-<slug>` from `master`. One issue per branch, one feature per commit, conventional commits.
3. **Implement** within the rules in `CLAUDE.md`: no allocation, locks, syscalls, logging or panics on the audio thread; `unsafe` only in `canalis-pw` and `canalis-chain` with a one-line `// SAFETY:`; WirePlumber metadata for defaults and targets, never a fight with its policy; plugins never in the GUI process.
4. **Prove it**: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, then run `cargo run -p canalis` against my real PipeWire session and check the issue's "done when" with `pw-cli`, `pw-link`, `pw-top`, `wpctl`, `pw-dump`. Report what you saw, not what you expected.
5. **Commit** with the three lenses in the body: PipeWire correctness (graph state survives reconnects and WirePlumber), real-time safety, user-facing behaviour. Reference the issue (`Refs #n`); I close issues after trying the result. Do not push, merge or open PRs unless I say so.
6. **Hand off**: update `docs/HANDOFF.md` (where it stopped, what to try, open items) and any doc the change made stale, in the same branch. If you found a bug outside the issue, list it in the handoff; do not fix it in this branch.

Things that are true today and easy to get wrong:
- egui 0.37 renamed the panel API: `egui::Panel::top(...)`, `CentralPanel::show(ui, ...)`, and `eframe::App::ui(&mut self, ui, frame)` instead of `update`.
- The `pipewire` crate (0.10) needs `pipewire-devel` and `clang-devel` installed; its main loop is `!Send`, so it lives on its own thread and talks to the GUI over channels (`architecture.md`).
- Ids change on every PipeWire restart; persist by `node.name` and port name only.
- Build on the chain-per-process design (ADR 0003); do not put slots in the GUI process to save time.

Be terse: one sentence before the first tool call, then only meaningful updates. Match my language (English or Italian). No emojis. If critical context is missing, ask one targeted question instead of guessing.
