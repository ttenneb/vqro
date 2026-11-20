#!/bin/sh
# Build vqro and install it to ~/.local/bin for day-to-day testing. Not a release process.
# Installs everything first, then restarts a running core, so an open `vqro tui` (which
# reconnects and restarts the core by itself) always comes back on the new binary.
set -e
cd "$(dirname "$0")/.."
cargo build --release
# Copy from the directory cargo just built into (a per-agent CARGO_TARGET_DIR included), and check
# every binary is there before anything is installed, so a failure never leaves a half install.
TARGET="${CARGO_TARGET_DIR:-target}"
for bin in vqro-herdr vqro-tmux vqro-bridge-pi vqro; do
  [ -x "$TARGET/release/$bin" ] || { echo "dev-install: $TARGET/release/$bin missing; nothing installed" >&2; exit 1; }
done
# Bundled extensions: the core installs the ones in defaults.json on first run.
mkdir -p "$HOME/.local/share/vqro/extensions" "$HOME/.local/bin"
cp extensions/*.py extensions/defaults.json "$HOME/.local/share/vqro/extensions/"
mkdir -p "$HOME/.local/share/vqro/extensions/manifests"
cp extensions/manifests/*.json "$HOME/.local/share/vqro/extensions/manifests/"
# UI clients for `vqro ui <name>` (client manifests and programs; the built-in TUI needs none).
mkdir -p "$HOME/.local/share/vqro/extensions/clients"
# The OpenTUI trial client (clients/opentui, needs Bun and its packages) is not installed; run it from the repo.
cp extensions/clients/*.py extensions/clients/text.json "$HOME/.local/share/vqro/extensions/clients/"
# Python extension packages live beside helpers. Only pi_bridge is replaced as
# a whole: it is installed code, never session/config data; old modules must go.
for source in extensions/*; do
  [ -d "$source" ] && [ -f "$source/__init__.py" ] || continue
  package=${source##*/}
  destination="$HOME/.local/share/vqro/extensions/$package"
  if [ "$package" = pi_bridge ]; then
    rm -rf "$HOME/.local/share/vqro/extensions/pi_bridge"
  fi
  mkdir -p "$destination"
  cp "$source"/*.py "$destination/"
done
# Atomic replace: a running binary keeps its old inode. The provider programs (backends run as
# extensions: vqro-herdr, vqro-tmux) go next to vqro, where the default extensions find them.
for bin in vqro-herdr vqro-tmux vqro-bridge-pi vqro; do
  install -m 755 "$TARGET/release/$bin" "$HOME/.local/bin/.$bin.new"
  mv -f "$HOME/.local/bin/.$bin.new" "$HOME/.local/bin/$bin"
  cmp -s "$TARGET/release/$bin" "$HOME/.local/bin/$bin" || { echo "dev-install: $bin differs from $TARGET/release/$bin after install" >&2; exit 1; }
done
echo "installed $("$HOME/.local/bin/vqro" --version) to ~/.local/bin/vqro"
if "$HOME/.local/bin/vqro" status 2>/dev/null | grep -q '^running'; then
  # UIs too old to restart themselves need a manual restart; say so.
  "$HOME/.local/bin/vqro" status 2>/dev/null | grep 'quit and reopen' || true
  "$HOME/.local/bin/vqro" stop || true
  echo "restarted the core: an open 'vqro tui' brings it back on the new binary (terminals keep running)"
fi
