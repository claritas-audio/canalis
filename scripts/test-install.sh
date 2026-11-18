#!/usr/bin/env bash
# install.sh against a local copy of the releases, in clean podman containers: install an
# older release, upgrade to the latest, check `canalis --version`, uninstall.
#
#   scripts/package-container.sh && scripts/package-container.sh rpm   # target/dist
#   scripts/test-install.sh [image...]    # default: every image below
#
# The packages in target/dist are served as v<version>, and repacked under the next patch
# version (same binaries, only the package version changes) as the latest.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)"
next="${version%.*}.$(( ${version##*.} + 1 ))"
dist="$here/target/dist"
root="$here/target/install-test"
port="${PORT:-8765}"
images=("$@")
[ ${#images[@]} -gt 0 ] || images=(fedora:43 fedora:44 ubuntu:24.04 debian:13 archlinux ubuntu:24.04/user)

for f in "canalis-$version-1.x86_64.rpm" "canalis_${version}-1_amd64.deb" "canalis-$version-x86_64.tar.gz"; do
  [ -f "$dist/$f" ] || { echo "missing $dist/$f (scripts/package-container.sh [rpm])"; exit 1; }
done

nfpm_version="$(sed -n 's/^  NFPM_VERSION: //p' "$here/.github/workflows/release.yml")"
nfpm_sha256="$(sed -n 's/^  NFPM_SHA256: //p' "$here/.github/workflows/release.yml")"
nfpm="$here/target/tools/nfpm"
if [ ! -x "$nfpm" ]; then
  mkdir -p "$here/target/tools"
  curl -sSfL -o "$here/target/tools/nfpm.tgz" "https://github.com/goreleaser/nfpm/releases/download/v${nfpm_version}/nfpm_${nfpm_version}_Linux_x86_64.tar.gz"
  echo "$nfpm_sha256  $here/target/tools/nfpm.tgz" | sha256sum -c - >/dev/null
  tar -xzf "$here/target/tools/nfpm.tgz" -C "$here/target/tools" nfpm
fi

rm -rf "$root"
old="$root/releases/download/v$version" new="$root/releases/download/v$next"
mkdir -p "$old" "$new"
cp "$dist/canalis-$version-1.x86_64.rpm" "$dist/canalis_${version}-1_amd64.deb" "$dist/canalis-$version-x86_64.tar.gz" "$old/"
(cd "$here" && for format in rpm deb; do VERSION="$next" "$nfpm" package -f packaging/nfpm.yaml -p "$format" -t "$new/" >/dev/null; done)
tar -C "$root" -xzf "$old/canalis-$version-x86_64.tar.gz"
mv "$root/canalis-$version-x86_64" "$root/canalis-$next-x86_64"
tar -C "$root" -czf "$new/canalis-$next-x86_64.tar.gz" "canalis-$next-x86_64"
rm -r "$root/canalis-$next-x86_64"
for d in "$old" "$new"; do (cd "$d" && sha256sum -- * > SHA256SUMS); done
# A release whose files do not match its SHA256SUMS: the script must install nothing.
bad="$root/releases/download/v0.0.1"
mkdir -p "$bad"
for f in canalis-0.0.1-1.x86_64.rpm canalis_0.0.1-1_amd64.deb canalis-0.0.1-x86_64.tar.gz; do
  echo tampered > "$bad/$f"
  echo "0000000000000000000000000000000000000000000000000000000000000000  $f" >> "$bad/SHA256SUMS"
done
printf '{\n  "tag_name": "v%s",\n  "name": "Canalis %s"\n}\n' "$next" "$next" > "$root/latest.json"

python3 -m http.server -d "$root" -b 127.0.0.1 "$port" >/dev/null 2>&1 &
server=$!
trap 'kill $server' EXIT
sleep 1

# Runs in the container: what a desktop has before the script (curl; PipeWire and the GL,
# Wayland and X11 libraries for the tarball path), then the install, upgrade and uninstall.
# shellcheck disable=SC2016
check='
set -eu
case "$IMAGE" in
  fedora*) dnf install -y -q curl >/dev/null ;;
  ubuntu*|debian*) apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates sudo \
    libpipewire-0.3-0t64 libegl1 libgl1 libwayland-client0 libwayland-cursor0 libwayland-egl1 libxkbcommon0 \
    libxkbcommon-x11-0 libx11-6 libx11-xcb1 libxcursor1 libxi6 >/dev/null ;;
  archlinux*) pacman -Sy --noconfirm --needed -q libpipewire libglvnd wayland libxkbcommon libxkbcommon-x11 \
    libx11 libxcursor libxi >/dev/null ;;
