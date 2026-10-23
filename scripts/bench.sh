#!/usr/bin/env bash
# Throughput and latency through a real server, SOCKS5 mode (no root).
#
#   scripts/bench.sh [user@]host
#
# Compares OpenSSH's own SOCKS proxy (`ssh -D`, all C: the ceiling for one
# ssh connection) with sshuttle2 over the same server.
#
# The main test file is served by the server itself on 127.0.0.1, so the
# numbers measure the Mac <-> server path and the tunnel, not a third party's
# rate limits. One internet download is added for realism. The server needs
# python3 (sshuttle needs it too); everything is removed afterwards.
# BENCH_MB sets the test file size (default 200).
set -euo pipefail

host=${1:?usage: $0 [user@]host}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=${SSHUTTLE2_BIN:-$root/target/release/sshuttle2}
port=18765
mb=${BENCH_MB:-200}
local_file="http://127.0.0.1:$port/bench.bin"
internet_file=${BENCH_URL:-https://proof.ovh.net/files/100Mb.dat}
small=${BENCH_SMALL:-https://www.google.com/generate_204}
pids=()

cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  ssh "$host" "pkill -f 'http.server $port' ; rm -rf /tmp/tsbench" >/dev/null 2>&1 || true
}
trap cleanup EXIT

wait_port() {
  for _ in $(seq 1 150); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; then return 0; fi
    sleep 0.1
  done
  echo "nothing listening on port $1" >&2
  return 1
}

now() { python3 -c 'import time; print(time.time())'; }

# fetch PORT URL -> "<bytes> <seconds>"; fails on HTTP errors
fetch() {
  curl -sS --fail --max-time 300 --socks5-hostname "127.0.0.1:$1" -o /dev/null \
    -w '%{size_download} %{time_total}\n' "$2"
}

# download PORT URL -> Mbit/s
download() {
  local r
  r=$(fetch "$1" "$2") || { echo "failed"; return; }
  echo "$r" | awk '{printf "%.1f Mbit/s", $1 * 8 / $2 / 1e6}'
}

# parallel PORT N URL -> aggregate Mbit/s of N simultaneous downloads
parallel() {
  local start end c total out curls=() bad=0
  out=$(mktemp -d)
  start=$(now)
  for i in $(seq 1 "$2"); do
    fetch "$1" "$3" >"$out/$i" &
    curls+=($!)
  done
  for c in "${curls[@]}"; do wait "$c" || bad=1; done
  end=$(now)
  total=$(cat "$out"/* | awk '{s += $1} END {print s + 0}')
  rm -rf "$out"
  [ "$bad" = 0 ] || { echo "failed"; return; }
  python3 -c "print(f'{$total * 8 / ($end - $start) / 1e6:.1f} Mbit/s')"
}

# ttfb PORT -> milliseconds to the first byte of a tiny request
ttfb() {
  curl -sS --max-time 30 --socks5-hostname "127.0.0.1:$1" -o /dev/null -w '%{time_starttransfer}' "$small" |
    awk '{printf "%.0f ms", $1 * 1000}'
}

# CPU seconds used so far by the sshuttle2 server process. The regex
# cannot match this command's own text, unlike a plain string.
server_cpu() {
  ssh "$host" 'p=$(pgrep -n -f "sshuttle2-server-[0-9a-f]+ server") && \
    awk -v t=$(getconf CLK_TCK) "{print (\$14 + \$15) / t}" /proc/$p/stat'
}

# suite PORT: the same measurements for whichever proxy listens on PORT
suite() {
  echo "   idle first-byte:       $(ttfb "$1")"
  echo "   1 download (server):   $(download "$1" "$local_file")"
  echo "   4 at once (server):    $(parallel "$1" 4 "$local_file")"
  echo "   1 download (internet): $(download "$1" "$internet_file")"
  curl -sS --max-time 300 --socks5-hostname "127.0.0.1:$1" -o /dev/null "$local_file" &
  local load=$!
  sleep 3
  echo "   first-byte under load: $(ttfb "$1")"
  wait "$load" || true
}

echo "== preparing a $mb MB test file on $host"
ssh "$host" "mkdir -p /tmp/tsbench && cd /tmp/tsbench && \
  head -c ${mb}000000 /dev/urandom > bench.bin && \
  (nohup python3 -m http.server $port --bind 127.0.0.1 >/dev/null 2>&1 &)"

echo "-- ssh -D (OpenSSH SOCKS, baseline)"
ssh -N -D 127.0.0.1:11180 -o ExitOnForwardFailure=yes "$host" &
sshpid=$!
pids+=($sshpid)
wait_port 11180
suite 11180
kill "$sshpid"

echo "-- sshuttle2 --socks5"
"$bin" -r "$host" --socks5 127.0.0.1:11181 &
pids+=($!)
wait_port 11181
sleep 1
cpu0=$(server_cpu)
t0=$(now)
suite 11181
cpu1=$(server_cpu)
t1=$(now)
python3 -c "print(f'   server CPU (average):  {100 * ($cpu1 - $cpu0) / ($t1 - $t0):.0f} % of one core')"
