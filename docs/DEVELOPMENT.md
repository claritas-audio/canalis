# Development

## Fedora setup
```sh
sudo dnf install gcc git pkgconf-pkg-config clang-devel pipewire-devel \
  mesa-libGL-devel libX11-devel libxcb-devel libXcursor-devel \
  libxkbcommon-devel libxkbcommon-x11-devel wayland-devel
sudo dnf install mise            # repo: https://mise.jdx.dev/rpm/mise.repo
mise trust && mise install
```
`pipewire-devel` and `clang-devel` are needed from M1 on (`pipewire-sys` runs bindgen against `libpipewire-0.3`); `canalis-chain` opens lilv and suil at run time (`canalis_chain::lilv`), so building needs neither; running LV2 plugins needs `lilv-libs` (and `suil` for their windows). To check the no-lilv path, hide the libraries: `bwrap --dev-bind / / --ro-bind /dev/null /usr/lib64/liblilv-0.so.0.<ver> canalis-chain --scan-lv2` exits 3 with the install hint.

## Build, test, run
```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace                      # also builds canalis-chain, which the GUI spawns from next to itself
RUST_LOG=canalis=debug cargo run -p canalis
CANALIS_VIEW=graph cargo run -p canalis      # open on a tab: devices, apps, chains, graph
CANALIS_CONFIG=/tmp/try.toml cargo run -p canalis   # another config file; CANALIS_CHAIN_BIN overrides the chain binary
CANALIS_SCREENSHOT=/tmp/chains.png CANALIS_VIEW=chains cargo run -p canalis   # saves the window after ~90 frames and quits
```
The screenshot variable is how a session without a display tool checks a view: play a tone
into a chain first if the meters should show. It needs a live compositor: with the screen
off or locked no frame renders and the run just times out.
`CANALIS_SCREENSHOT_FRAMES=n` waits n frames instead of 90 (audio needs a few seconds to
reach the loudness meters). `CANALIS_SCREENSHOT_CLICK` plays input before the shot, one
action every ten frames from frame 30, separated by `;`: `x,y` clicks, `x1,y1>x2,y2` drags,
`rmb:x,y` right-clicks, `text:…` types and presses Enter (`dbl:x,y` does not register as a
double-click yet). The view lays out over the first frames as the graph arrives, so pad
with clicks on empty space (`700,1200;`) before the one that matters.
Screenshot runs skip the startup intro; `CANALIS_SCREENSHOT_INTRO=<seconds>` shows it frozen
at that moment instead (plugged in at about 1.3 s, signal arrived by about 2 s).
A second instance next to a running one treats `canalis.*` nodes missing from its config
as orphans and removes them: give it a copy of the running config, never one without its
chains.

## Headless and autostart
```sh
canalis --daemon      # the service: control plane, chains and the socket, until SIGTERM
canalis scene Calls   # a client call over the socket; `canalis quit` stops the service
canalis --install     # ~/.config/systemd/user/canalis.service, the .desktop entry and the icon
systemctl --user enable canalis   # or the "Run at login" box under the ⚙ settings
canalis --uninstall
```
The window is a client of the service (ADR 0006): it starts one when none answers on
`$XDG_RUNTIME_DIR/canalis/canalis.sock` (systemd first, rewriting the unit if it names
another binary; else `setsid canalis --daemon`) and never stops it. A service of another
build is asked to shut down and started again as this one, so after `cargo build` the
next window restarts the chains once. `CANALIS_CONFIG` keeps the old in-process mode for
screenshot runs and tests; a private service is `CANALIS_CONFIG=… canalis --daemon`, and
a plain `canalis` window then attaches to it instead of starting the real one.
The Claritas EQ and Comp CLAPs ship with Canalis (ADR 0007). Build them once, and again
after the pinned Claritas commit changes:

```sh
scripts/bundle-claritas.sh        # clones claritas-audio/claritas into ~/.cache/canalis
CLARITAS_SRC=~/dev/personal/rust/claritas scripts/bundle-claritas.sh   # from a local checkout
```

They land in `target/lib/clap`, where `target/{debug,release}/canalis` and
`canalis-chain` look (`<bin>/../lib/clap`). Without them, presets and Add slot show the
Claritas plugins as missing; a build of your own in `~/.clap` takes their place.

## A test build for someone else
```sh
scripts/package-container.sh       # ubuntu:24.04 → target/dist: the tarball and the .deb
scripts/package-container.sh rpm   # fedora:43 → the .rpm; SHA256SUMS over target/dist
```
The release packages built locally in podman on the release workflow's images, from this
checkout and the Claritas clone `scripts/bundle-claritas.sh` keeps in `~/.cache/canalis`.
The tarball (`canalis-<version>-x86_64.tar.gz`: `bin/`, `lib/clap/`, `share/`, an
`install.sh` into `~/.local/lib/canalis` with no root, a README) runs on any distribution
with glibc 2.39 or newer.

