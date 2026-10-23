#!/usr/bin/env bash
# Builds the AUR package with makepkg and installs it, inside an Arch Linux
# container, as an AUR user would. DIR holds the PKGBUILD, plus the source
# tarball if it is not published yet (makepkg then uses it instead of
# downloading). Leaves the PKGBUILD and the .SRCINFO the AUR needs in DIR/out.
#
#   docker run --rm -e VERSION -v "$DIR:/pkg" -v "$PWD:/src:ro" archlinux:latest \
#       bash /src/packaging/aur/check.sh /pkg

set -euo pipefail

dir=${1:?usage: check.sh DIR}
: "${VERSION:?set VERSION}"

pacman -Syu --noconfirm --needed base-devel namcap openssh >/dev/null

# makepkg refuses to run as root.
useradd -m builder
work=/home/builder/pkg
cp -r "$dir" "$work"
rm -rf "$work/out"
chown -R builder: "$work"
cd "$work"
as_builder() { runuser -u builder -- "$@"; }

as_builder namcap PKGBUILD
as_builder makepkg --printsrcinfo >.SRCINFO
as_builder makepkg --cleanbuild --noconfirm
pacman -U --noconfirm sshuttle2-bin-*.pkg.tar.zst >/dev/null
as_builder namcap sshuttle2-bin-*.pkg.tar.zst

out=$(sshuttle2 --version)
if [ "$out" != "sshuttle2 $VERSION" ]; then
    echo "check.sh: sshuttle2 --version printed: $out" >&2
    exit 1
fi

mkdir -p "$dir/out"
cp PKGBUILD .SRCINFO "$dir/out/"
cat .SRCINFO
echo "ok: sshuttle2-bin $VERSION built, installed and checked"
