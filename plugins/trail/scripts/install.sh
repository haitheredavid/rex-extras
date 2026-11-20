#!/bin/sh
# Install rex-trail: the binary to $REX_EXTRAS_HOME/trail/bin/rex-trail and the Rex Lua module
# to $REX_EXTRAS_HOME/lua/rex_extras/trail.lua. Updating is running it again.
#
# It downloads the prebuilt binary of Cargo.toml's version from the GitHub release, checks it
# against the release's SHA256SUMS, and falls back to `cargo build --release` (Rust 1.89+) when
# there is no binary for this machine or the download or the check fails. It never edits
# init.lua: it prints the lines to add.
#
#   REX_TRAIL_BUILD=source     skip the download and build with cargo
#   REX_TRAIL_DOWNLOAD_BASE=   release download URL to use instead of GitHub's
#                              (the directory holding the assets)
#   REX_EXTRAS_HOME=           install root (default ~/.local/share/rex-extras)
set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
cd "$ROOT" || exit 1
EXTRAS="${REX_EXTRAS_HOME:-$HOME/.local/share/rex-extras}"
BIN_DIR="$EXTRAS/trail/bin"
TARGET="$BIN_DIR/rex-trail"
LUA_DIR="$EXTRAS/lua/rex_extras"
REPO_URL="https://github.com/haitheredavid/rex-extras"

say() { printf 'rex-trail: %s\n' "$*" >&2; }

version=$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
    say "cannot read the version from Cargo.toml"
    exit 1
fi

asset_name() {
    case "$(uname -s)" in
        Linux) os=linux ;;
        Darwin) os=macos ;;
        *) return 1 ;;
    esac
    arch=$(uname -m)
    # A shell under Rosetta reports x86_64 on Apple silicon; take the native build.
    if [ "$os" = macos ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
        arch=arm64
    fi
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) return 1 ;;
    esac
    printf 'rex-trail-%s-%s\n' "$os" "$arch"
}

fetch() { # url destination
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 2 --connect-timeout 15 --max-time 300 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -T 30 --tries=2 -O "$2" "$1"
    else
        say "neither curl nor wget is available"
        return 1
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        say "neither sha256sum nor shasum is available"
        return 1
    fi
}

# Move a file into place in one rename, so a running trail or recorder never sees half a binary.
install_binary() { # source
    mkdir -p "$BIN_DIR" || return 1
    cp "$1" "$TARGET.tmp.$$" && chmod 755 "$TARGET.tmp.$$" && mv -f "$TARGET.tmp.$$" "$TARGET" || {
        rm -f "$TARGET.tmp.$$"
        return 1
    }
}

# Copy a verified download next to its target and run it there rather than in $TMPDIR, which may
# be mounted noexec; only a binary reporting Cargo.toml's version is moved into place.
install_checked() { # source
    mkdir -p "$BIN_DIR" || return 1
    tmpbin="$BIN_DIR/.rex-trail.tmp.$$"
    if ! cp "$1" "$tmpbin" || ! chmod 755 "$tmpbin"; then
        rm -f "$tmpbin"
        return 1
    fi
    reported=$("$tmpbin" --version 2>/dev/null) || {
        rm -f "$tmpbin"
        say "the downloaded $asset does not run on this machine"
        return 1
    }
    if [ "$reported" != "rex-trail $version" ]; then
        rm -f "$tmpbin"
        say "downloaded binary reports '$reported', expected 'rex-trail $version'"
        return 1
    fi
    mv -f "$tmpbin" "$TARGET" || {
        rm -f "$tmpbin"
        return 1
    }
}

download() {
    asset=$(asset_name) || {
        say "no prebuilt binary for $(uname -s) $(uname -m)"
        return 1
    }
    base="${REX_TRAIL_DOWNLOAD_BASE:-$REPO_URL/releases/download/trail-v$version}"
    case "$base" in
        https://* | file://*) ;;
        *) say "warning: REX_TRAIL_DOWNLOAD_BASE is not https: $base" ;;
    esac
    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t rex-trail) || return 1
    say "downloading $asset $version"
    ok=1
    if fetch "$base/$asset" "$tmp/$asset" && fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"; then
        expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name {print $1; exit}' "$tmp/SHA256SUMS")
        actual=$(sha256 "$tmp/$asset")
        if [ -z "$expected" ]; then
            say "SHA256SUMS has no entry for $asset"
        elif [ "$expected" != "$actual" ]; then
            say "checksum mismatch for $asset (expected $expected, got $actual)"
        elif install_checked "$tmp/$asset"; then
            ok=0
        fi
    else
        say "download failed"
    fi
    rm -rf "$tmp"
    return $ok
}

build_from_source() {
    if ! command -v cargo >/dev/null 2>&1; then
        say "cargo is not installed (building rex-trail needs Rust 1.89 or newer)"
        return 1
    fi
    say "building from source with cargo"
    # target/ is left in place: in a development clone it is the build cache.
    if ! cargo build --release --locked --target-dir target; then
        say "cargo build failed (rex-trail needs Rust 1.89 or newer)"
        return 1
    fi
    install_binary "$ROOT/target/release/rex-trail"
}

install_lua() {
    mkdir -p "$LUA_DIR" || return 1
    cp "$ROOT/lua/trail.lua" "$LUA_DIR/trail.lua.tmp.$$" && mv -f "$LUA_DIR/trail.lua.tmp.$$" "$LUA_DIR/trail.lua" || {
        rm -f "$LUA_DIR/trail.lua.tmp.$$"
        return 1
    }
}

finish() {
    if ! install_lua; then
        say "could not copy lua/trail.lua to $LUA_DIR"
        exit 1
    fi
    say "installed $TARGET"
    say "installed $LUA_DIR/trail.lua"
    # A running recorder holds the lock with the old binary; stop it so `rex config reload`
    # starts the new one.
    if command -v pkill >/dev/null 2>&1 && pkill -f "$TARGET record" 2>/dev/null; then
        say "stopped the running recorder; rex config reload starts the new one"
    fi
    if [ -n "${REX_EXTRAS_HOME:-}" ]; then
        lua_root="\"$EXTRAS/lua/?.lua;\""
        bin_opt=", bin = \"$TARGET\""
    else
        lua_root='os.getenv("HOME") .. "/.local/share/rex-extras/lua/?.lua;"'
        bin_opt=""
    fi
    cat >&2 <<EOF

Add to your init.lua (this script never edits it):

  package.path = $lua_root .. package.path
  require("rex_extras.trail").setup{ keys = { ["ctrl+a>alt+g"] = "trail_open", ["ctrl+a>alt+shift+g"] = "trail_popup" }$bin_opt }

then run:

  rex config reload
EOF
    exit 0
}

if [ "${REX_TRAIL_BUILD:-}" != source ] && download; then
    finish
fi
if build_from_source; then
    finish
fi
say "could not install rex-trail $version: no prebuilt binary could be downloaded"
say "and building from source failed. Install Rust 1.89 or newer"
say "(https://rustup.rs), then run the installer again:"
say "  git clone https://github.com/haitheredavid/rex-extras && sh rex-extras/plugins/trail/scripts/install.sh"
exit 1
