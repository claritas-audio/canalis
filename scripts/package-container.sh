#!/usr/bin/env bash
# The release packages built locally in podman, on the same images as the release workflow,
# from this checkout and the Claritas clone scripts/bundle-claritas.sh keeps.
#
#   scripts/package-container.sh               # ubuntu:24.04: the tarball and the .deb
#   scripts/package-container.sh rpm           # fedora:43: the .rpm
#   → target/dist/, plus SHA256SUMS over everything there
#
# CLARITAS_SRC   a Claritas checkout (default: ~/.cache/canalis/claritas-src); CLARITAS_REV
#                as in scripts/bundle-claritas.sh.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
claritas="${CLARITAS_SRC:-${XDG_CACHE_HOME:-$HOME/.cache}/canalis/claritas-src}"
rust="$(sed -n 's/^rust = { version = "\([^"]*\)".*/\1/p' "$here/mise.toml")"
nfpm_version="$(sed -n 's/^  NFPM_VERSION: //p' "$here/.github/workflows/release.yml")"
nfpm_sha256="$(sed -n 's/^  NFPM_SHA256: //p' "$here/.github/workflows/release.yml")"
[ -d "$claritas/.git" ] || { echo "no Claritas checkout at $claritas (run scripts/bundle-claritas.sh once)"; exit 1; }

if [ "${1:-}" = rpm ]; then
  image=docker.io/library/fedora:43 formats=rpm
  setup='dnf install -y -q --setopt=install_weak_deps=False gcc git pkgconf-pkg-config curl tar gzip
    clang-devel pipewire-devel mesa-libGL-devel libX11-devel libxcb-devel xcb-util-wm-devel
    libXcursor-devel libxkbcommon-devel libxkbcommon-x11-devel wayland-devel'
else
  image=docker.io/library/ubuntu:24.04 formats="tar deb"
  setup='apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends
    ca-certificates curl git build-essential pkg-config clang libclang-dev libpipewire-0.3-dev
    libgl-dev libx11-dev libx11-xcb-dev libxcb1-dev libxcb-icccm4-dev libxcursor-dev
    libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev >/dev/null'
fi
tag="${image##*/}"
setup="${setup//$'\n'/ }"

podman run --rm \
  -v "$here:/src:z" \
  -v "$claritas:/claritas:ro,z" \
  -v "canalis-pkg-${tag%%:*}-cargo:/root/.cargo" \
  -v "canalis-pkg-${tag%%:*}-rustup:/root/.rustup" \
  -e CLARITAS_SRC=/claritas -e CLARITAS_WORK=/work/claritas \
  -e CARGO_TARGET_DIR="/src/target/${tag/:/-}" \
  -e RUST="$rust" -e NFPM_VERSION="$nfpm_version" -e NFPM_SHA256="$nfpm_sha256" \
  "$image" bash -euo pipefail -c "
    $setup
    [ -x /root/.cargo/bin/cargo ] || curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain \"\$RUST\"
    export PATH=/root/.cargo/bin:\$PATH
    rustup toolchain install --profile minimal \"\$RUST\" >/dev/null
    rustup default \"\$RUST\"
    curl -sSfL -o /tmp/nfpm.tgz https://github.com/goreleaser/nfpm/releases/download/v\${NFPM_VERSION}/nfpm_\${NFPM_VERSION}_Linux_x86_64.tar.gz
    echo \"\$NFPM_SHA256  /tmp/nfpm.tgz\" | sha256sum -c - >/dev/null
    tar -xzf /tmp/nfpm.tgz -C /usr/local/bin nfpm
    cd /src && scripts/package.sh $formats
  "
cd "$here/target/dist"
sums="$(find . -maxdepth 1 -type f ! -name SHA256SUMS -printf '%f\n' | sort | xargs -d '\n' sha256sum)"
printf '%s\n' "$sums" > SHA256SUMS
ls -l "$here/target/dist"
