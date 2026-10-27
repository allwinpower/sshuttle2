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

# The completions a package installed, and the man page in its file list
# ($1): container images often skip installing man pages.
check_docs() {
    for f in /usr/share/bash-completion/completions/sshuttle2 \
        /usr/share/zsh/site-functions/_sshuttle2 \
        /usr/share/fish/vendor_completions.d/sshuttle2.fish; do
        [ -s "$f" ] || { echo "missing: $f" >&2; exit 1; }
    done
    case $1 in
        *usr/share/man/man1/sshuttle2.1*) ;;
        *) echo "the package has no usr/share/man/man1/sshuttle2.1" >&2; exit 1 ;;
    esac
    echo "ok: man page and completions"
}

if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$dist/sshuttle2_${VERSION}_amd64.deb" curl >/dev/null
    check /usr/bin/sshuttle2
    check_docs "$(dpkg-deb -c "$dist/sshuttle2_${VERSION}_amd64.deb")"
    apt-get remove -y -qq sshuttle2 >/dev/null
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y -q "$dist/sshuttle2-${VERSION}-1.x86_64.rpm"
    check /usr/bin/sshuttle2
    check_docs "$(rpm -qlp "$dist/sshuttle2-${VERSION}-1.x86_64.rpm")"
    dnf remove -y -q sshuttle2
elif command -v pacman >/dev/null 2>&1; then
    case $VERSION in
        *-*) echo "no Arch package for a prerelease" ;;
        *)
            pacman -Sy --noconfirm >/dev/null
            pacman -U --noconfirm "$dist/sshuttle2-${VERSION}-1-x86_64.pkg.tar.zst" >/dev/null
            check /usr/bin/sshuttle2
            check_docs "$(pacman -Qlp "$dist/sshuttle2-${VERSION}-1-x86_64.pkg.tar.zst")"
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
