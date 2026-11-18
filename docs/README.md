# Canalis documentation

Single source of truth for humans and agents. Read in this order:

| File | What it answers |
|---|---|
| [brief.md](brief.md) | What we are building, for whom, constraints, scope, definition of done. |
| [architecture.md](architecture.md) | Control plane vs audio plane, process layout, data flow, real-time rules. |
| [DEVELOPMENT.md](DEVELOPMENT.md) | Fedora setup, build, run, debug against a live PipeWire. |
| [roadmap.md](roadmap.md) | Milestones M1–M7 and what each one proves. Issues live on GitHub. |
| [HANDOFF.md](HANDOFF.md) | Where the last session stopped, what to do first in the next one, decisions and open questions. Read before touching code. |
| [manual/](manual/README.md) | The user manual, one file per page; the same pages are the docs on the website. [manual/MAINTAINING.md](manual/MAINTAINING.md) says how to keep the two aligned. |
| [adr/](adr/) | Architecture decision records, one page each. |
| [prompts/](prompts/) | Session openers: `dev-kickoff.md` to work an issue, `site-canalis.md` for the product page on the Claritas site, `distribution.md` for the installer, COPR, AUR and the Flathub preparation. |

Rules every agent must follow are in the repo root `CLAUDE.md`. When a fact here and the code disagree, the code is the truth and this folder needs a fix.

## Status
2026-11-09: M1–M3 on `master`, M3 proven on the 2i2 (issues In review on the board). M4 started: #18 (CLAP slot with clack-host) on `issue/18-clap-slot`, #19 (plugin browser) on `issue/19-plugin-browser` #20 (plugin GUI window) on `issue/20-plugin-gui` #21 (CLAP state in config) on `issue/21-clap-state` and #22 (crash slot) on `issue/22-crash-slot`, each on top of the previous, then `ui/chains-strips` with all four views after the mockups and the logos in the app: M4 is implemented and waits for Luca's hands-on check; M6 (#26–#29) and #23 of M5 follow on `issue/26-daemon`, `issue/27-presets`, `issue/28-29-flathub-manual`, `issue/23-lv2-slot`. #31 deferred.
