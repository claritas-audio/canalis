Canalis: PipeWire router with effect chains

For any x86_64 Linux with glibc 2.39 or newer (Fedora 40+, Ubuntu 24.04+, Debian 13+,
Arch) and PipeWire 1.0 or newer. On Fedora and Ubuntu/Debian the .rpm and .deb are the
better choice:
  curl -fsSL https://claritas.meltingbots.com/install.sh | sh

Install (no root), into ~/.local/lib/canalis with ~/.local/bin/canalis:
  ./install.sh
  canalis

Re-running ./install.sh from a newer tarball upgrades. In the window, ⚙ (top right) ›
General › Run at login starts the chains when you log in.

Closing the window leaves the chains running in the background; the tray icon or
`canalis quit` stops them.

Plugins: Claritas EQ and Comp come with Canalis. ⚙ › Plugins installs more (LSP,
Calf, ZAM...). LV2 plugins also need lilv and suil from your distribution
(Arch: lilv suil; Debian/Ubuntu: liblilv-0-0 libsuil-0-0; Fedora: lilv-libs suil).

Uninstall:
  ./install.sh --uninstall
  rm -r ~/.config/canalis      # your settings, if you want them gone too

Problems: run `RUST_LOG=info canalis` in a terminal and send the output.

License: GPL-3.0-or-later (LICENSE). Source: https://github.com/claritas-audio/canalis
