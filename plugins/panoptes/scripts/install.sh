#!/bin/sh
# Install the panoptes Rex module to $REX_EXTRAS_HOME/lua/rex_extras/panoptes.lua. Updating is
# running it again. It never edits init.lua: it prints the lines to add.
#
#   REX_EXTRAS_HOME=   install root (default ~/.local/share/rex-extras)
set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
EXTRAS="${REX_EXTRAS_HOME:-$HOME/.local/share/rex-extras}"
LUA_DIR="$EXTRAS/lua/rex_extras"

say() { printf 'rex-panoptes: %s\n' "$*" >&2; }

command -v panoptes >/dev/null 2>&1 || say "warning: panoptes is not on PATH (brew install david-morgan-suffolk/tap/panoptes)"
mkdir -p "$LUA_DIR" && cp "$ROOT/lua/panoptes.lua" "$LUA_DIR/panoptes.lua" || exit 1
say "installed $LUA_DIR/panoptes.lua"

cat <<EOF
Add to init.lua, then run: rex config reload

package.path = "$EXTRAS/lua/?.lua;" .. package.path
require("rex_extras.panoptes").setup{ keys = { ["ctrl+a>alt+o"] = "panoptes_open", ["ctrl+a>alt+shift+o"] = "panoptes_open_tab" } }
EOF
