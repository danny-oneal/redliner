#!/bin/sh
# Installs a prebuilt redliner binary from a GitHub release, after checking it
# against the release's SHA-256 checksum.
#
#   curl -fsSL https://raw.githubusercontent.com/danny-oneal/redliner/main/scripts/install.sh | sh
#
# Environment:
#   REDLINER_VERSION       release tag to install (default: the newest release)
#   REDLINER_INSTALL_DIR   where to put the binary (default: $HOME/.local/bin)
#   REDLINER_DOWNLOAD_URL  base URL of the release assets (default: GitHub)
set -eu

repo="danny-oneal/redliner"

die() {
    echo "redliner-install: $*" >&2
    exit 1
}

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q "$1" -O "$2"; }
else
    die "needs curl or wget"
fi

if command -v sha256sum >/dev/null 2>&1; then
    check_sha256() { sha256sum -c "$1" >/dev/null 2>&1; }
elif command -v shasum >/dev/null 2>&1; then
    check_sha256() { shasum -a 256 -c "$1" >/dev/null 2>&1; }
else
    die "needs sha256sum or shasum to verify the download"
fi

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
    Linux/x86_64) target=x86_64-unknown-linux-musl ;;
    Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    *) die "no prebuilt binary for $os/$arch; build from source: cargo install --git https://github.com/$repo redliner --locked" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

version=${REDLINER_VERSION:-}
if [ -z "$version" ]; then
    # The list endpoint, not /releases/latest: that one skips pre-releases.
    fetch "https://api.github.com/repos/$repo/releases?per_page=1" "$tmp/releases.json" ||
        die "could not look up the newest release; set REDLINER_VERSION"
    version=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp/releases.json" | head -n 1)
    [ -n "$version" ] || die "no releases found for $repo"
fi

base=${REDLINER_DOWNLOAD_URL:-https://github.com/$repo/releases/download/$version}
asset="redliner-$version-$target.tar.gz"

echo "redliner-install: downloading $asset"
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" ||
    die "no checksum published for $asset; refusing to install an unverified binary"
(cd "$tmp" && check_sha256 "$asset.sha256") || die "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp" redliner
dir=${REDLINER_INSTALL_DIR:-$HOME/.local/bin}
mkdir -p "$dir"
cp "$tmp/redliner" "$dir/redliner.tmp"
chmod 755 "$dir/redliner.tmp"
mv "$dir/redliner.tmp" "$dir/redliner"

echo "redliner-install: installed redliner $version to $dir/redliner"
case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "redliner-install: note: $dir is not on your PATH" ;;
esac
