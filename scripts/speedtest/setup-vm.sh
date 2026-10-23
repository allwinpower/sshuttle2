#!/usr/bin/env bash
# Prepares a fresh Ubuntu VM for the speed test; runs as root on the VM.
#
#   SSHUTTLE_VERSION=... OOKLA_VERSION=... [OOKLA_SHA256=...] setup-vm.sh client|server
#
# Both: Ookla's speedtest CLI (the server runs it for the "ceiling" mode and
# to find the nearest Ookla server), python3 and jq. The client also gets
# nftables (sshuttle2's nft mode), iptables (sshuttle's nat method) and
# sshuttle from PyPI at a pinned version. Background jobs that would show up
# in the CPU numbers (apt timers, unattended upgrades) are stopped.
set -euo pipefail

role=${1:?usage: $0 client|server}
: "${OOKLA_VERSION:?}" "${SSHUTTLE_VERSION:?}"
export DEBIAN_FRONTEND=noninteractive

cloud-init status --wait >/dev/null 2>&1 || true
systemctl stop unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer \
  apt-daily.service apt-daily-upgrade.service >/dev/null 2>&1 || true
pkgs=(python3 jq curl)
[ "$role" = client ] && pkgs+=(nftables iptables python3-venv)
# The lock timeout waits for an apt run that started before the stop.
apt-get -o DPkg::Lock::Timeout=300 update -q >/dev/null
apt-get -o DPkg::Lock::Timeout=300 install -y -q --no-install-recommends "${pkgs[@]}" >/dev/null

case $(uname -m) in
  x86_64) arch=x86_64 ;;
  aarch64) arch=aarch64 ;;
  *) echo "unsupported CPU $(uname -m)" >&2; exit 1 ;;
esac
tgz=/tmp/ookla.tgz
curl -fsSL --retry 3 -o "$tgz" "https://install.speedtest.net/app/cli/ookla-speedtest-$OOKLA_VERSION-linux-$arch.tgz"
sum=$(sha256sum "$tgz" | cut -c1-64)
echo "ookla-speedtest-$OOKLA_VERSION-linux-$arch.tgz sha256 $sum"
if [ -n "${OOKLA_SHA256:-}" ] && [ "$sum" != "$OOKLA_SHA256" ]; then
  echo "Ookla CLI checksum mismatch: expected $OOKLA_SHA256" >&2
  exit 1
fi
tar -xzf "$tgz" -C /usr/local/bin speedtest
speedtest --accept-license --accept-gdpr --version | head -1

if [ "$role" = client ]; then
  python3 -m venv /opt/sshuttle
  /opt/sshuttle/bin/pip install -q --disable-pip-version-check "sshuttle==$SSHUTTLE_VERSION"
  ln -sf /opt/sshuttle/bin/sshuttle /usr/local/bin/sshuttle
  echo "sshuttle $(sshuttle --version)"
fi
echo "$role ready: $(nproc) vCPU, $(awk '/MemTotal/ {printf "%.1f GB", $2 / 1048576}' /proc/meminfo), $(awk -F= '$1 == "PRETTY_NAME" {gsub(/"/, "", $2); print $2}' /etc/os-release), kernel $(uname -r)"
