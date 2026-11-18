#!/usr/bin/env bash
# Builds the Flatpak from this working tree (not the tag) and the local Claritas clone,
# offline like Flathub, and installs it for this user:
#
#   packaging/flatpak/build-local.sh          # → flatpak run io.github.claritas_audio.Canalis
#   packaging/flatpak/build-local.sh --lint   # also flatpak-builder-lint on the manifest
#
# Needs `flatpak install --user flathub org.flatpak.Builder` and the runtimes the manifest
# names (flatpak-builder installs them with --install-deps-from=flathub).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
claritas="${CLARITAS_SRC:-${XDG_CACHE_HOME:-$HOME/.cache}/canalis/claritas-src}"
app=io.github.claritas_audio.Canalis
work="$here/target/flatpak"
manifest="$work/$app.yml"
mkdir -p "$work"

# The same manifest with the two git sources swapped for this checkout and the local clone.
python3 - "$here/packaging/flatpak/$app.yml" "$manifest" "$here" "$claritas" <<'EOF'
import re, sys
src, out, repo, claritas = sys.argv[1:]
text = open(src).read()
text = re.sub(r"      - type: git\n        url: https://github.com/claritas-audio/canalis.git\n        tag: .*\n(        commit: .*\n)?",
              f"      - type: dir\n        path: {repo}\n        skip: [target, .git]\n", text)
text = text.replace("url: https://github.com/claritas-audio/claritas.git", f"url: file://{claritas}")
text = re.sub(r"^(\s+- )([\w-]+-cargo-sources\.json|cargo-sources\.json)$",
              lambda m: f"{m.group(1)}{repo}/packaging/flatpak/{m.group(2)}", text, flags=re.M)
open(out, "w").write(text)
EOF

if [ "${1:-}" = --lint ]; then
  flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest "$here/packaging/flatpak/$app.yml" || true
fi
flatpak run org.flatpak.Builder --user --install --force-clean --install-deps-from=flathub \
  --state-dir="$work/state" --repo="$work/repo" "$work/build" "$manifest"
if [ "${1:-}" = --lint ]; then
  flatpak run --command=flatpak-builder-lint org.flatpak.Builder repo "$work/repo" || true
fi
echo "flatpak run $app"