## The install script
`install.sh` in the repo root is what https://claritas.meltingbots.com/install.sh serves (the
site copies it with `pnpm docs:sync`). It installs the `.rpm` (Fedora, RHEL family) or the
`.deb` (Debian, Ubuntu) of a release with sudo, else the tarball under `~/.local/lib/canalis`
(`--user` forces that); `--version X`, `--uninstall`. `CANALIS_RELEASES` and `CANALIS_LATEST`
point it at another copy of the releases. Test it after any change:
```sh
scripts/package-container.sh && scripts/package-container.sh rpm   # target/dist
scripts/test-install.sh     # fedora:43/44, ubuntu:24.04, debian:13, archlinux, ubuntu --user
podman run --rm -v "$PWD:/mnt:ro,z" docker.io/koalaman/shellcheck:stable /mnt/install.sh
```
`test-install.sh` serves `target/dist` as the current version and a repack as the next
patch version over `python3 -m http.server`, then in each clean container: install the older,
upgrade, `canalis --version`, a CLAP scan from the installed tree, uninstall, and a release
whose `SHA256SUMS` does not match must install nothing.

## Releases
Bump `version` in `Cargo.toml`, commit, then `git tag v<version> && git push origin v<version>`.
`.github/workflows/release.yml` builds `scripts/package.sh deb tar` in `ubuntu:24.04` (Ubuntu 24.04+,
Debian 13, and the tarball for any distribution with glibc 2.39+) and `scripts/package.sh rpm` in
`fedora:43`, installs each package in its container, checks the Claritas CLAPs load from it and
from the unpacked tarball, and attaches all three with `SHA256SUMS` to a GitHub release.
A manual run (Actions › release › Run workflow) builds the packages as artifacts, no release.
CI clones the public Claritas repository at the commit `scripts/bundle-claritas.sh` pins.
Packages install to `/usr/lib/canalis/{bin,lib/clap}` (`<bin>/../lib/clap`, ADR 0007)
with `/usr/bin/canalis` as a symlink; dependencies and files are in `packaging/nfpm.yaml`.

### COPR (Fedora)
`packaging/rpm/canalis.spec` builds Canalis from the tag's GitHub archive and the Claritas
CLAPs from the archive of the pinned commit (`claritas_commit`, the same as `CLARITAS_REV`),
with Fedora's own Rust and `cargo build --locked`; crates come from crates.io, so the COPR
project has networking on. `.copr/Makefile` is COPR's "make srpm" method: it sets `Version`
from `Cargo.toml` and downloads the two archives, so both repos must be public.

