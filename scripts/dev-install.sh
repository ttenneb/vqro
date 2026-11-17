#!/bin/sh
# Build prim and install it to ~/.local/bin for day-to-day testing. Not a release process.
# Installs everything first, then restarts a running core, so an open `prim tui` (which
# reconnects and restarts the core by itself) always comes back on the new binary.
set -e
cd "$(dirname "$0")/.."
cargo build --release
# Bundled extensions: the core installs the ones in defaults.json on first run.
mkdir -p "$HOME/.local/share/prim/extensions" "$HOME/.local/bin"
cp extensions/*.py extensions/defaults.json "$HOME/.local/share/prim/extensions/"
# Python extension packages live beside helpers. Only pi_bridge is replaced as
# a whole: it is installed code, never session/config data; old modules must go.
for source in extensions/*; do
  [ -d "$source" ] && [ -f "$source/__init__.py" ] || continue
  package=${source##*/}
  destination="$HOME/.local/share/prim/extensions/$package"
  if [ "$package" = pi_bridge ]; then
    rm -rf "$HOME/.local/share/prim/extensions/pi_bridge"
  fi
  mkdir -p "$destination"
  cp "$source"/*.py "$destination/"
done
# Atomic replace: a running binary keeps its old inode.
install -m 755 target/release/prim "$HOME/.local/bin/.prim.new"
mv -f "$HOME/.local/bin/.prim.new" "$HOME/.local/bin/prim"
echo "installed $("$HOME/.local/bin/prim" --version) to ~/.local/bin/prim"
if "$HOME/.local/bin/prim" status 2>/dev/null | grep -q '^running'; then
  # UIs too old to restart themselves need a manual restart; say so.
  "$HOME/.local/bin/prim" status 2>/dev/null | grep 'quit and reopen' || true
  "$HOME/.local/bin/prim" stop || true
  echo "restarted the core: an open 'prim tui' brings it back on the new binary (terminals keep running)"
fi
