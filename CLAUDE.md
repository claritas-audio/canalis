# Canalis

PipeWire router for the Linux desktop: virtual mics and outputs, per-app routing, a patchbay, and DAW-style effect strips that host built-in Claritas DSP plus CLAP/LV2 plugins. Read `docs/README.md` first.

## Non-negotiable rules
- **Real-time safety in the chain's process callback:** no heap allocation, no locks, no syscalls, no logging, no panics. Preallocate for `MAX_QUANTUM` in `activate()`.
- **Work with WirePlumber, never against it.** Defaults and app targets are set through its metadata (`default.configured.audio.*`, `target.object`), not by racing its session policy.
- **Crash isolation.** Third-party plugins run only in `canalis-chain` processes, never in the GUI process.
- `unsafe` only in `canalis-pw` and `canalis-chain` (FFI to libpipewire and plugin hosts), one-line `// SAFETY:` per block. `apps/canalis` has `#![forbid(unsafe_code)]`. Enforced by `clippy::undocumented_unsafe_blocks`.
- Linux/Fedora/PipeWire ≥ 1.0 is the target. No other audio server, no other OS.
- Canalis is not a clean-room project (only Claritas EQ is). Open-source code such as PipeWire, WirePlumber, Helvum, EasyEffects and qpwgraph is fine to read.

## Layout
- `crates/canalis-pw` — control plane: registry mirror, link/unlink, virtual devices, metadata. Own thread, talks to the GUI over channels.
- `crates/canalis-chain` — audio plane: a capture+playback stream pair per chain, slots run inside the callback; also the `canalis-chain` binary later.
- `apps/canalis` — egui/eframe GUI: Devices, Apps, Chains, Graph.
- Conventions: Claritas `docs/code-standard.md` applies verbatim (layout, naming, visibility, errors, comments, tests).

## Commands
Toolchain from `mise.toml` (`mise install` once; never install rustup by hand).
```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p canalis
```
Build needs `pipewire-devel clang-devel` (from M1 on) and the egui/eframe X11/Wayland dev packages; see `docs/DEVELOPMENT.md`.

## Workflow
- Work is tracked in GitHub issues grouped by milestone (`docs/roadmap.md`). One issue per branch, one feature per commit, conventional commits.
- Plan mode per issue; wait for confirmation before implementing.
- Every change states three lenses in the commit body: PipeWire correctness (graph state survives reconnects and WirePlumber), real-time safety, user-facing behaviour.
- No secrets or binaries in git. The repo is private until the first usable release.
