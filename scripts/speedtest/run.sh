#!/usr/bin/env bash
# The speed test itself, on the client VM (runner.sh starts it): for each
# server, ROUNDS rounds of every mode, in a rotated order each round so a
# slow minute does not always hit the same mode. Each measurement is one
# JSON line in results.jsonl; run.done says "ok" or why it stopped.
#
#   SERVERS="near-1c far-1c" ROUNDS=3 RUNNER_IP=a.b.c.d ./run.sh
#
# SERVERS are ssh aliases (~/.ssh/config); each runs sample.py from its home
# directory. RUNNER_IP stays out of every tunnel, so the runner's ssh session
# here survives tun mode. Nothing this writes holds an address: the workflow
# log is public.
#
# Modes (MODES picks some):
#   direct          Ookla from here, no tunnel
#   server          Ookla on the server itself: the ceiling for its side
#   sshuttle        sshuttle -r SERVER 0/0 --dns
#   sshuttle-nlc    the same with --no-latency-control
#   sshuttle2-nft   sshuttle2 -r SERVER 0/0 --dns --method nft (TCP + DNS)
#   sshuttle2-tun   sshuttle2 -r SERVER 0/0 --udp (TCP + UDP + DNS)
set -uo pipefail

cd "$(dirname "$0")" || exit 1
: "${SERVERS:?}" "${ROUNDS:?}" "${RUNNER_IP:?}"
MODES=${MODES:-direct server sshuttle sshuttle-nlc sshuttle2-nft sshuttle2-tun}
out=$PWD/results.jsonl
tmp=$(mktemp -d)
tpid=
# The tunnel server process, for sample.py on the server. [.] and [0-9a-f]
# keep the pattern from matching a command line that contains it.
server_match='assembler[.]py|sshuttle2-server-[0-9a-f]+ server'

: >"$out"
rm -f run.done

# ookla [ssh ALIAS]: one Ookla run against $ookla_id, here or on ALIAS.
# Prints the fields we keep, as JSON, or {"error": ...}. Ookla refuses too
# many tests in a row ("Limit reached"); that gets one retry after a pause.
ookla() {
  local raw err try
  for try in 1 2; do
    raw=$(timeout 180 "$@" speedtest --accept-license --accept-gdpr -p no -f json -s "$ookla_id" 2>"$tmp/ookla.err")
    if jq -e '.download.bandwidth' <<<"$raw" >/dev/null 2>&1; then
      jq -c '{download_mbps: (.download.bandwidth * 8 / 1e6), upload_mbps: (.upload.bandwidth * 8 / 1e6),
        ping_ms: .ping.latency, jitter_ms: .ping.jitter,
        download_ping_ms: .download.latency.iqm, upload_ping_ms: .upload.latency.iqm,
        packet_loss: .packetLoss}' <<<"$raw"
      return
    fi
    err=$( (cat "$tmp/ookla.err"; jq -r '.error? // .message? // empty' <<<"$raw" 2>/dev/null) | tr '\n' ' ' | tail -c 300)
    [[ $try = 1 && $err == *"imit"* ]] || break
    echo "   Ookla: $err; trying again in 90 s" >&2
    sleep 90
  done
  jq -cn --arg e "Ookla: ${err:-no result}" '{error: $e}'
}

# Firewall rules a tunnel that did not exit cleanly can leave behind.
clear_rules() {
  local ipt c rule fam name
  for ipt in iptables ip6tables; do
    for c in $($ipt -t nat -S 2>/dev/null | awk '$1 == "-N" && $2 ~ /^sshuttle/ {print $2}'); do
      echo "   removing a leftover $ipt chain $c"
      $ipt -t nat -S | awk -v c="$c" '$1 == "-A" && $NF == c' | sed 's/^-A/-D/' |
        while read -r rule; do
          # shellcheck disable=SC2086 # a rule is several words
          $ipt -t nat $rule
        done
      $ipt -t nat -F "$c"
      $ipt -t nat -X "$c"
    done
  done
  nft list tables 2>/dev/null | awk '$3 ~ /sshuttle/ {print $2, $3}' | while read -r fam name; do
    echo "   removing a leftover nft table $fam $name"
    nft delete table "$fam" "$name"
  done
}

