# Submitting Canalis to Flathub

Everything the manifest needs is here; what is left needs the repos public and Luca's accounts.
Flathub's own guide: https://docs.flathub.org/docs/for-app-authors/submission

## What the Flatpak does differently
- **Plugins** come from Flathub's LinuxAudio extensions (`org.freedesktop.LinuxAudio.Plugins.*`,
  branch `26.08`), mounted at `/app/extensions/Plugins/{clap,lv2}` and on `CLAP_PATH` /
  `LV2_PATH`, plus the user's `~/.clap` and `~/.lv2` and the Claritas CLAPs in `/app/lib/clap`.
  Host plugins in `/usr/lib*/{clap,lv2}` are not visible (no `host-os`): they are built
  against host libraries that are often missing in the runtime. ⚙ › Plugins lists the
  extensions with their `flatpak install` command instead of PackageKit.
- **LV2** works: serd, sord, sratom, zix, lilv and suil are built in the manifest (suil for
  X11 plugin windows only).
- **The service** is started by the Flatpak portal (`org.freedesktop.portal.Flatpak.Spawn`
  of `/app/bin/canalis --daemon`): a sandbox of its own that outlives the window, with
  nothing left waiting in the window's sandbox (`flatpak-spawn` would be). Not systemd.
- **Run at login** asks the Background portal (`RequestBackground` with `autostart` and
  `canalis --daemon`); the desktop writes `~/.config/autostart/io.github.claritas_audio.Canalis.desktop`.
  No `--talk-name=org.freedesktop.Flatpak`, no access to `~/.config/systemd`.
- **Real time**: the chain's `data-loop.0` runs `SCHED_FIFO|SCHED_RESET_ON_FORK` at priority
  70 inside the sandbox (`chrt -p`, 2026-11-16), through libpipewire's RT module and rtkit, as
  outside.

## Checklist
1. **Both repos public**: `claritas-audio/canalis` and `claritas-audio/claritas` (the manifest
   clones both over HTTPS).
2. **Tag the release** the manifest builds (`tag:` and `commit:` of the `canalis` module) and
   add its `<release>` to the metainfo; keep the Claritas `commit:` equal to `CLARITAS_REV` in
   `scripts/bundle-claritas.sh`.
3. **Regenerate the Cargo sources** for that tag (below) and check the local build:
   `packaging/flatpak/build-local.sh --lint` must end with no lint errors.
4. **App id verification.** `io.github.claritas_audio.Canalis` is verified by owning the
   GitHub organisation `claritas-audio`: after the app is on Flathub, log in to
   https://flathub.org with the GitHub account that owns the org and verify it from the
   developer portal (Flathub checks the org's membership; nothing to host).
5. **Submit**: fork https://github.com/flathub/flathub, branch from `new-pr`, add
   `io.github.claritas_audio.Canalis.yml`, `cargo-sources.json` and
   `claritas-cargo-sources.json` at the root, open the PR against `new-pr` titled
   "Add io.github.claritas_audio.Canalis". Answer the reviewers there; the bot builds on
   `bot, build`.
6. **After merge**: Flathub creates `flathub/io.github.claritas_audio.Canalis`; updates are
   PRs there (new tag, commit, regenerated sources). Then the site's Flathub card gets its
   `href`, the manual's Install section moves Flathub first, and the README says so.

## Testing a local build
`packaging/flatpak/build-local.sh` installs the app for this user. Its config
(`~/.var/app/io.github.claritas_audio.Canalis/config/canalis`) is not the native one, and a
second Canalis service on the same PipeWire removes the first one's `canalis.*` nodes as
orphans. So test it only against a private PipeWire (`docs/DEVELOPMENT.md` › Testing
reconnects; the agent memory has the WirePlumber half), confined with an override, and
uninstall it afterwards:
```sh
flatpak override --user --env=PIPEWIRE_REMOTE=canalis-test --filesystem=xdg-run/canalis-test \
  --nofilesystem=xdg-run/pipewire-0 io.github.claritas_audio.Canalis
flatpak run io.github.claritas_audio.Canalis            # the window starts the service
flatpak kill io.github.claritas_audio.Canalis
flatpak override --user --reset io.github.claritas_audio.Canalis
flatpak uninstall --user --delete-data io.github.claritas_audio.Canalis
```
The override reaches the portal-spawned service too; `pipewire-0` becomes an unreadable
placeholder in the sandbox. Proven that way on 2026-11-16: the window started the service
through the portal and it outlived the window; a chain with Claritas EQ (`/app/lib/clap`),
the LSP limiter CLAP and the LSP compressor LV2 from the LSP extension passed a tone to the
test sink; Settings › Plugins showed the extension commands; Run at login made the portal
write `~/.config/autostart/io.github.claritas_audio.Canalis.desktop`
(`Exec=flatpak run --command=canalis io.github.claritas_audio.Canalis --daemon`), that line
started the service and its chain, and switching it off removed the file.

## Things reviewers may ask about
- `--filesystem=~/.clap` and `~/.lv2` (read-write): a plugin dropped on the window is
  installed there, and the user's own plugins load from there.
- `--filesystem=xdg-run/pipewire-0`: Canalis is a PipeWire patchbay; it needs the whole graph,
  not the portal's screen-cast subset.
- `--filesystem=xdg-documents/Canalis:create`: Export to file.
- `--device=dri`: the egui window renders with OpenGL.
- No `--talk-name=org.freedesktop.Flatpak`, no `host-os`, no `--socket=pulseaudio`.

## Regenerate the Cargo sources
```sh
curl -fsSLO https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
# needs aiohttp, PyYAML and tomlkit (e.g. `uv run flatpak-cargo-generator.py …`)
python3 flatpak-cargo-generator.py Cargo.lock -o packaging/flatpak/cargo-sources.json
python3 flatpak-cargo-generator.py ~/.cache/canalis/claritas-src/Cargo.lock \
  -o packaging/flatpak/claritas-cargo-sources.json     # the Claritas clone at CLARITAS_REV
```

## Still open
- Screenshots in the metainfo point at `raw.githubusercontent.com/.../master/docs/screenshots/`:
  they resolve only once the repo is public (`appstreamcli validate` without `--no-net`
  checks them then).
- The Background portal's autostart was proven on the home PC's portal backend
  (`xdg-desktop-portal-gnome` under niri), not on KDE; a real login start was not tried.
- `flatpak-builder-lint` (repo and manifest) reports only what a private repo causes:
  `appid-url-not-reachable` and the screenshots it cannot fetch or mirror.
- The Flatpak and a native install side by side on one PipeWire would fight over the
  `canalis.*` nodes (separate configs and sockets); the manual should say to use one.
