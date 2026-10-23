#!/bin/sh
# Installs the sshuttle2 client from a GitHub release, after checking it
# against the release's SHA256SUMS.
#
#   curl -fsSL https://github.com/allwinpower/sshuttle2/releases/latest/download/install.sh | sh
#
# Options (after `sh -s --` when piped):
#   --version X.Y.Z   that release instead of the latest
#   --user            install into ~/.local/bin instead of /usr/local/bin
#   --bin-dir DIR     install into DIR
# SSHUTTLE2_RELEASE_URL replaces the download location (used by the tests).

set -eu

repo=allwinpower/sshuttle2
version=
bin_dir=/usr/local/bin

die() {
    echo "install.sh: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case $1 in
        --version) [ $# -ge 2 ] || die "--version needs a value"; version=${2#v}; shift 2 ;;
        --user) bin_dir=$HOME/.local/bin; shift ;;
        --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a value"; bin_dir=$2; shift 2 ;;
        -h | --help) echo "usage: install.sh [--version X.Y.Z] [--user | --bin-dir DIR]"; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

case $(uname -s) in
    Darwin) os=macos ;;
    Linux) os=linux ;;
    *) die "sshuttle2 runs on macOS and Linux, not $(uname -s)" ;;
esac
case $(uname -m) in
    x86_64 | amd64) arch=amd64 ;;
    arm64 | aarch64) arch=arm64 ;;
    *) die "no sshuttle2 build for $(uname -m)" ;;
esac

if [ -n "$version" ]; then
    base=https://github.com/$repo/releases/download/v$version
else
    base=https://github.com/$repo/releases/latest/download
fi
base=${SSHUTTLE2_RELEASE_URL:-$base}

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
else
    die "needs curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$@"; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$@"; }
else
    die "needs sha256sum or shasum"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download $base/SHA256SUMS"
# The checksum list names the release's files, so it also tells which
# version "latest" is.
asset=$(sed -n "s/^[0-9a-f]\{64\}  \(sshuttle2-[^ ]*-$os-$arch\.tar\.gz\)\$/\1/p" "$tmp/SHA256SUMS")
[ -n "$asset" ] || die "this release has no build for $os $arch"
echo "Downloading $asset"
fetch "$base/$asset" "$tmp/$asset" || die "could not download $base/$asset"
grep "  $asset\$" "$tmp/SHA256SUMS" >"$tmp/sum"
(cd "$tmp" && sha256 -c sum >/dev/null) ||
    die "$asset does not match its SHA-256 in SHA256SUMS"
tar -xzf "$tmp/$asset" -C "$tmp"

src=$tmp/${asset%.tar.gz}/sshuttle2
if mkdir -p "$bin_dir" 2>/dev/null && [ -w "$bin_dir" ]; then
    install -m 755 "$src" "$bin_dir/sshuttle2"
else
    echo "Installing into $bin_dir needs sudo"
    sudo mkdir -p "$bin_dir"
    sudo install -m 755 "$src" "$bin_dir/sshuttle2"
fi

echo "Installed $("$bin_dir/sshuttle2" --version) to $bin_dir/sshuttle2"
case :$PATH: in
    *:"$bin_dir":*) ;;
    *) echo "$bin_dir is not in your PATH; add it, or run $bin_dir/sshuttle2" ;;
esac
