# ADR 0007: Canalis ships the Claritas CLAPs; the built-in DSP is gone

Date: 2026-11-12. Status: accepted. Supersedes the "built-in, not shipped" choice of #46 and the `claritas-core` dependency of ADR 0001.

## Context
Claritas EQ and Comp reached Canalis twice: as DSP compiled into `canalis-chain` from `claritas-core` (no window, a band table under the slot), and as the CLAP plugins of the Claritas repo when the user happened to have them in `~/.clap` (with their own window). Presets picked one or the other, so "Desktop EQ" got the plugin and lost its curve while "Voice for calls" kept its curve and got no window. Luca, 2026-11-12: use only the CLAP, and ship it with Canalis.

## Decision
- **Ship the CLAPs.** `scripts/bundle-claritas.sh` builds Claritas EQ and Comp (nice-plug's `cargo xtask bundle`) from one pinned Claritas commit into `<bin>/../lib/clap`: `target/lib/clap` for development, `/app/lib/clap` in the Flatpak (its `claritas` module, same commit). The catalogue scans that folder last, so a user's own build of the same plugin id (in `~/.clap`) wins.
- **Presets add the plugins as they start**, flat: Luca shapes them in the window (2026-11-12).
- **No built-in DSP.** `SlotSpec::Eq` and `SlotSpec::Comp`, the runner's engine and the band editor are removed; `canalis-chain` no longer depends on `claritas-core`, so Canalis has no git dependency left. Add slot offers the bundled CLAPs.
- **Curves become parameters.** `canalis_pw::claritas` writes an EQ curve as Claritas EQ parameters: nice-plug's CLAP id is a 31-bit hash of the string id (`freq_3`), and its CLAP value is the normalized value (the step index for choices). Config migration 1 → 2 uses it to turn saved `eq`/`comp` slots into Claritas CLAP slots with an empty path.
- **An empty path means "the bundled one".** `canalis-chain` resolves a CLAP slot without a path to the bundle in its bundled folder that holds the slot's id, so migrated configs do not depend on where Canalis is installed.

## Consequences
- The parameter mapping depends on Claritas EQ's parameter ids, ranges and shape/slope order. A test loads the real plugin (from `~/.clap` or `target/lib/clap`) with a mapped high cut and checks a tone above it is gone; bump the pinned commit only with that test passing.
- Claritas Comp has no parameters yet: like the old built-in, it passes audio through until Claritas implements it.
- Development needs `scripts/bundle-claritas.sh` once (and after a Claritas bump), or presets and Add slot show the Claritas plugins as missing.
- Flathub needs the Claritas repo public, like Canalis.
