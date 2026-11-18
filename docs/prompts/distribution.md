# Prompt: distribute Canalis (one-line installer, COPR, AUR), then prepare Flathub

Paste the block below as the first message of a Claude Code session opened in the `canalis`
checkout on the home PC (the `claritas-site` checkout is at
`../../astro-projects/claritas-site`). Written 2026-11-16 with Luca; the reasoning behind the
channel choice is in `docs/HANDOFF.md` (2026-11-16 evening) and summed up below.

---

You are working on Canalis, a PipeWire router for the Linux desktop written in Rust, repo
`claritas-audio/canalis`. I am Luca, the owner. Read first: `CLAUDE.md`, `docs/README.md`,
`docs/HANDOFF.md` (the "Next" section from 2026-11-16), `docs/DEVELOPMENT.md` (Releases),
`docs/manual/MAINTAINING.md`, `.github/workflows/release.yml`, `packaging/` (nfpm, the
tarball with its own `install.sh`, the Flatpak manifest).

**Mode for this session: work autonomously from start to finish and report to me at the end.**
No plan-and-wait per issue. Stop and ask only for things only I can do: sudo, accounts and
tokens (Fedora account / COPR API token, AUR account / SSH key, Flathub), making the repo
public, merging and pushing. When you hit one of those, finish everything around it, write
down exactly what I must do, and move on to the next item.

## Why these channels (decided with Luca)
- The repo goes public soon; until then GitHub Releases links work only for people with
  access, so build and test everything now and leave the publishing steps for me.
- **A one-line installer** (`curl -fsSL https://claritas.meltingbots.com/install.sh | sh`) as
  the quick way in, in front of the real packages: host plugins, PackageKit and the systemd
  user service all work, unlike in a sandbox.
- **COPR** (Fedora) and **AUR** (Arch) give those packages automatic updates.
- **Flathub** next, done properly; it becomes the recommended channel once it exists.
- Not doing: AppImage (the service, its unit and updates fit it badly), Snap (confinement
  fights PipeWire and host plugins), crates.io. Ubuntu PPA/OBS later, if people ask.

## Work, in this order
Open one GitHub issue per item (labels as fits; put them on project 2 "Canalis board", owner
`claritas-audio`, and move each through In progress → In review), one branch per issue
(`issue/<n>-<slug>`, stacked if the previous is not merged), conventional commits with the
three lenses in the body (PipeWire correctness, real-time safety, user-facing behaviour),
"Issue #N; Luca closes it after review.", and the attribution lines.

1. **Release tarball.** The release workflow builds `.deb` and `.rpm` with `SHA256SUMS`. Add a
   plain `canalis-<version>-x86_64.tar.gz` (the `packaging/tarball` layout: `bin/`,
   `lib/clap/` with the Claritas plugins, `share/`) built on the oldest glibc we support
   (Ubuntu 24.04, glibc 2.39), included in `SHA256SUMS`. The installer's fallback and the AUR
   package use it. Prove it with a manual run of the workflow (Actions › release › Run
   workflow builds artifacts without a release).
2. **`install.sh`** in the repo root (served by the site at `/install.sh`):
   - POSIX `sh`, short and readable; `set -eu`; explains what it will do before doing it.
   - Finds the latest release (GitHub API, no `jq`), downloads into a temp dir, verifies with
     `SHA256SUMS`, cleans up.
   - Fedora/RHEL family (`/etc/os-release` `ID`/`ID_LIKE`): `sudo dnf install ./canalis-*.rpm`.
     Debian/Ubuntu: `sudo apt install ./canalis_*.deb`. Anything else, or `--user`: the
     tarball into `~/.local/lib/canalis` with `~/.local/bin/canalis` linked, no sudo, and
     `canalis --install` for the launcher entry.
   - Re-running upgrades. `--uninstall` undoes what it did. `--version X` picks a release.
   - Tested in podman containers: `fedora:43`, `fedora:44`, `ubuntu:24.04`, `debian:13`,
     `archlinux` (tarball path), each from a clean image: install, `canalis --version`,
     upgrade, uninstall. Use a local release (files served by `python3 -m http.server`, URL
     overridable by an environment variable) so this works while the repo is private.
   - `shellcheck` clean (install it in a container if it is not on the host).
