#!/bin/sh
# Tests scripts/install.sh against a fake release served from localhost:
# a good install, a tampered archive (checksum mismatch), and a missing
# checksum file. Needs python3 for the HTTP server. Run: make test-install
set -eu

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
server_pid=""
cleanup() {
    if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; fi
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) target=x86_64-unknown-linux-musl ;;
    Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    *) fail "no test target for $(uname -s)/$(uname -m)" ;;
esac

version=v9.9.9
asset="redliner-$version-$target.tar.gz"

# A fake release: the "binary" is a script that prints a marker.
mkdir -p "$work/pkg" "$work/release/$version"
printf '#!/bin/sh\necho "redliner fake %s"\n' "$version" > "$work/pkg/redliner"
chmod +x "$work/pkg/redliner"
tar -czf "$work/release/$version/$asset" -C "$work/pkg" redliner
(
    cd "$work/release/$version"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$asset" > "$asset.sha256"
    else
        shasum -a 256 "$asset" > "$asset.sha256"
    fi
)

port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$work/release" >/dev/null 2>&1 &
server_pid=$!
i=0
until python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:$port/')" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || fail "fake release server did not start"
    sleep 0.1
done

install_into() {
    REDLINER_VERSION=$version \
    REDLINER_DOWNLOAD_URL="http://127.0.0.1:$port/$version" \
    REDLINER_INSTALL_DIR="$1" \
        sh "$here/install.sh"
}

# 1. A good release installs an executable binary.
install_into "$work/bin" > "$work/out.txt" 2>&1 || fail "install failed: $(cat "$work/out.txt")"
[ -x "$work/bin/redliner" ] || fail "binary not installed"
[ "$("$work/bin/redliner")" = "redliner fake $version" ] || fail "wrong binary installed"
grep -q "installed redliner $version" "$work/out.txt" || fail "no success message: $(cat "$work/out.txt")"
echo "ok - installs a verified release"

# 2. A tampered archive is rejected and nothing is installed.
cp "$work/release/$version/$asset" "$work/good.tar.gz"
printf 'tampered' >> "$work/release/$version/$asset"
if install_into "$work/bin2" > "$work/out.txt" 2>&1; then
    fail "tampered archive was installed"
fi
[ ! -e "$work/bin2/redliner" ] || fail "tampered binary left behind"
grep -q "checksum" "$work/out.txt" || fail "no checksum error: $(cat "$work/out.txt")"
echo "ok - rejects a checksum mismatch"
cp "$work/good.tar.gz" "$work/release/$version/$asset"

# 3. A release without a checksum file is rejected, not trusted blindly.
rm "$work/release/$version/$asset.sha256"
if install_into "$work/bin3" > "$work/out.txt" 2>&1; then
    fail "installed without a checksum"
fi
[ ! -e "$work/bin3/redliner" ] || fail "unverified binary left behind"
echo "ok - refuses a release without a checksum"
