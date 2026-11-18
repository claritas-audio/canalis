# Prompt: Canalis on the Claritas website

Paste the block below as the first message of a Claude Code session (Fable 5.1) opened in the `claritas-site` checkout. Decision behind it: Canalis lives on the Claritas site as a product, not on its own site; the site already models the product line as modules on a rack, so Canalis is one more module plus a product page. Have the `canalis` checkout next to it and, if M1 is done, a screenshot or two of the real window.

---

You are working on `claritas-site`, the Astro site of Claritas (audio plugins for Linux, Rust). I am Luca, the owner. Read `README.md`, `src/data/products.ts`, the `/eq` product page and the components it uses before changing anything, and keep the site's look: the signal-chain metaphor (a cable down the left edge, numbered jacks, products as rack modules), the palette and the fonts (Outfit, JetBrains Mono), plain TypeScript for interaction, Biome clean, `pnpm check && pnpm lint && pnpm build` green.

Add **Canalis**, the second product. Its source of truth is the `canalis` repo at `../../rust/canalis` (ask me if the path differs): read `docs/brief.md`, `docs/architecture.md`, `docs/roadmap.md` and `docs/HANDOFF.md`. In one line: a PipeWire router for the Linux desktop, with virtual mics and outputs, per-app routing, a patchbay, and DAW-style effect strips that host built-in Claritas DSP and CLAP/LV2 plugins; a chain is itself a device.

Deliverables, in this order, with a short plan and my go before each:

1. **Rack module.** An entry in `src/data/products.ts` for Canalis, placed after the EQ. The module is a router, not a plugin: its front panel should read as a patchbay (input and output jacks, a few LEDs for chains) rather than as an EQ curve. Status "in development". No GitHub link while the repo is private; link to `/canalis`.
2. **`/canalis` product page**, built from the EQ page's structure, with these sections:
   - *The problem*, told from the user's side: EQ the desktop, clean a mic for calls, send one app to OBS, and today it means Carla, qpwgraph, pavucontrol and a shell script.
   - *How it works*: apps → chain → device, drawn as a small diagram in the site's style (SVG, no raster). Name the four views (Devices, Apps, Chains, Graph) and the four chain types (output, mic, cable, insert), each in one sentence.
   - *Chains are devices*: the one idea to land. "Desktop EQ" is what your apps see as an output; "Canalis Mic" is what Discord records from.
   - *Built on Claritas*: the EQ and the compressor run inside chains with no plugin boundary; CLAP and LV2 for everything else; every plugin in its own process.
   - *Status*: the milestones from `roadmap.md` as a list with what is done, honest about what is not. No release date.
   - *Later*: scenes, sends, ducking, loudness, MIDI learn, taken from the M7 issues; one line each.
   Screenshots: use the ones I give you; if I give none, use the diagram and leave a clearly marked placeholder, never a mock screenshot.
3. **Wordmark.** Claritas logos are generated from code in the plugin repo (`claritas_ui::wordmark`) and synced with `pnpm assets`. Propose a Canalis wordmark in the same family (same construction rules, its own glyph: a channel or a patch cable), as SVG in `src/assets/logos/`, plus the `og-canalis.png` link preview made the same way as the others in `README.md`. Show me the SVG before using it.
4. **Navigation and index.** The home page's product section and any nav list Canalis; `/canalis` has its own `<title>`, description and OG image.

Rules: nothing from the private repos appears on the site except what the page copy says; no claims of features that are not in `roadmap.md`; no download or install instructions until there is a release. Be terse, match my language, no emojis. If a design choice is not obvious, show me two options and recommend one.
