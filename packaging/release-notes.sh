#!/bin/sh
# Prints the GitHub release notes for VERSION: its section of CHANGELOG.md,
# then the install instructions. A prerelease (0.2.0-rc.1) uses the section
# of the version it leads to (0.2.0).
#
#   packaging/release-notes.sh 0.2.0 >notes.md

set -eu

version=${1:?usage: release-notes.sh VERSION}
base=${version%%-*}

notes=$(awk -v head="## [$base]" '
    /^## \[/ { if (found) exit; if (index($0, head) == 1) { found = 1; next } }
    found && (started || NF) { started = 1; print }
' CHANGELOG.md)
if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
    echo "release-notes.sh: CHANGELOG.md has no section \"## [$base]\"" >&2
    exit 1
fi

printf '%s\n\n' "$notes"
sed "s/{{VERSION}}/$version/g" packaging/release-notes-install.md