One-time setup (Luca): a Fedora account, then an API token from
https://copr.fedorainfracloud.org/api/ saved as `~/.config/copr` (never in this repo), and
```sh
sudo dnf install copr-cli
copr-cli create canalis --enable-net on \
  --chroot fedora-43-x86_64 --chroot fedora-44-x86_64 --chroot fedora-45-x86_64 --chroot fedora-rawhide-x86_64 \
  --description "PipeWire router: virtual devices, per-app routing, a patchbay and effect chains" \
  --instructions "sudo dnf copr enable meltingbots/canalis && sudo dnf install canalis"
```
When Fedora branches a new release, add its chroot (`copr-cli modify canalis --chroot …` with
the full list) and rebuild the current version for it.
(the docs and the site say `meltingbots/canalis`; with another account name, change
`docs/manual/getting-started.md`, the README and the site's COPR card). At each release,
after the tag is pushed and the GitHub release exists:
```sh
copr-cli buildscm canalis --clone-url https://github.com/claritas-audio/canalis.git \
  --commit v<version> --method make_srpm --spec packaging/rpm/canalis.spec
```
**At each release this is automatic:** the release workflow's `copr` job, after the GitHub
release, builds the SRPM from the tag and the Claritas clone and runs
`copr-cli build meltingbots/canalis`, failing if COPR fails. It needs the `COPR_CONFIG` Actions
secret: the contents of `~/.config/copr`, renewed with the token every 180 days (also in chezmoi).
By hand, the same steps:
```sh
curl -fsSLo ~/rpmbuild/SOURCES/canalis-<version>.tar.gz https://github.com/claritas-audio/canalis/archive/v<version>/canalis-<version>.tar.gz
git -C ~/dev/personal/rust/claritas archive --prefix=claritas-<commit>/ -o ~/rpmbuild/SOURCES/claritas-<commit>.tar.gz <commit>
rpmbuild -bs packaging/rpm/canalis.spec          # in a fedora:43 container if rpm-build is missing
copr-cli build canalis ~/rpmbuild/SRPMS/canalis-<version>-1.*.src.rpm
```
Locally, without COPR: `git archive --prefix=canalis-<version>/` of the tag and
`git archive --prefix=claritas-<commit>/` of the Claritas clone into `~/rpmbuild/SOURCES`
(the names in the spec's `Source` lines), then `rpmbuild -ba` in a clean `fedora:43`
container after `dnf builddep`, or `mock -r fedora-43-x86_64 --enable-network`.

### APT (Debian, Ubuntu)
`https://claritas.meltingbots.com/apt` (the site redirects `/apt/*` to GitHub Pages,
`claritas-audio.github.io/canalis/apt`) is a signed APT repository, suite `stable`, component
`main`, amd64, holding the latest release only. `.github/workflows/apt.yml` rebuilds it from
scratch: it downloads the latest release's `.deb`, checks it against `SHA256SUMS`, indexes it
with `reprepro` (`packaging/apt/distributions`), signs with the key in the `APT_GPG_KEY`
secret, checks the signature with the public key in `packaging/apt/canalis-archive-keyring.asc`
and deploys to GitHub Pages. The release workflow calls it after each tag; Actions › apt › Run
workflow republishes by hand. The install script adds the repository on Debian and Ubuntu
(`/etc/apt/sources.list.d/canalis.sources`, key in
`/usr/share/keyrings/canalis-archive-keyring.gpg`).

The signing key: ed25519, signing only, no expiry, no passphrase (CI signs unattended),
fingerprint `A24EDC4A9F2CF09C06194286183B993A6CCFCB15`. Its private half and revocation
certificate live in Luca's Vaultwarden (secure note "Canalis APT signing key") and in the
`APT_GPG_KEY` secret, never in git. A new key means a new `.asc` here, `SignWith` in
`packaging/apt/distributions`, the secret, and every user fetching the new keyring (the
install script does it on its next run).

Test a change without publishing: download a release's `.deb`, build the repository with the
same steps in an `ubuntu:24.04` container, serve it with `python3 -m http.server`, and run
`CANALIS_APT=http://127.0.0.1:<port>/apt sh install.sh` in clean `ubuntu:24.04` and
`debian:13` containers.

### AUR (Arch)
`packaging/aur/canalis-bin/PKGBUILD` repackages the release tarball into
`/usr/lib/canalis/{bin,lib/clap}` with `/usr/bin/canalis` and `share/`; `.SRCINFO` beside it
is generated. At each release, once the GitHub release has its `SHA256SUMS`:
```sh
packaging/aur/update.sh <version>    # pkgver, pkgrel=1, the tarball's sha256, .SRCINFO (podman archlinux)
git add packaging/aur/canalis-bin && git commit -m "chore(aur): canalis-bin <version>"
```
One-time setup (Luca): an account on https://aur.archlinux.org with an SSH public key under
My Account (a key of its own, e.g. `ssh-keygen -t ed25519 -f ~/.ssh/aur`), and in
`~/.ssh/config`:
```
Host aur.archlinux.org
  IdentityFile ~/.ssh/aur
  User aur
```
Publishing (the first time creates the package; afterwards it is the same push):
```sh
git clone ssh://aur@aur.archlinux.org/canalis-bin.git ~/aur/canalis-bin
cp packaging/aur/canalis-bin/PKGBUILD packaging/aur/canalis-bin/.SRCINFO ~/aur/canalis-bin/
cd ~/aur/canalis-bin && git add PKGBUILD .SRCINFO && git commit -m "canalis-bin <version>" && git push
```
The AUR only accepts the `master` branch and needs `.SRCINFO` in every commit. Test a
PKGBUILD before pushing: copy it and the tarball into an `archlinux` container as a normal
user with sudo and run `makepkg -si` (the release tarball must match the sum).

## Running a chain by hand
```sh
RUST_LOG=info ./target/debug/canalis-chain --node-name canalis.test --label "Test" --kind output
echo '{"type":"slots","slots":[{"spec":{"kind":"clap","path":"","id":"audio.claritas.eq"}}]}' > /proc/$(pgrep -n canalis-chain)/fd/0
pw-cat -p --target canalis.test some.wav        # --target takes a node.name or serial, not an id
pw-record -P '{ stream.capture.sink = true }' --target canalis.test out.wav   # a sink's monitor needs this property
```
A CLAP plugin goes in the same way; an empty `id` takes the bundle's first plugin, and the
`slot_info` line that comes back lists the params with their ids and ranges:
```sh
echo '{"type":"slots","slots":[{"spec":{"kind":"clap","path":"/home/me/.clap/Claritas EQ.clap"}}]}' > /proc/$(pgrep -n canalis-chain)/fd/0
echo '{"type":"param","slot":0,"id":1515834035,"value":1.0}' > /proc/$(pgrep -n canalis-chain)/fd/0
echo '{"type":"gui","slot":0,"open":true}' > /proc/$(pgrep -n canalis-chain)/fd/0   # xwininfo -root -tree | grep canalis
```
`canalis-chain --scan-lv2` prints every LV2 plugin lilv finds (`uri`, `name`, `vendor`, `bundle`);
`canalis-chain --scan <bundle.clap>` prints the bundle's plugins (`id`, `name`, `vendor`) as
JSON lines and exits; the control plane runs it per bundle to build the catalogue cached as
`[[plugin]]` in the config. The chain reports `up`, `format`, `meters`, `latency` and
`slot_info`, `gui` and `state` (base64 plugin state, written to the config as `spec.state`)
as JSON lines on stdout, plus `crash` from the fatal-signal handler; see `canalis_chain::ipc`.
A debug build takes `canalis-chain --crash-test <slot>` to die in that slot on purpose. Do not `pkill -f` a chain from a script whose command
line contains the pattern: it kills the script.

## Live tests against the running session
`crates/canalis-pw/tests/live.rs` drives every `Command` against the real daemon and is
ignored by default. It loads its own null sink with `pactl`, plays silence with `pw-cat`,
uses a temporary config file and leaves nothing behind. It refuses to run if a
`canalis.*` node already exists, so that a real config is never touched. The chain test
needs the chain binary:
```sh
cargo build --workspace
CANALIS_CHAIN_BIN=$PWD/target/debug/canalis-chain RUST_LOG=canalis_pw=info \
  cargo test -p canalis-pw --test live -- --ignored --test-threads 1 --nocapture
```
Config lives in `~/.config/canalis/config.toml`, the plugin catalogue in
`~/.cache/canalis/plugins.toml`; `CANALIS_CONFIG` (`canalis_pw::spawn_with_config`) points a
session at another config file, with its catalogue beside it. A format change needs a
migration step and a golden pair: ADR 0005.

## Poking at PipeWire while developing
- `pw-cli ls Node`, `pw-cli ls Port`, `pw-link -l`: what the registry holds.
- `pw-top`: quantum, rate and xruns per node.
- `wpctl status`, `wpctl set-default <id>`: WirePlumber's view of defaults.
- `pw-metadata`: the metadata Canalis writes (`default.configured.audio.sink`, `target.object`).
- `pw-dump | jq`: the full object tree with every property.
- `RUST_LOG=canalis_pw=debug cargo run -p canalis`: one line per node, port and link mirrored or removed.

## Testing reconnects without touching your session
Run a private daemon under another core name and point Canalis at it; kill and restart
the daemon to exercise the backoff path. Nothing in the desktop session is affected.
```sh
PIPEWIRE_CORE=canalis-test pipewire -c pipewire.conf &
pw-cli -r canalis-test create-node adapter '{ factory.name=support.null-audio-sink node.name=private_sink media.class=Audio/Sink object.linger=true }'
PIPEWIRE_REMOTE=canalis-test RUST_LOG=canalis_pw=info cargo run -p canalis
kill %1   # Canalis logs "connection lost", retries at 1, 2, 4 … 10 s; start the daemon again and it reconnects
```
The real thing is `systemctl --user restart pipewire`, which also restarts WirePlumber and
re-creates the `default` metadata object Canalis reads the defaults from.

## Translations

The English text in the code is the key: wrap every string the window or tray shows in
`t("…")`, or `tf("… {name} …", &[("name", &value)])` when it holds values (whole
sentences, named placeholders, one string per plural form). Each other language is
`apps/canalis/i18n/<code>.toml`, `"English" = "translation"`, embedded in the binary.

- **New string:** add it to every catalog; `cargo test -p canalis i18n` lists what is
  missing or no longer used, per language, and checks the placeholders match.
- **New language:** copy `i18n/it.toml` to `i18n/<code>.toml` (ISO 639-1), translate the
  values, add one line to `LANGUAGES` in `apps/canalis/src/i18n.rs` with its own name for
  itself. It shows in Settings and is picked from the locale with no other change.
- Not translated: logs, the command line, config keys, node names, product names.

## Conventions
Claritas `docs/code-standard.md` applies verbatim. Rust edition 2024, rustfmt defaults, clippy `-D warnings`.
