#!/bin/sh
# Prints one version's section of CHANGELOG.md without its heading: the GitHub
# release notes. Fails when the section is missing or empty, so CI can't
# release a version nobody wrote notes for.
#
# Usage: scripts/changelog-section.sh <version> [changelog]
set -eu

version=${1:?usage: changelog-section.sh <version> [changelog]}
file=${2:-CHANGELOG.md}

# From "## [<version>]" up to the next section or the link references, with
# the blank lines at either end dropped.
notes=$(awk -v heading="## [$version]" '
    index($0, heading) == 1 { found = 1; next }
    found && (/^## \[/ || /^\[[^]]*\]: /) { exit }
    found && !started && /^[[:space:]]*$/ { next }
    found { started = 1; print }
' "$file")

if [ -z "$notes" ]; then
    echo "changelog-section: no notes for $version in $file" >&2
    exit 1
fi
printf '%s\n' "$notes"
