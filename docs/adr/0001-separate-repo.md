# ADR 0001: Canalis is its own repository

Date: 2026-11-09. Status: accepted.

## Context
Canalis is a host (PipeWire client, plugin loader), Claritas is a set of plugins. The host needs `libpipewire` headers and FFI-heavy `unsafe`; the plugin crates build with nothing beyond the Rust toolchain and forbid `unsafe` almost everywhere.

## Decision
Separate repository `claritas-audio/canalis`, same org and license. Built-in DSP comes from `claritas-core` as a crates.io dependency once published; until then as a git dependency.

## Consequences
A `claritas-core` change needed by Canalis is two steps (commit there, bump here). The plugin repo keeps its zero-system-deps build and its `unsafe` policy.