esac
run() {
  if [ -n "${AS_USER:-}" ]; then
    su tester -c "CANALIS_RELEASES=$CANALIS_RELEASES CANALIS_LATEST=$CANALIS_LATEST sh /install.sh $*"
  else
    sh /install.sh "$@"
  fi
}
as() { if [ -n "${AS_USER:-}" ]; then su tester -c "$*"; else sh -c "$*"; fi; }
[ -z "${AS_USER:-}" ] || useradd -m tester
echo "--- install $OLD"
run --version "$OLD" $FLAGS
as "PATH=\$HOME/.local/bin:/usr/bin:/bin canalis --version" | grep -qx "canalis $OLD"
echo "--- upgrade to the latest ($NEW)"
# Fedora, Debian and Ubuntu take the latest from COPR and the APT repository, tested on their
# own; here they upgrade through the release file.
case "$IMAGE:$FLAGS" in
  fedora*:|ubuntu*:|debian*:) run --version "$NEW" ;;
  *) run $FLAGS ;;
esac
case "$IMAGE:$FLAGS" in
  fedora*:) rpm -q canalis | grep -q "canalis-$NEW-1" ;;
  ubuntu*:|debian*:) dpkg-query -W -f "\${Version}" canalis | grep -qx "$NEW-1" ;;
  *) as "test -x \$HOME/.local/lib/canalis/bin/canalis-chain && test -f \$HOME/.local/share/applications/io.github.claritas_audio.Canalis.desktop" ;;
esac
as "PATH=\$HOME/.local/bin:/usr/bin:/bin canalis --version" | grep -q "^canalis "
as "PATH=\$HOME/.local/bin:/usr/bin:/bin; d=\$(dirname \$(readlink -f \$(command -v canalis))); \$d/canalis-chain --scan \"\$d/../lib/clap/Claritas EQ.clap\"" | grep -q audio.claritas.eq
echo "--- uninstall"
run --uninstall
if as "PATH=\$HOME/.local/bin:/usr/bin:/bin command -v canalis"; then echo "canalis still installed"; exit 1; fi
as "test ! -e \$HOME/.local/lib/canalis"
echo "--- a download that does not match SHA256SUMS installs nothing"
if run --version 0.0.1 $FLAGS; then echo "a tampered release was installed"; exit 1; fi
if as "PATH=\$HOME/.local/bin:/usr/bin:/bin command -v canalis"; then echo "canalis installed from a tampered release"; exit 1; fi
'

failed=()
for spec in "${images[@]}"; do
  image="${spec%/user}" flags="" as_user=""
  [ "$spec" = "$image" ] || { flags=--user; as_user=1; }
  echo "=== $spec"
  if podman run --rm --network host \
      -v "$here/install.sh:/install.sh:ro,z" \
      -e IMAGE="$image" -e OLD="$version" -e NEW="$next" -e FLAGS="$flags" -e AS_USER="$as_user" \
      -e CANALIS_RELEASES="http://127.0.0.1:$port/releases" -e CANALIS_LATEST="http://127.0.0.1:$port/latest.json" \
      "docker.io/library/$image" sh -c "$check"; then
    echo "=== $spec: ok"
  else
    echo "=== $spec: FAILED"; failed+=("$spec")
  fi
done
[ ${#failed[@]} -eq 0 ] || { echo "failed: ${failed[*]}"; exit 1; }
echo "all passed: ${images[*]}"
