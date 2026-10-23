#!/bin/sh
# Installs a release build inside a fresh Linux container and runs it: the
# .deb with apt, the .rpm with dnf or the Arch package with pacman, and
# install.sh, as root and with --user. The release workflow runs it for
# several distributions:
#
#   docker run --rm -v "$PWD/dist:/dist:ro" -v "$PWD/packaging:/packaging:ro" \
#       -e VERSION=0.2.0 IMAGE sh /packaging/install-test.sh

set -eu

dist=/dist
: "${VERSION:?set VERSION}"

check() {
    out=$("$1" --version)
    [ "$out" = "sshuttle2 $VERSION" ] || { echo "$1 --version printed: $out" >&2; exit 1; }
    echo "ok: $1 ($out)"
}

if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$dist/sshuttle2_${VERSION}_amd64.deb" curl >/dev/null
    check /usr/bin/sshuttle2
    apt-get remove -y -qq sshuttle2 >/dev/null
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y -q "$dist/sshuttle2-${VERSION}-1.x86_64.rpm"
    check /usr/bin/sshuttle2
    dnf remove -y -q sshuttle2
elif command -v pacman >/dev/null 2>&1; then
    case $VERSION in
        *-*) echo "no Arch package for a prerelease" ;;
        *)
            pacman -Sy --noconfirm >/dev/null
            pacman -U --noconfirm "$dist/sshuttle2-${VERSION}-1-x86_64.pkg.tar.zst" >/dev/null
            check /usr/bin/sshuttle2
            pacman -R --noconfirm sshuttle2 >/dev/null
            ;;
    esac
elif command -v apk >/dev/null 2>&1; then
    apk add -q curl
fi

export SSHUTTLE2_RELEASE_URL=file://$dist
sh "$dist/install.sh"
check /usr/local/bin/sshuttle2
sh "$dist/install.sh" --user
check "$HOME/.local/bin/sshuttle2"