# start_tunnel MODE ALIAS: starts the tunnel in the background ($tpid) and
# waits until it says it is connected.
start_tunnel() {
  local ip ready cmd keep
  ip=$(ssh -G "$2" | awk '$1 == "hostname" {print $2}')
  keep=(-x "$ip/32" -x "$RUNNER_IP/32")
  case $1 in
    sshuttle) cmd=(sshuttle -r "$2" 0/0 --dns "${keep[@]}") ready="Connected to server" ;;
    sshuttle-nlc) cmd=(sshuttle -r "$2" 0/0 --dns --no-latency-control "${keep[@]}") ready="Connected to server" ;;
    sshuttle2-nft) cmd=(./sshuttle2 -r "$2" 0/0 --dns --method nft -v "${keep[@]}") ready="connected to" ;;
    sshuttle2-tun) cmd=(./sshuttle2 -r "$2" 0/0 --udp -v "${keep[@]}") ready="connected to" ;;
    *) echo "   unknown mode $1"; return 1 ;;
  esac
  "${cmd[@]}" >"$tmp/tunnel.log" 2>&1 &
  tpid=$!
  for _ in $(seq 1 120); do
    if grep -q "$ready" "$tmp/tunnel.log"; then
      sleep 2
      return 0
    fi
    kill -0 "$tpid" 2>/dev/null || break
    sleep 0.5
  done
  echo "   $1 did not connect; its log:"
  tail -20 "$tmp/tunnel.log" | sed 's/^/     /'
  stop_tunnel
  return 1
}

stop_tunnel() {
  [ -n "$tpid" ] || return 0
  kill -TERM "$tpid" 2>/dev/null
  for _ in $(seq 1 30); do
    kill -0 "$tpid" 2>/dev/null || break
    sleep 0.5
  done
  kill -KILL "$tpid" 2>/dev/null
  wait "$tpid" 2>/dev/null
  tpid=
  sleep 1
  clear_rules
}

# server_quiet ALIAS: waits until the tunnel's server side has exited, so the
# next sample counts only the next tunnel.
server_quiet() {
  ssh -n "$1" "for i in \$(seq 1 30); do pgrep -f '$server_match' >/dev/null || exit 0; sleep 0.5; done
    pkill -KILL -f '$server_match'; exit 0"
}

# start_samplers ALIAS / stop_samplers ALIAS: sample.py on both ends of the
# running tunnel, for the length of one Ookla run.
start_samplers() {
  stop=/tmp/sample-stop.$$
  rm -f "$stop"
  ssh -n "$1" "rm -f $stop; exec python3 sample.py --match '$server_match' --stop $stop" \
    >"$tmp/server.json" 2>"$tmp/server.err" &
  spid=$!
  python3 sample.py --pid "$tpid" --stop "$stop" >"$tmp/client.json" 2>"$tmp/client.err" &
  cpid=$!
  # Both take their first (idle) sample before the load starts.
  sleep 2
}

stop_samplers() {
  touch "$stop"
  ssh -n "$1" "touch $stop"
  wait "$spid" "$cpid"
  ssh -n "$1" "rm -f $stop"
  rm -f "$stop"
}

json_or_null() { jq -ce . "$1" 2>/dev/null || echo null; }