3. **COPR** (Fedora): `packaging/rpm/canalis.spec` that builds from the source tag (Rust
   toolchain from Fedora, `cargo build --release --locked`, network allowed in the COPR
   project or vendored crates; the Claritas plugins come from `scripts/bundle-claritas.sh`,
   which needs `claritas-audio/claritas` reachable: note what changes when both repos are
   public). Prove it with `rpmbuild`/`mock` locally (ask me for `sudo dnf install mock
   rpm-build` if missing). Write the COPR project settings and the webhook or `copr-cli`
   commands I need to run, in `docs/DEVELOPMENT.md` › Releases.
4. **AUR**: `packaging/aur/canalis-bin/PKGBUILD` (+ `.SRCINFO`) from the release tarball, with
   the right `depends` (pipewire, lilv/suil as optdepends for LV2) and `sha256sums`. Prove it
   with `makepkg -si` in an `archlinux` container against the local tarball. Publishing needs
   my AUR account and SSH key: write the exact commands.
5. **Site and docs**:
   - Manual `getting-started.md` › Install: the one-liner first (with "read it first" and the
     manual commands next to it), then COPR, AUR, the `.rpm`/`.deb`, Flatpak "coming". Then
     `python3 scripts/manual.py`.
   - Site (`claritas-site`): serve `install.sh` at `/install.sh` (copied from the Canalis repo
     by a sync step like `pnpm docs:sync`, never edited there); the Canalis download section
     (`src/pages/canalis.astro`, `Download.astro`): the one-liner as the main card, then
     COPR, AUR, `.rpm`, `.deb`, Flathub "planned"; drop the AppImage card. `pnpm docs:sync`,
     `pnpm check`, `pnpm build`, screenshots at desktop and phone width.
   - `docs/manual/MAINTAINING.md` and the site README: what to update for each channel at a
     release.
6. **Flathub preparation** (the next big story; prepare everything, do not submit):
   - Plugins: in the Flatpak, host plugins built against host libraries often do not load in
     the runtime. Use the LinuxAudio extensions (`add-extensions:
     org.freedesktop.LinuxAudio.Plugins` and `LV2Plugins`/`ClapPlugins` as Flathub defines
     them; check the current names on Flathub), add their paths to the CLAP/LV2 search, keep
     `~/.clap`/`~/.lv2`, and drop `--filesystem=host-os:ro` if it is no longer needed.
     **⚙ › Plugins** inside the Flatpak lists the extensions with the `flatpak install` command
     instead of PackageKit (Canalis already knows it runs in a Flatpak).
   - Run at login: the Background portal's autostart (`org.freedesktop.portal.Background`
     RequestBackground with autostart and the `--daemon` command line) instead of the systemd
     user unit, only inside the Flatpak.
   - Real-time: check that chain threads get RT priority through the portal/rtkit in the
     sandbox; report what `chrt -p` shows for a chain thread.
   - LV2 inside the Flatpak: modules for serd, sord, sratom, zix, lilv and suil.
   - Offline build: `flatpak-cargo-generator` sources for Canalis and for the Claritas build,
     a module that builds the Claritas CLAPs into `/app/lib/clap`.
   - Metainfo: screenshots (the `docs/screenshots/*.png` raw URLs on GitHub, captions), OARS,
     release notes for 0.1.0 and 0.2.0, `appstreamcli validate` and `flatpak-builder-lint`.
   - Build and run it locally with `flatpak-builder` (ask me for the install), test against
     my session: a chain, an LSP plugin from the extension, run at login through the portal.
   - Write `packaging/flatpak/SUBMITTING.md`: the Flathub submission checklist and what is
     left (public repo, app id verification for `io.github.claritas_audio`).
7. **Handoff and report**: `docs/HANDOFF.md` (a dated section with what is proven, what is
   not, and my to-do list: accounts, tokens, making the repo public, the order to publish),
   the site's README if it changed, then a short report to me.

## Rules that matter here
- Never touch my running Canalis service or my audio session for tests: containers for
  packages, the private PipeWire recipe (`docs/DEVELOPMENT.md`, and the agent memory) for
  audio. Kill test processes by PID, never with `pkill -f`.
- No secrets in git. Tokens and keys stay in my home directory, never in the repo or in a
  command line that ends up in a commit.
- `git add` explicit files only. No `--force`, no `reset --hard`. Do not push or merge unless I
  say so; commit on the branches.
- Before every commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace`; on the site `pnpm check` and `pnpm build`.
- User-visible strings go through `t()`/`tf()` with it/fr/de translations
  (`cargo test -p canalis i18n`); manual changes follow `docs/manual/MAINTAINING.md`.

Be terse, match my language, no emojis.
