#!/bin/sh
# Installs Canalis from its GitHub release: the .rpm on Fedora, the .deb on Debian and
# Ubuntu (both with sudo), anywhere else the tarball under ~/.local without sudo.
#
#   curl -fsSL https://claritas.meltingbots.com/install.sh | sh
#   curl -fsSL https://claritas.meltingbots.com/install.sh | sh -s -- --user
#
# Options: --user (the tarball under ~/.local, also on Fedora/Debian/Ubuntu),
# --version X (that release, not the latest), --uninstall. Running it again upgrades.
# CANALIS_RELEASES and CANALIS_LATEST point it at another copy of the releases (tests).
set -eu

repo=claritas-audio/canalis
releases="${CANALIS_RELEASES:-https://github.com/$repo/releases}"
latest="${CANALIS_LATEST:-https://api.github.com/repos/$repo/releases/latest}"
prefix="$HOME/.local"
dest="$prefix/lib/canalis"

say() { printf '%s\n' "$*"; }
die() { printf 'canalis install: %s\n' "$*" >&2; exit 1; }

mode=auto version="" uninstall=""
while [ $# -gt 0 ]; do
  case "$1" in
    --user) mode=user ;;
    --uninstall) uninstall=1 ;;
    --version) [ $# -ge 2 ] || die "--version needs a version, e.g. --version 0.2.0"
      version="${2#v}"; shift ;;
    -h|--help) say "usage: install.sh [--user] [--version X] [--uninstall]"; exit 0 ;;
    *) die "unknown option $1 (--user, --version X, --uninstall)" ;;
  esac
  shift
done

if [ "$(id -u)" = 0 ]; then sudo=""; else sudo=sudo; fi
need_sudo() {
  [ -z "$sudo" ] || command -v sudo >/dev/null 2>&1 || die "sudo is missing; use --user for a no-root install"
}

if [ -n "$uninstall" ]; then
  removed=""
  # The login service and launcher entry under ~/.config and ~/.local/share, first.
  for exe in "$dest/bin/canalis" /usr/lib/canalis/bin/canalis; do
    [ -x "$exe" ] && "$exe" --uninstall >/dev/null 2>&1 || true
  done
  if command -v rpm >/dev/null 2>&1 && rpm -q canalis >/dev/null 2>&1; then
    say "Removing the canalis package (sudo dnf remove canalis)."
    need_sudo; $sudo dnf remove -y canalis; removed=1
  elif command -v dpkg >/dev/null 2>&1 && dpkg -s canalis >/dev/null 2>&1; then
    say "Removing the canalis package (sudo apt remove canalis)."
    need_sudo; $sudo apt remove -y canalis; removed=1
  fi
  if [ -x "$dest/bin/canalis" ]; then
    say "Removing $dest, $prefix/bin/canalis and the launcher entry."
    rm -f "$prefix/bin/canalis"
    rm -rf "$dest"
    removed=1
  fi
  [ -n "$removed" ] || die "no Canalis installed by this script was found"
  say "Done. A Canalis still running keeps going until you quit it (tray icon › Quit, or"
  say "'canalis quit' before uninstalling). Your settings stay in ~/.config/canalis."
  exit 0
fi

[ "$(uname -m)" = x86_64 ] || die "releases are built for x86_64 only, this is $(uname -m)"
command -v curl >/dev/null 2>&1 || die "curl is missing"
glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | sed 's/^glibc //')"
case "$glibc" in
  2.[0-9]|2.[0-2][0-9]|2.3[0-8]|"") die "needs glibc 2.39 or newer (Fedora 40, Ubuntu 24.04, Debian 13); this system has ${glibc:-none}" ;;
esac

if [ "$mode" = auto ]; then
  # shellcheck disable=SC1091
  [ -r /etc/os-release ] && . /etc/os-release
  case " ${ID:-} ${ID_LIKE:-} " in
    *" fedora "*|*" rhel "*) mode=rpm ;;
    *" debian "*|*" ubuntu "*) mode=deb ;;
    *) mode=user ;;
  esac
fi

if [ -z "$version" ]; then
  version="$(curl -fsSL "$latest" | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$version" ] || die "cannot find the latest release at $latest"
fi
case "$mode" in
  rpm) file="canalis-$version-1.x86_64.rpm"
    plan="install $file with 'sudo dnf install' (dnf upgrades an older one)" ;;
  deb) file="canalis_${version}-1_amd64.deb"
    plan="install $file with 'sudo apt install' (apt upgrades an older one)" ;;
  user) file="canalis-$version-x86_64.tar.gz"
    plan="unpack $file into $dest, link $prefix/bin/canalis and add the launcher entry (no sudo)" ;;
esac

say "Canalis $version: this script will"
say "  1. download $file and SHA256SUMS from $releases/download/v$version/"
say "  2. check the file against SHA256SUMS"
say "  3. $plan"
say ""

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
# apt reads the file as its own unprivileged user.
chmod 755 "$tmp"
for f in "$file" SHA256SUMS; do
  curl -fSL --progress-bar -o "$tmp/$f" "$releases/download/v$version/$f" || die "cannot download $f for $version"
done
( cd "$tmp" && grep "  $file\$" SHA256SUMS | sha256sum -c - >/dev/null ) || die "$file does not match SHA256SUMS; nothing installed"
chmod 644 "$tmp/$file"
say "Checked $file against SHA256SUMS."

case "$mode" in
  rpm) need_sudo; $sudo dnf install -y "$tmp/$file" ;;
  deb) need_sudo; $sudo apt install -y "$tmp/$file" ;;
  user)
    tar -xzf "$tmp/$file" -C "$tmp"
    # Replaced, not overwritten: a running Canalis keeps its old files until it restarts.
    rm -rf "$dest.new"
    mkdir -p "$dest.new" "$prefix/bin"
    cp -R "$tmp/canalis-$version-x86_64/bin" "$tmp/canalis-$version-x86_64/lib" \
      "$tmp/canalis-$version-x86_64/share" "$dest.new/"
    rm -rf "$dest"
    mv "$dest.new" "$dest"
    ln -sf "$dest/bin/canalis" "$prefix/bin/canalis"
    "$dest/bin/canalis" --install >/dev/null 2>&1 || say "Could not add the launcher entry; 'canalis --install' does it."
    ;;
esac

say ""
say "Canalis $version is installed. Open it from the app menu or run: canalis"
if [ "$mode" = user ]; then
  case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) say "($prefix/bin is not on your PATH yet: run $prefix/bin/canalis, or log in again.)" ;;
  esac
  say "LV2 plugins also need lilv and suil from your distribution."
fi
say "A Canalis already running switches to this version the next time you open its window."
say "Remove it with: curl -fsSL https://claritas.meltingbots.com/install.sh | sh -s -- --uninstall"
