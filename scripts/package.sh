#!/usr/bin/env bash
# Canalis with the Claritas CLAPs as a .deb, .rpm or plain tarball, built on the distribution
# it targets (CI: ubuntu:24.04 for the .deb and the tarball, fedora:43 for the .rpm,
# .github/workflows/release.yml).
#
#   scripts/package.sh deb|rpm|tar...   # → target/dist/canalis_<version>-1_amd64.deb,
#                                       #   canalis-<version>-1.x86_64.rpm,
#                                       #   canalis-<version>-x86_64.tar.gz
#
# Needs cargo, git and nfpm (deb, rpm) on PATH; Claritas comes as scripts/bundle-claritas.sh
# fetches it (CLARITAS_SRC, CLARITAS_WORK, CLARITAS_REV).
set -euo pipefail

[ $# -gt 0 ] || { echo "usage: $0 deb|rpm|tar..."; exit 2; }
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"
target="${CARGO_TARGET_DIR:-$here/target}"
stage="$here/target/pkg"
dist="$here/target/dist"

cd "$here"
cargo build --release --locked -p canalis -p canalis-chain

rm -rf "$stage" && mkdir -p "$stage" "$dist"
install -m 755 "$target/release/canalis" "$target/release/canalis-chain" "$stage/"
"$here/scripts/bundle-claritas.sh" "$stage/clap"

# The same tree as the packages' /usr/lib/canalis plus share/, under one top folder.
tarball() {
  local name="canalis-$version-x86_64" app=io.github.claritas_audio.Canalis
  local root="$stage/tar/$name"
  install -Dm755 -t "$root/bin" "$stage/canalis" "$stage/canalis-chain"
  install -Dm755 -t "$root/lib/clap" "$stage"/clap/*.clap
  install -Dm644 "packaging/flatpak/$app.desktop" "$root/share/applications/$app.desktop"
  install -Dm644 "packaging/flatpak/$app.metainfo.xml" "$root/share/metainfo/$app.metainfo.xml"
  install -Dm644 assets/canalis-app-icon.svg "$root/share/icons/hicolor/scalable/apps/$app.svg"
  install -Dm755 packaging/tarball/install.sh "$root/install.sh"
  install -Dm644 -t "$root" packaging/tarball/README.txt LICENSE
  tar -C "$stage/tar" --owner=0 --group=0 -czf "$dist/$name.tar.gz" "$name"
  echo "$dist/$name.tar.gz"
}

for format in "$@"; do
  case "$format" in
    deb|rpm) VERSION="$version" nfpm package --config packaging/nfpm.yaml --packager "$format" --target "$dist/" ;;
    tar) tarball ;;
    *) echo "unknown format $format"; exit 2 ;;
  esac
done
