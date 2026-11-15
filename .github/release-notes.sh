#!/bin/sh
# Print the section of CHANGELOG.md for one version, for the release workflow to use as the release notes.
# Usage (run from the repo root): .github/release-notes.sh v0.1.0
#
# It also checks two things that must hold before a release, and exits non-zero when either fails:
#   1. the tag matches the version in Cargo.toml (the binary's --version prints the Cargo.toml version)
#   2. CHANGELOG.md has a section for this version, and it is not empty
set -eu

tag=${1:?usage: release-notes.sh <tag>, e.g. v0.1.0}
version=${tag#v}

cargo_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
if [ "$version" != "$cargo_version" ]; then
    echo "tag $tag does not match the version in Cargo.toml ($cargo_version)" >&2
    exit 1
fi

# Start on the line after "## [version]" and print up to the next "## [" heading or the link definitions at the end of the file
notes=$(awk -v head="## [$version]" '
    index($0, "## [") == 1 { if (found) exit; found = index($0, head) == 1; next }
    found && /^\[[^]]*\]: / { exit }
    found
' CHANGELOG.md)

if ! printf '%s' "$notes" | grep -q '[^[:space:]]'; then
    echo "CHANGELOG.md has no section for $tag" >&2
    exit 1
fi

printf '%s\n' "$notes"
