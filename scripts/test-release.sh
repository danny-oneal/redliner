#!/bin/sh
# Tests scripts/release.sh and scripts/changelog-section.sh against a copy of
# the workspace manifests and a fixture changelog: a version bump, the release
# notes it produces, and the inputs both scripts refuse. Run: make test-release
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

url=https://github.com/danny-oneal/redliner

# A copy of the real manifests (so a layout change breaks this test) and a
# changelog with something unreleased. The lockfile update is stubbed out.
fixture() {
    rm -rf "$work/ws"
    mkdir -p "$work/ws/crates/redliner" "$work/ws/crates/volley"
    cp "$repo/Cargo.toml" "$work/ws/"
    cp "$repo/crates/redliner/Cargo.toml" "$work/ws/crates/redliner/"
    cp "$repo/crates/volley/Cargo.toml" "$work/ws/crates/volley/"
    cat > "$work/ws/CHANGELOG.md" <<EOF
# Changelog

## [Unreleased]

### Added
- **Compare** — diff two runs.

## [0.1.0] - 2026-10-20

### Added
- First release.

[Unreleased]: $url/compare/v0.1.0...HEAD
[0.1.0]: $url/releases/tag/v0.1.0
EOF
}

release() {
    RELEASE_ROOT="$work/ws" CARGO=true sh "$here/release.sh" "$@"
}

current=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml")
[ -n "$current" ] || fail "no [workspace.package] version in Cargo.toml"

# 1. A bump sets the one workspace version and the engine pin.
fixture
release 9.9.9 > "$work/out.txt" 2>&1 || fail "release failed: $(cat "$work/out.txt")"
grep -q '^version = "9.9.9"$' "$work/ws/Cargo.toml" || fail "workspace version not bumped"
grep -q '^volley = .*version = "=9.9.9"' "$work/ws/crates/redliner/Cargo.toml" ||
    fail "engine dependency not pinned to 9.9.9"
for crate in redliner volley; do
    grep -q '^version\.workspace = true$' "$work/ws/crates/$crate/Cargo.toml" ||
        fail "crates/$crate does not inherit the workspace version"
done
echo "ok - bumps the workspace version and the engine pin"

# 2. ...and moves Unreleased under the new version, with its links.
today=$(date +%Y-%m-%d)
grep -q "^## \[9.9.9\] - $today\$" "$work/ws/CHANGELOG.md" || fail "no dated 9.9.9 heading"
[ "$(grep -c '^## \[Unreleased\]$' "$work/ws/CHANGELOG.md")" = 1 ] || fail "Unreleased heading not kept"
grep -qF "[Unreleased]: $url/compare/v9.9.9...HEAD" "$work/ws/CHANGELOG.md" || fail "Unreleased link not moved"
grep -qF "[9.9.9]: $url/compare/v0.1.0...v9.9.9" "$work/ws/CHANGELOG.md" || fail "no 9.9.9 link"
echo "ok - moves Unreleased under a dated version heading"

# 3. The release notes are that section's body: no heading, no next section.
notes=$(sh "$here/changelog-section.sh" 9.9.9 "$work/ws/CHANGELOG.md") || fail "no notes for 9.9.9"
expected=$(printf '### Added\n- **Compare** — diff two runs.')
[ "$notes" = "$expected" ] || fail "wrong 9.9.9 notes: $notes"
notes=$(sh "$here/changelog-section.sh" 0.1.0 "$work/ws/CHANGELOG.md") || fail "no notes for 0.1.0"
[ "$notes" = "$(printf '### Added\n- First release.')" ] || fail "0.1.0 notes ran into the links: $notes"
echo "ok - extracts one version's notes"

# 4. No notes, no release: a missing or empty section fails.
if sh "$here/changelog-section.sh" 9.9 "$work/ws/CHANGELOG.md" > /dev/null 2>&1; then
    fail "notes for a prefix of a version"
fi
if sh "$here/changelog-section.sh" Unreleased "$work/ws/CHANGELOG.md" > /dev/null 2>&1; then
    fail "notes for an empty section"
fi
echo "ok - refuses a missing or empty section"

# 5. Bad versions are refused and leave the tree untouched.
for bad in 1.2 v1.2.3 0.0.1 "$current" minorr Patch ""; do
    fixture
    if release "$bad" > "$work/out.txt" 2>&1; then
        fail "accepted version $bad"
    fi
    cmp -s "$repo/Cargo.toml" "$work/ws/Cargo.toml" || fail "version $bad changed Cargo.toml"
done
echo "ok - refuses a malformed, older or unchanged version"

# 6. major / minor / patch bump that part of the current version, through
#    the make targets.
major=${current%%.*}
rest=${current#*.}
minor=${rest%%.*}
patch=${rest#*.}
for bump in "major $((major + 1)).0.0" "minor $major.$((minor + 1)).0" "patch $major.$minor.$((patch + 1))"; do
    part=${bump% *}
    want=${bump#* }
    fixture
    RELEASE_ROOT="$work/ws" CARGO=true make -s -C "$repo" "release-$part" > "$work/out.txt" 2>&1 ||
        fail "make release-$part failed: $(cat "$work/out.txt")"
    grep -q "^version = \"$want\"\$" "$work/ws/Cargo.toml" || fail "release-$part did not bump to $want"
    grep -q "^## \[$want\] - " "$work/ws/CHANGELOG.md" || fail "release-$part did not date $want"
done
echo "ok - major, minor and patch bump the current version"

# 7. Nothing unreleased, nothing to release.
fixture
release 9.9.9 > /dev/null 2>&1
if release 9.9.10 > "$work/out.txt" 2>&1; then
    fail "released an empty Unreleased section"
fi
grep -q "Unreleased" "$work/out.txt" || fail "no reason given: $(cat "$work/out.txt")"
echo "ok - refuses an empty release"
