#!/usr/bin/env bash
# Builds the Claritas CLAPs Canalis ships with (ADR 0007) and puts them where the
# canalis and canalis-chain binaries look: <bin>/../lib/clap.
#
#   scripts/bundle-claritas.sh                 # into target/lib/clap, for target/{debug,release}
#   scripts/bundle-claritas.sh /app/lib/clap   # the Flatpak build
#
# CLARITAS_SRC   a Claritas checkout or clone URL (default: the GitHub repo)
# CLARITAS_REV   the commit to build (default below; keep it in step with the Flatpak manifest)
# CLARITAS_WORK  where the clone lives (default ~/.cache/canalis/claritas-src)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${1:-$here/target/lib/clap}"
src="${CLARITAS_SRC:-https://github.com/claritas-audio/claritas.git}"
rev="${CLARITAS_REV:-1a57f8e}"
# Outside this repo: nice-plug's xtask builds in the outermost folder with a Cargo.toml.
work="${CLARITAS_WORK:-${XDG_CACHE_HOME:-$HOME/.cache}/canalis/claritas-src}"

if [ ! -d "$work/.git" ]; then
  git clone --quiet "$src" "$work"
fi
git -C "$work" fetch --quiet origin || true
git -C "$work" checkout --quiet --detach "$rev"

# Its own target/, whatever CARGO_TARGET_DIR says for Canalis: the bundles are read from there.
cargo_in_claritas() {
  if command -v mise >/dev/null 2>&1; then
    (cd "$work" && env -u CARGO_TARGET_DIR mise exec -- cargo "$@")
  else
    (cd "$work" && env -u CARGO_TARGET_DIR cargo "$@")
  fi
}
for package in claritas-eq claritas-comp; do
  cargo_in_claritas xtask bundle "$package" --release
done

mkdir -p "$dest"
for plugin in "Claritas EQ" "Claritas Comp"; do
  install -m 755 "$work/target/bundled/$plugin.clap" "$dest/$plugin.clap"
done
echo "Claritas $rev → $dest"
