# ADR 0004: the Graph view is a hand-rolled canvas on `egui::Scene`

Date: 2026-11-09. Status: accepted.

## Context
Issue #4 asked for a choice between `egui-snarl` and our own canvas. `egui-snarl` 0.12
pins `egui 0.36` while Canalis is on 0.37 (ADR 0002), so it cannot be linked today.
Beyond the version, snarl owns an editable `Snarl<T>` graph whose wires are created and
removed by the viewer; our graph is a mirror rebuilt from every PipeWire snapshot, and
in M2 links are made by asking the daemon and waiting for the registry to confirm, not
by mutating a local structure. A chain also has to collapse to one node with custom
chips (`architecture.md`), which snarl's node template does not cover.

## Decision
`apps/canalis/src/views/graph.rs` paints node boxes, ports and bezier links with the
egui painter inside `egui::Scene`, which provides pan and zoom. Retained state is the
camera rectangle and one position per `node.name`, so a daemon restart (new ids) keeps
the layout. Nodes are first placed in three columns: producers (sources, playing apps)
left, consumers (sinks, recording apps) right, everything else between.

## Consequences
About 250 lines we own, including hit-testing for M2's drag-to-link and link removal.
No third-party widget to track across egui releases. Revisit if snarl, or another
widget, becomes a thin painter over a borrowed graph.
