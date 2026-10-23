#!/bin/sh
# Live macOS tun check, driven from a terminal so sudo can ask once.
# Lists pf anchors that capture TCP, clears sshuttle's leftover ones (a
# running sshuttle re-adds them), then starts tun mode with a packet trace.
#   scripts/mac-tun-check.sh [user@]host
host=${1:?usage: $0 [user@]host}
bin=$(cd "$(dirname "$0")/.." && pwd)/target/release/sshuttle2
sudo -v -p '[local sudo, for the tun test] Password: ' || exit 1
echo "== pf anchors that send TCP to lo0:"
for a in $(sudo pfctl -s Anchors 2>/dev/null); do
  r=$(sudo pfctl -a "$a" -s rules 2>/dev/null)
  case "$r" in *route-to*) echo "-- $a"; echo "$r" ;; esac
done
if pgrep -f 'bin/sshuttle .*-r ' >/dev/null; then
  echo "sshuttle is running; stop it first" >&2
  exit 1
fi
for a in sshuttle-12300 sshuttle-12299 sshuttle6-12300 sshuttle6-12299; do
  sudo pfctl -a "$a" -F all >/dev/null 2>&1
done
echo "== cleared sshuttle anchors"
SSHUTTLE2_TUN_TRACE=${TRACE:-60} exec "$bin" -r "$host" 0/0 --udp -v