# measure SERVER MODE ROUND: one line in results.jsonl, and one on screen.
measure() {
  local srv=$1 mode=$2 round=$3 o sp=null cp=null line
  case $mode in
    direct) o=$(ookla) ;;
    server) o=$(ookla ssh -n "$srv") ;;
    *)
      if start_tunnel "$mode" "$srv"; then
        start_samplers "$srv"
        o=$(ookla)
        stop_samplers "$srv"
        sp=$(json_or_null "$tmp/server.json")
        cp=$(json_or_null "$tmp/client.json")
        stop_tunnel
      else
        o='{"error": "the tunnel did not connect"}'
      fi
      server_quiet "$srv"
      ;;
  esac
  line=$(jq -cn --arg server "$srv" --arg mode "$mode" --argjson round "$round" \
    --argjson ookla "$o" --argjson sp "$sp" --argjson cp "$cp" \
    '{type: "result", server: $server, mode: $mode, round: $round, ookla: $ookla, server_proc: $sp, client_proc: $cp}')
  echo "$line" >>"$out"
  jq -r --arg tag "[$srv round $round] $mode" '
    def n: if . == null then "-" else (. * 10 | round / 10 | tostring) end;
    def procs(p; name): if p == null then "" elif p.error then " | \(name): \(p.error)"
      else " | \(name) CPU \(p.cpu_avg | n) % (peak \(p.cpu_peak | n)), RAM \(p.rss_peak_mb | n) MB" end;
    if .ookla.error then "\($tag): \(.ookla.error)"
    else "\($tag): down \(.ookla.download_mbps | n), up \(.ookla.upload_mbps | n) Mbit/s, ping \(.ookla.ping_ms | n) ms (loaded \(.ookla.download_ping_ms | n) / \(.ookla.upload_ping_ms | n))"
      + procs(.server_proc; "server") + procs(.client_proc; "client") end' <<<"$line"
  sleep 3
}

trap 'stop_tunnel; echo "failed: interrupted" >run.done; exit 1' INT TERM

jq -cn --arg started "$(date -u +%FT%TZ)" --argjson rounds "$ROUNDS" --arg modes "$MODES" \
  --arg sshuttle2 "$(./sshuttle2 --version 2>&1 | head -1)" \
  --arg sshuttle "$(sshuttle --version 2>&1 | head -1)" \
  --arg ookla "$(speedtest --accept-license --accept-gdpr --version 2>&1 | head -1)" \
  --argjson cpus "$(nproc)" --argjson mem "$(awk '/MemTotal/ {print int($2 / 1024)}' /proc/meminfo)" \
  --arg os "$(. /etc/os-release && echo "$PRETTY_NAME"), kernel $(uname -r)" \
  '{type: "meta", started: $started, rounds: $rounds, modes: ($modes | split(" ")),
    versions: {sshuttle2: $sshuttle2, sshuttle: $sshuttle, ookla: $ookla},
    client: {cpus: $cpus, mem_mb: $mem, os: $os}}' >>"$out"
echo "speed test: servers $SERVERS, $ROUNDS rounds, modes: $MODES"
read -r -a modes <<<"$MODES"

for srv in $SERVERS; do
  echo
  echo "== $srv"
  if ! spec=$(ssh -n "$srv" 'echo "$(nproc) $(awk "/MemTotal/ {print int(\$2 / 1024)}" /proc/meminfo)"'); then
    echo "   cannot reach $srv; skipped"
    jq -cn --arg s "$srv" '{type: "server", server: $s, error: "unreachable"}' >>"$out"
    continue
  fi
  # The nearest Ookla server to the server VM, the same for every mode:
  # through a tunnel the traffic leaves from there.
  list=$(ssh -n "$srv" 'speedtest --accept-license --accept-gdpr -L -f json' 2>/dev/null)
  ookla_id=$(jq -r '.servers[0].id // empty' <<<"$list" 2>/dev/null)
  if [ -z "$ookla_id" ]; then
    echo "   no Ookla server found from $srv; skipped"
    jq -cn --arg s "$srv" '{type: "server", server: $s, error: "no Ookla server found"}' >>"$out"
    continue
  fi
  read -r cpus mem <<<"$spec"
  jq -c --arg s "$srv" --argjson cpus "$cpus" --argjson mem "$mem" \
    '{type: "server", server: $s, cpus: $cpus, mem_mb: $mem,
      ookla_server: (.servers[0] | {id, name, location, country})}' <<<"$list" | tee -a "$out" |
    jq -r '"   \(.cpus) vCPU, \(.mem_mb) MB; Ookla server \(.ookla_server.id): \(.ookla_server.name), \(.ookla_server.location), \(.ookla_server.country)"'
  for round in $(seq 1 "$ROUNDS"); do
    for i in "${!modes[@]}"; do
      measure "$srv" "${modes[$(((i + round - 1) % ${#modes[@]}))]}" "$round"
    done
  done
done

errors=$(jq -s '[.[] | select(.type == "result" and .ookla.error != null)] | length' "$out")
echo
echo "done: $(grep -c '"type":"result"' "$out") measurements, $errors failed"
echo ok >run.done
