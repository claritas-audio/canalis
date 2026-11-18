<!-- Part of the Canalis manual: one file per page, the same pages as https://claritas.meltingbots.com/canalis/docs. See docs/manual/MAINTAINING.md. -->

# Getting started

Canalis is a console for the audio of a PipeWire desktop: virtual mics and outputs, a target
for every app, effect chains with any CLAP or LV2 plugin, and a patchbay. Everything is saved
by name, so it is all there again after you log in.

## Install

The quickest way in is the install script. It picks the right package for your system:

```sh
curl -fsSL https://claritas.meltingbots.com/install.sh | sh
```

On Fedora it enables the Canalis COPR and on Ubuntu and Debian it adds the meltingbots APT
repository (`apt.meltingbots.com`), then installs from there, so `dnf upgrade` or `apt upgrade` keeps Canalis up to
date. Both ask for your password through `sudo`. Anywhere else it unpacks Canalis into
`~/.local/lib/canalis`, with no password. Downloads are checked against the release's
`SHA256SUMS`, and the APT repository is signed.

> [!TIP]
> Read a script before you pipe it into `sh`: open
> [install.sh](https://claritas.meltingbots.com/install.sh) in your browser, or download it
> and run it as `sh install.sh`. It says what it will do before it does it.

Running it again upgrades. It also takes options after `sh -s --`:

```sh
curl -fsSL https://claritas.meltingbots.com/install.sh | sh -s -- --user           # under ~/.local, no sudo
curl -fsSL https://claritas.meltingbots.com/install.sh | sh -s -- --version 0.2.1  # that release
curl -fsSL https://claritas.meltingbots.com/install.sh | sh -s -- --uninstall
```

The Claritas EQ and Comp plugins come with Canalis in every package; you don't install them
apart.

### Updates with your system

On Fedora, the COPR repository brings every new version with your normal updates:

```sh
sudo dnf copr enable meltingbots/canalis
sudo dnf install canalis
```

On Ubuntu 24.04 and later and Debian 13, the APT repository does the same. The install script
sets it up; by hand it is:

```sh
sudo curl -fsSLo /usr/share/keyrings/meltingbots-archive-keyring.gpg https://apt.meltingbots.com/meltingbots-archive-keyring.gpg
sudo tee /etc/apt/sources.list.d/meltingbots.sources <<'EOF'
Types: deb
URIs: https://apt.meltingbots.com
Suites: stable
Components: main
Architectures: amd64
Signed-By: /usr/share/keyrings/meltingbots-archive-keyring.gpg
EOF
sudo apt update && sudo apt install canalis
```

On Arch Linux and other distributions, the install script puts Canalis under `~/.local`;
run it again to upgrade. The Flatpak on Flathub, coming, will update with the rest of your
apps.

### Packages by hand

Every release is on [GitHub Releases](https://github.com/claritas-audio/canalis/releases/latest),
with a package for Fedora, one for Ubuntu and Debian, and a tarball for any other
distribution:

```sh
# Fedora 43 and later
sudo dnf install ./canalis-*.x86_64.rpm

# Ubuntu 24.04 and later, Debian 13
sudo apt install ./canalis_*_amd64.deb

# Any distribution with glibc 2.39 or newer, Arch Linux among them
tar -xzf canalis-*-x86_64.tar.gz && ./canalis-*-x86_64/install.sh
```

A Flatpak on Flathub is coming.

Install Canalis one way only. The Flatpak and a package each run their own Canalis service
with their own settings, and two services on one desktop remove each other's chains. To
switch, uninstall the old one first (`--uninstall` for the script, or `flatpak uninstall`).

### Checking a download

Put `SHA256SUMS` from the same release next to the download and run
`sha256sum -c SHA256SUMS --ignore-missing`.

### Requirements

- Linux on x86-64 with PipeWire 1.0 or newer and WirePlumber, which is what Fedora, Ubuntu,
  Debian and most current distributions ship.
- LV2 plugins also need `lilv` (and `suil` for their windows). Canalis offers to install
  them when it needs them.

## Starting

`canalis` opens the window. The first time, open **Chains** and click **Create** on a
preset, or build a chain yourself ([Chains](chains.md)).

Once you have chains, the presets stay under **From a preset** below the New chain card. A
second copy of one gets a number.

### The Canalis service

The chains run in the Canalis service, a background process the window starts when it is
not running. Closing the window leaves your routing in place.

To stop the service itself, run `canalis quit` (open windows close with it).

### Settings

The **⚙** at the top right opens the settings page (Esc or any tab closes it). Under
**General**:

- **Run at login**: switch it on to have the chains up before you open anything.
- **Add**: puts the app launcher entry in your desktop's app list.
- **Remove**: takes the launcher entry out and stops the login start.

### The intro

The logo intro plays each time you open Canalis from the launcher or a terminal; a click or
a key skips it. From the tray the window opens at once.

## Your first chain

1. Open **Chains**, pick **Desktop EQ** under **From a preset** and click **Create**.
2. In **Devices**, click its glyph so *Desktop EQ* wears the ★. Everything you play now goes
   through it.
3. Press **▣** on the EQ slot and shape the curve while music plays.
4. **⚙ › General › Run at login**. It will be there tomorrow.

More step-by-step setups are in [Recipes](recipes.md).

## Language

Canalis speaks English, Italian, French and German. It follows your desktop's language
(`LANGUAGE`, `LC_MESSAGES` or `LANG`) and falls back to English.

**Settings → General → Interface language** picks one by hand, at once. The tray menu
follows within a couple of seconds.

> [!NOTE]
> Names you give chains and devices are never translated.

<!-- nav: generated by scripts/manual.py -->

---

[Contents](README.md) · [Devices](devices.md) →
