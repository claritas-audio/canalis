# Brief

## Problem
Putting an EQ on the desktop output, cleaning up a mic for calls, or routing one app somewhere else on Linux today means Carla (heavy, rack UI, one node per plugin), qpwgraph (links only), pavucontrol (devices only) and shell scripts with `pactl`/`pw-link` (see Claritas `docs/manual/output-eq-fedora.md`). Nothing does all of it, and nothing survives a reboot without more scripts.

## Product
One window that does what a broadcast console or an interface's control app does, on top of PipeWire:
- **Devices:** real and virtual inputs/outputs, defaults, volumes; create/delete virtual mics and outputs.
- **Apps:** every app stream with a target picker (device or chain).
- **Chains:** DAW-style strips: input → slots → output, meters at both ends. A chain is itself a virtual device, so apps see "Desktop EQ" as an output and "Canalis Mic" as a microphone.
- **Graph:** the patchbay for power users; a chain collapses to one node.
- Everything persists by `node.name` and comes back at login.

## Users
The Claritas owner first (desktop EQ, call mic, OBS input), then Linux streamers, podcasters and anyone who wants EasyEffects-style processing with any plugin format and real routing.

## Constraints
- Fedora, PipeWire ≥ 1.0, WirePlumber. No PulseAudio-only or JACK-only paths.
- Plugins never run in the GUI process.
- Real-time rules as in Claritas inside the audio callback.
- GPL-3.0-or-later.

## Scope v0.1
M1–M3 of the roadmap: read-only graph, virtual devices and routing, one chain with built-in Claritas EQ and Comp. CLAP/LV2 hosting and autostart follow.

## Definition of done (v0.1)
Starting Canalis, creating "Desktop EQ" with the Claritas EQ in it and making it the default output takes under a minute, needs no terminal, and is still there after logout/login.
