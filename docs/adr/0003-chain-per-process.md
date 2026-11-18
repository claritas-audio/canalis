# ADR 0003: one process per chain, a chain is the virtual device

Date: 2026-11-09. Status: accepted.

## Context
Carla puts every plugin in its own graph node and in the host process; a crash takes everything down and the graph is unreadable. A null sink plus manual links (the shell recipe in Claritas `output-eq-fedora.md`) is fragile.

## Decision
Each chain runs in a `canalis-chain` process as a capture+playback stream pair, after `module-filter-chain`. The app-facing side carries `Audio/Sink` or `Audio/Source`, so the chain is the virtual device: no separate null sink, one node in the graph. The control plane supervises the processes and restarts a crashed one with its slots bypassed.

## Consequences
Per-chain CPU isolation and crash isolation. Plugin GUIs must be opened from the chain process. Latency is reported per chain through `SPA_PARAM_ProcessLatency`.
