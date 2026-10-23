#!/bin/sh
# Prints the Homebrew formula for a release, with the SHA-256 of each
# tarball taken from the release's SHA256SUMS:
#
#   packaging/homebrew/formula.sh 0.2.0 dist/SHA256SUMS >sshuttle2.rb
#
# BASE_URL (default: the GitHub release) is where the tarballs are. Homebrew
# reads the version from the URL, so keep a /vVERSION/ directory in it.

set -eu

version=${1:?usage: formula.sh VERSION SHA256SUMS [BASE_URL]}
sums=${2:?usage: formula.sh VERSION SHA256SUMS [BASE_URL]}
base=${3:-https://github.com/allwinpower/sshuttle2/releases/download/v$version}
template=$(dirname "$0")/sshuttle2.rb.in

sum() {
    s=$(sed -n "s/^\([0-9a-f]\{64\}\)  sshuttle2-$version-$1\.tar\.gz\$/\1/p" "$sums")
    if [ -z "$s" ]; then
        echo "formula.sh: $sums has no sshuttle2-$version-$1.tar.gz" >&2
        exit 1
    fi
    echo "$s"
}

macos_arm64=$(sum macos-arm64)
macos_amd64=$(sum macos-amd64)
linux_arm64=$(sum linux-arm64)
linux_amd64=$(sum linux-amd64)

sed -e "s|{{BASE}}|$base|g" \
    -e "s|{{VERSION}}|$version|g" \
    -e "s|{{SHA256_MACOS_ARM64}}|$macos_arm64|" \
    -e "s|{{SHA256_MACOS_AMD64}}|$macos_amd64|" \
    -e "s|{{SHA256_LINUX_ARM64}}|$linux_arm64|" \
    -e "s|{{SHA256_LINUX_AMD64}}|$linux_amd64|" \
    "$template"
