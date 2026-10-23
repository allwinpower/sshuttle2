#!/bin/sh
# Prints the AUR PKGBUILD of sshuttle2-bin for a release, with the SHA-256
# of each Linux tarball taken from the release's SHA256SUMS:
#
#   packaging/aur/pkgbuild.sh 0.2.0 SHA256SUMS >PKGBUILD

set -eu

version=${1:?usage: pkgbuild.sh VERSION SHA256SUMS}
sums=${2:?usage: pkgbuild.sh VERSION SHA256SUMS}
case $version in
    *-*)
        echo "pkgbuild.sh: $version is a prerelease; the AUR gets final releases only" >&2
        exit 1
        ;;
esac
template=$(dirname "$0")/PKGBUILD.in

sum() {
    s=$(sed -n "s/^\([0-9a-f]\{64\}\)  sshuttle2-$version-$1\.tar\.gz\$/\1/p" "$sums")
    if [ -z "$s" ]; then
        echo "pkgbuild.sh: $sums has no sshuttle2-$version-$1.tar.gz" >&2
        exit 1
    fi
    echo "$s"
}

linux_amd64=$(sum linux-amd64)
linux_arm64=$(sum linux-arm64)

sed -e "s|{{VERSION}}|$version|g" \
    -e "s|{{SHA256_LINUX_AMD64}}|$linux_amd64|" \
    -e "s|{{SHA256_LINUX_ARM64}}|$linux_arm64|" \
    "$template"
