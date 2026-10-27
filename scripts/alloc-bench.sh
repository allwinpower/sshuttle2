#!/usr/bin/env bash
# CPU cost of Linux client builds, on one Linux host: the numbers behind the
# "Linux client allocator" table in docs/benchmarks.md.
#
#   scripts/alloc-bench.sh SERVER_BIN CLIENT_BIN...
#
# `-r local` runs SERVER_BIN as a local process instead of over ssh, so the
# tunnel is limited by the CPU, not by a network, and the clients can be
# compared. Name each client after what it is (sshuttle2-musl, ...). Every
# client gets RUNS rounds (default 3) of one download of an MB-sized test file
# (default 300) served on 127.0.0.1, then four at once. Prints each round and
# the medians. Needs python3 and curl.
set -euo pipefail

server=${1:?usage: $0 SERVER_BIN CLIENT_BIN...}
shift
[ $# -gt 0 ] || { echo "usage: $0 SERVER_BIN CLIENT_BIN..." >&2; exit 1; }
port=18765
socks=11181
mb=${MB:-300}
runs=${RUNS:-3}
tck=$(getconf CLK_TCK)
dir=$(mktemp -d)
pids=()

cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  rm -rf "$dir"
}
trap cleanup EXIT

wait_port() {
  for _ in $(seq 1 100); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; then return 0; fi
    sleep 0.1
  done
  echo "nothing listening on port $1" >&2
  return 1
}

url=http://127.0.0.1:$port/bench.bin

# one [SOCKS_PORT] -> Mbit/s of one download
one() {
  curl -sS --fail ${1:+--socks5-hostname "127.0.0.1:$1"} -o /dev/null \
    -w '%{size_download} %{time_total}\n' "$url" |
    awk '{printf "%.0f", $1 * 8 / $2 / 1e6}'
}

# four SOCKS_PORT -> aggregate Mbit/s of four downloads at once
four() {
  local s e
  s=$(date +%s.%N)
  for _ in 1 2 3 4; do curl -sS --fail --socks5-hostname "127.0.0.1:$1" -o /dev/null "$url" & done
  wait
  e=$(date +%s.%N)
  awk -v b=$((4 * mb * 1000000)) -v s="$s" -v e="$e" 'BEGIN {printf "%.0f", b * 8 / (e - s) / 1e6}'
}

# CPU seconds (user + system) a process has used so far
cpu() { awk -v t="$tck" '{print ($14 + $15) / t}' "/proc/$1/stat"; }

# median: the middle line of numbers on stdin
median() { sort -n | awk '{v[NR] = $1} END {print v[int((NR + 1) / 2)]}'; }

echo "== a $mb MB test file, served on 127.0.0.1:$port"
head -c "${mb}000000" /dev/urandom >"$dir/bench.bin"
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$dir" >/dev/null 2>&1 &
pids+=($!)
wait_port "$port"
echo "   no tunnel: $(one) Mbit/s"

for r in $(seq 1 "$runs"); do
  for client in "$@"; do
    name=$(basename "$client")
    "$client" -r local --server-bin "$server" --socks5 "127.0.0.1:$socks" >/dev/null 2>&1 &
    c=$!
    pids+=("$c")
    wait_port "$socks"
    sleep 0.5
    t0=$(cpu "$c")
    a=$(one "$socks")
    b=$(four "$socks")
    t1=$(cpu "$c")
    kill "$c"
    wait "$c" 2>/dev/null || true
    used=$(awk -v a="$t0" -v b="$t1" 'BEGIN {printf "%.1f", b - a}')
    echo "$name $a $b $used" >>"$dir/results"
    echo "-- $name, round $r: 1 download $a Mbit/s, 4 at once $b Mbit/s, client CPU $used s"
  done
done

echo "== medians of $runs rounds (client CPU is for $((5 * mb)) MB)"
for client in "$@"; do
  name=$(basename "$client")
  col() { awk -v n="$name" -v c="$1" '$1 == n {print $c}' "$dir/results" | median; }
  echo "   $name: 1 download $(col 2) Mbit/s, 4 at once $(col 3) Mbit/s, client CPU $(col 4) s"
done
