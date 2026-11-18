# Roadmap

Each milestone proves one thing. Issues live on GitHub under the milestone of the same name.

| Milestone | Proves | Done when |
|---|---|---|
| **M1 Read-only graph** | `canalis-pw` can mirror a live PipeWire registry and the GUI can show it | Devices and Graph views list every node, port and link; changes made with `pw-link` appear live |
| **M2 Virtual devices and routing** | We can change the graph without fighting WirePlumber | Create/delete virtual sink/source, link/unlink in Graph, set default, move an app stream; all of it reapplied after a PipeWire restart |
| **M3 Built-in chain** | The audio plane works with Claritas DSP | "Desktop EQ" chain with Claritas EQ → Comp, chosen as default output, with meters and reported latency; replaces `output-eq-fedora.md` |
| **M4 CLAP hosting** | Third-party plugins run isolated | A CLAP plugin in a slot with a floating GUI; killing the chain process does not stop desktop audio and the chain restarts bypassed |
| **M5 LV2 hosting** | The Linux plugin ecosystem is reachable | LSP/x42 plugins load in a slot; LV2 state saved and restored |
| **M6 Supervision and autostart** | It survives a reboot | systemd user unit, config persisted by name, presets ("Desktop EQ", "Voice for calls"), Flathub manifest |
| **M7 Console** | It works like a DAW console, not a settings panel | Scenes, per-output send gains, sidechain/ducking, mute/solo/undo, listen, loudness meter, MIDI/hotkey learn, hardware insert, names/colours/icons. Scenes (#32) may move into v0.1. |

Not planned: VST3 (license review first), Windows/macOS, PulseAudio-only systems.
