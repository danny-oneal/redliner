#!/bin/sh
# Prepares a release: sets the workspace version (and redliner's exact pin on
# the engine), moves the changelog's Unreleased section under the new version
# with today's date, and refreshes Cargo.lock. Commit the result as
# "chore(release): <version>" in a PR; when it merges, the release workflow
# tags v<version>, builds every target and publishes the GitHub release.
#
# Usage: scripts/release.sh <version>    (or: make release VERSION=<version>)
set -eu

die() {
    echo "release: $*" >&2
    exit 1
}

version=${1:-}
printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
    die "usage: release.sh X.Y.Z (got '${version}')"

# RELEASE_ROOT and CARGO let scripts/test-release.sh run this on a copy.
cd "${RELEASE_ROOT:-$(dirname "$0")/..}"
cargo=${CARGO:-cargo}

current=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"$/\1/p' Cargo.toml)
[ -n "$current" ] || die "no [workspace.package] version in Cargo.toml"

# Newer than the current version, comparing major, minor, patch as numbers.
newer=$(printf '%s %s\n' "$version" "$current" | awk '{
    split($1, a, "."); split($2, b, ".")
    for (i = 1; i <= 3; i++) {
        if (a[i] + 0 > b[i] + 0) { print "yes"; exit }
        if (a[i] + 0 < b[i] + 0) { print "no"; exit }
    }
    print "no"
}')
[ "$newer" = yes ] || die "$version is not newer than the current $current"

[ -n "$(sh "$(dirname "$0")/changelog-section.sh" Unreleased CHANGELOG.md 2>/dev/null)" ] ||
    die "nothing under ## [Unreleased] in CHANGELOG.md to release"

# The previous release, from the Unreleased compare link (v0.1.0...HEAD).
previous=$(sed -n 's/^\[Unreleased\]: .*\/compare\/\(v[^.]*\.[^.]*\.[^.]*\)\.\.\.HEAD$/\1/p' CHANGELOG.md)
[ -n "$previous" ] || die "no '[Unreleased]: .../compare/vX.Y.Z...HEAD' link in CHANGELOG.md"

# Rewrites a file through a filter, in place (sed -i differs on GNU and BSD).
rewrite() {
    file=$1
    shift
    "$@" < "$file" > "$file.tmp"
    mv "$file.tmp" "$file"
}

rewrite Cargo.toml sed "/^\[workspace\.package\]/,/^\[/ s/^version = \".*\"\$/version = \"$version\"/"
rewrite crates/redliner/Cargo.toml sed "/^volley = /s/version = \"=[^\"]*\"/version = \"=$version\"/"
grep -q "^volley = .*version = \"=$version\"" crates/redliner/Cargo.toml ||
    die "could not pin the engine dependency in crates/redliner/Cargo.toml"

today=$(date +%Y-%m-%d)
rewrite CHANGELOG.md awk -v v="$version" -v today="$today" -v prev="$previous" '
    $0 == "## [Unreleased]" { print; print ""; print "## [" v "] - " today; next }
    /^\[Unreleased\]: / {
        base = $2
        sub(/\/compare\/.*/, "", base)
        print "[Unreleased]: " base "/compare/v" v "...HEAD"
        print "[" v "]: " base "/compare/" prev "...v" v
        next
    }
    { print }
'

"$cargo" update --workspace --quiet

echo "release: $current -> $version. Review the diff, then commit it as"
echo "  chore(release): $version"
echo "in a PR. Merging it tags v$version and publishes the release."
