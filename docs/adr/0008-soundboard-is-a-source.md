# ADR 0008: the soundboard is a source node fed by its own engine crate

Date: 2026-11-15. Status: accepted.

## Context
#72 first played each pad as its own short-lived stream linked into a chain's outputs. Luca's hand test (2026-11-15) asked for more: pads that stop on their own, beds, hold, a whole Launchpad, effects on the pads, and an idea of routing them through an automatic "padcable" chain. He also asked whether a soundboard is a Canalis feature or a product of its own (a drum machine).

## Decision
- **One source node.** The service runs one real-time stream, `canalis.soundboard` (`media.class = Audio/Source`, not autoconnected, always processing, `priority.session` 1 so it never becomes the fallback mic). Every pad mixes into it. Where it plays is ordinary routing: `Soundboard::outputs` (node names) plus `monitor` (the default output), linked by the reconciler, which owns every link leaving the node. A link drawn from it in the Graph becomes one more output, an unlink removes one. Effects on the pads are a chain that takes Soundboard as its input; nothing is created behind the user's back.
- **Not a padcable.** An automatic cable chain would cost a chain process and its latency for every user, stack two chains' latency in front of the mic chain, and be a special object nobody created. The source node gives the same reach with none of that, and a chain is one click away when wanted.
- **`canalis-pads` holds the engine** and knows nothing of PipeWire, chains or the config: decoding (symphonia, then a windowed-sinc resampler to 48 kHz), voices with one-shot/toggle/hold, loop, choke groups, 20 ms fades, and a lock-free command/event pair between the host thread and the callback. The callback never allocates or frees (`tests/no_alloc.rs`); finished sounds go back to the host to be dropped. Canalis provides the stream, routing, MIDI learn, the Pads tab, tray and CLI.
- **A feature, not a product, for now.** A drum machine (sequencer, patterns, tempo, kits) is a different product. If one is ever built it is its own app or a CLAP instrument; as a PipeWire node it needs no integration in Canalis. The engine crate is the piece that would move.

## Consequences
The Pads tab is off by default (Settings › General › Features). The graph shows the Soundboard like a mic, and apps can record it directly. Per-pad buses (several source nodes) are a later addition, a new field with a default. Launchpad LEDs need a MIDI output Canalis does not have yet.
