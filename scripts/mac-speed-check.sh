#!/bin/bash
# Speed comparison on this Mac, unattended after one password prompt:
# direct once, then for each server sshuttle2 pf mode, sshuttle2 tun mode,
# and sshuttle. Results go to ~/sshuttle2-results.txt as well as the screen.
#
#   scripts/mac-speed-check.sh [user@]host...
#
# A running sshuttle is stopped first and restarted, with the same command,
# at the end. Set SIZE=10Mb for a quick trial run.
[ $# -ge 1 ] || { echo "usage: $0 [user@]host..." >&2; exit 2; }
root=$(cd "$(dirname "$0")/.." && pwd)
bin=$root/target/release/sshuttle2
out=$HOME/sshuttle2-results.txt
big=https://proof.ovh.net/files/${SIZE:-100Mb}.dat
small=https://www.google.com/generate_204
logs=$(mktemp -d)
pid=

exec > >(tee "$out") 2>&1

stop() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid"
    for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
    kill -0 "$pid" 2>/dev/null && kill -KILL "$pid"
  fi
  pid=
}
trap 'stop; echo "aborted"; exit 1' INT TERM

clear_sshuttle_rules() {
  for a in sshuttle-12300 sshuttle-12299 sshuttle6-12300 sshuttle6-12299; do
    sudo pfctl -a "$a" -F all >/dev/null 2>&1
  done
}

mbit() { awk '{printf "%.1f Mbit/s", $1 * 8 / 1e6}'; }
ms() { awk '{printf "%.0f ms", $1 * 1000}'; }

# A firewall that opens HTTPS to look inside fails the certificate check
# (curl exit 60): that is how the hospital Wi-Fi blocks Wikipedia.
wikipedia() {
  local rc
  curl -s --max-time 15 -o /dev/null https://en.wikipedia.org/
  rc=$?
  case $rc in
    0) echo "reachable" ;;
    60) echo "blocked (certificate error)" ;;
    *) echo "failed (curl exit $rc)" ;;
  esac
}

server_spec() {
  ssh -o BatchMode=yes -o ConnectTimeout=15 "$1" 'nproc; grep MemTotal /proc/meminfo' 2>/dev/null |
    awk 'NR == 1 {c = $1} /MemTotal/ {m = $2}
      END {if (c) printf "%s vCPU, %.1f GB RAM\n", c, m / 1048576; else print "unknown (ssh failed)"}'
}

measure() {
  echo "   exit IP:               $(curl -s --max-time 15 https://ifconfig.me || echo failed)"
  echo "   en.wikipedia.org:      $(wikipedia)"
  echo "   (downloading ${SIZE:-100Mb}, up to 60 s...)"
  echo "   download:              $(curl -s --max-time 60 -o /dev/null -w '%{speed_download}' "$big" | mbit)"
  echo "   page, idle:            $(curl -s --max-time 15 -o /dev/null -w '%{time_starttransfer}' "$small" | ms)"
  curl -s --max-time 60 -o /dev/null "$big" &
  local load=$!
  sleep 3
  echo "   page, during download: $(curl -s --max-time 15 -o /dev/null -w '%{time_starttransfer}' "$small" | ms)"
  wait "$load"
}

stun() {
  python3 - <<'EOF'
import socket, struct, os
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(5)
try:
    s.sendto(struct.pack('!HHI', 1, 0, 0x2112A442) + os.urandom(12), ('stun.l.google.com', 19302))
    d, _ = s.recvfrom(2048)
    i = 20
    while i < len(d):
        t, l = struct.unpack('!HH', d[i:i + 4]); v = d[i + 4:i + 4 + l]
        if t == 0x20:
            ip = socket.inet_ntoa(struct.pack('!I', struct.unpack('!I', v[4:8])[0] ^ 0x2112A442))
            print('   UDP exit IP (STUN):    ' + ip)
        i += 4 + l + (-l % 4)
except Exception as e:
    print('   UDP exit IP (STUN):    failed (%s)' % e)
EOF
}

