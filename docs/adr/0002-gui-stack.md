# ADR 0002: egui/eframe 0.37 with the Claritas theme

Date: 2026-11-09. Status: accepted.

## Context
Claritas's editors are egui 0.37; the owner wants one look across the products. `claritas-ui` is not yet published and pulls in DSP, GL and X11 crates Canalis does not need.

## Decision
`eframe` 0.37 for the window. The palette and `apply()` are copied into `apps/canalis/src/theme.rs` and kept in sync by hand. When `claritas-ui` splits its theme into a light crate, Canalis depends on that instead. Plugin GUIs are floating native windows, not embedded.

## Consequences
Two copies of ~60 lines until the split. No dependency on a private repo.
