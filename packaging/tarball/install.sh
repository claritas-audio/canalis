#!/bin/sh
# Copies Canalis to ~/.local/lib/canalis (or $PREFIX/lib/canalis), links bin/canalis into
# ~/.local/bin and adds the launcher entry. `./install.sh --uninstall` undoes it.
# The same layout as `install.sh --user` from https://claritas.meltingbots.com/install.sh.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
prefix="${PREFIX:-$HOME/.local}"
dest="$prefix/lib/canalis"

if [ "${1:-}" = --uninstall ]; then
  if [ -x "$dest/bin/canalis" ]; then
    "$dest/bin/canalis" quit 2>/dev/null || true
    "$dest/bin/canalis" --uninstall
  fi
  rm -f "$prefix/bin/canalis"
  rm -rf "$dest"
  echo "Canalis removed. Your settings stay in ~/.config/canalis."
  exit 0
fi

# Replaced, not overwritten: a running Canalis keeps its old files until it restarts.
rm -rf "$dest.new"
mkdir -p "$dest.new" "$prefix/bin"
cp -R "$here/bin" "$here/lib" "$here/share" "$dest.new/"
rm -rf "$dest"
mv "$dest.new" "$dest"
ln -sf "$dest/bin/canalis" "$prefix/bin/canalis"
"$dest/bin/canalis" --install

echo "Canalis installed in $dest."
case ":$PATH:" in
  *":$prefix/bin:"*) echo "Run: canalis" ;;
  *) echo "Run: $prefix/bin/canalis   ($prefix/bin is on PATH after you log in again)" ;;
esac