# start NAME READY-TEXT COMMAND... : runs a tunnel in the background and
# waits until its log shows READY-TEXT.
start() {
  local name=$1 ready=$2
  shift 2
  "$@" >"$logs/$name.log" 2>&1 &
  pid=$!
  for _ in $(seq 1 120); do
    grep -q "$ready" "$logs/$name.log" 2>/dev/null && return 0
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
  done
  echo "   did not connect; log:"
  sed 's/^/     /' "$logs/$name.log" | tail -20
  stop
  return 1
}

echo "sshuttle2 speed check, $(date), servers: $*"
sudo -v -p '[local sudo] Password (asked once): ' || exit 1
# The running sshuttle, to stop now and restart at the end.
sshuttle_pids=$(pgrep -f 'bin/sshuttle .*-r ')
sshuttle_pid=$(echo "$sshuttle_pids" | head -1)
sshuttle_cmd=
if [ -n "$sshuttle_pid" ]; then
  sshuttle_cmd=$(ps -o command= -p "$sshuttle_pid" |
    awk '{for (i = 1; i <= NF; i++) if ($i ~ /sshuttle$/) {for (j = i; j <= NF; j++) printf "%s ", $j; exit}}')
  echo "sshuttle is running; it is stopped for the test and restarted afterwards:"
  echo "   $sshuttle_cmd"
fi
echo "starting in 30 s..."
sleep 30
# SIGTERM, not SIGINT: a copy started by a script ignores SIGINT.
for p in $sshuttle_pids; do kill -TERM "$p"; done
for p in $sshuttle_pids; do
  for _ in $(seq 1 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.2; done
done
# Put the user's sshuttle back, also when the run is aborted.
restart_sshuttle() {
  [ -n "$sshuttle_cmd" ] || return 0
  echo
  echo "restarting sshuttle: $sshuttle_cmd"
  clear_sshuttle_rules
  nohup $sshuttle_cmd >"$HOME/sshuttle.log" 2>&1 &
  for _ in $(seq 1 60); do grep -q Connected "$HOME/sshuttle.log" 2>/dev/null && break; sleep 0.5; done
  if grep -q Connected "$HOME/sshuttle.log"; then
    echo "   sshuttle is connected again (log: ~/sshuttle.log; stop it with: pkill -f 'bin/sshuttle ')"
  else
    echo "   sshuttle did not report Connected; see ~/sshuttle.log"
  fi
  sshuttle_cmd=
}

# Keep sudo's ticket fresh for the whole run.
( while sleep 50; do sudo -n -v 2>/dev/null; done ) &
keepalive=$!
trap 'stop; echo "aborted"; restart_sshuttle; kill $keepalive 2>/dev/null; exit 1' INT TERM
clear_sshuttle_rules

echo
echo "== direct (no tunnel)"
measure

for host in "$@"; do
  echo
  echo "######## server $host: $(server_spec "$host")"

  echo
  echo "== $host: sshuttle2, pf mode (TCP + DNS)"
  if start pf "connected to" "$bin" -r "$host" 0/0 --dns -v; then
    sleep 2
    measure
    stop
  fi

  echo
  echo "== $host: sshuttle2, tun mode (TCP + UDP + DNS)"
  if start tun "connected to" "$bin" -r "$host" 0/0 --udp -v; then
    sleep 2
    grep "pf anchor" "$logs/tun.log" | sed 's/^/   /'
    measure
    stun
    stop
  fi

  echo
  echo "== $host: sshuttle --no-latency-control (for comparison)"
  if command -v sshuttle >/dev/null; then
    if start sshuttle "Connected" sshuttle -r "$host" 0/0 --dns --no-latency-control; then
      sleep 2
      measure
      stop
    fi
    clear_sshuttle_rules
  else
    echo "   sshuttle not installed; skipped"
  fi
done

restart_sshuttle
kill "$keepalive" 2>/dev/null
echo
echo "done. Results saved in $out"
