#!/bin/bash
# DNS in tun mode, examined on this Mac. Stops a running sshuttle, starts
# tun mode, queries DNS several ways, then restores sshuttle.
# Output goes to ~/sshuttle2-dns.txt as well as the screen.
#
#   scripts/mac-dns-check.sh [user@]host
host=${1:?usage: $0 [user@]host}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=$root/target/release/sshuttle2
out=$HOME/sshuttle2-dns.txt
log=$(mktemp)
pid=

exec > >(tee "$out") 2>&1

clear_sshuttle_rules() {
  for a in sshuttle-12300 sshuttle-12299 sshuttle6-12300 sshuttle6-12299; do
    sudo pfctl -a "$a" -F all >/dev/null 2>&1
  done
}

stop_tun() {
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid"
    for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
  fi
  pid=
}

restart_sshuttle() {
  [ -n "$sshuttle_cmd" ] || return 0
  echo
  echo "restarting sshuttle: $sshuttle_cmd"
  clear_sshuttle_rules
  nohup $sshuttle_cmd >"$HOME/sshuttle.log" 2>&1 &
  for _ in $(seq 1 60); do grep -q Connected "$HOME/sshuttle.log" 2>/dev/null && break; sleep 0.5; done
  grep -q Connected "$HOME/sshuttle.log" && echo "   sshuttle is connected again" ||
    echo "   sshuttle did not report Connected; see ~/sshuttle.log"
  sshuttle_cmd=
}

echo "sshuttle2 tun DNS check, $(date)"
sudo -v -p '[local sudo] Password (asked once): ' || exit 1
sshuttle_pids=$(pgrep -f 'bin/sshuttle .*-r ')
sshuttle_pid=$(echo "$sshuttle_pids" | head -1)
sshuttle_cmd=
if [ -n "$sshuttle_pid" ]; then
  sshuttle_cmd=$(ps -o command= -p "$sshuttle_pid" |
    awk '{for (i = 1; i <= NF; i++) if ($i ~ /sshuttle$/) {for (j = i; j <= NF; j++) printf "%s ", $j; exit}}')
fi
echo "starting in 30 s..."
sleep 30
# SIGTERM, not SIGINT: a copy started by a script ignores SIGINT.
for p in $sshuttle_pids; do kill -TERM "$p"; done
sleep 3
trap 'stop_tun; echo aborted; restart_sshuttle; exit 1' INT TERM
clear_sshuttle_rules

echo "== DNS configuration before"
scutil --dns | grep -E "resolver #|nameserver|if_index|flags|reach" | head -20

SSHUTTLE2_TUN_TRACE=600 "$bin" -r "$host" 0/0 --udp -vv >"$log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do grep -q "connected to" "$log" && break; sleep 0.5; done
sleep 2
echo
echo "== DNS configuration with tun up"
scutil --dns | grep -E "resolver #|nameserver|if_index|flags|reach" | head -20
echo
echo "== route to the DNS servers"
for ns in $(scutil --dns | awk '/nameserver\[/{print $3}' | sort -u); do
  echo "-- $ns: $(route -n get "$ns" 2>/dev/null | awk '/interface:/{print $2}')"
done
echo
echo "== dig straight at the system nameserver (goes through tun)"
ns=$(scutil --dns | awk '/nameserver\[0\]/{print $3; exit}')
dig +time=3 +tries=1 @"$ns" ifconfig.me A | grep -E "status:|^ifconfig|timed out|SERVER"
echo
echo "== dig at 1.1.1.1 (through tun)"
dig +time=3 +tries=1 @1.1.1.1 ifconfig.me A | grep -E "status:|^ifconfig|timed out|SERVER"
echo
echo "== system resolver (mDNSResponder), after flushing its cache"
sudo dscacheutil -flushcache; sudo killall -HUP mDNSResponder 2>/dev/null; sleep 1
since=$(date '+%Y-%m-%d %H:%M:%S')
for name in ifconfig.me stun.l.google.com example.com; do
  echo "-- $name: $(python3 "$root/scripts/resolve.py" "$name" 2>&1 | tail -1)"
done
echo "-- dscacheutil example.org: $(dscacheutil -q host -a name example.org | tr '\n' ' ')"
echo
echo "== mDNSResponder log while resolving"
log show --start "$since" --predicate 'process == "mDNSResponder"' --style compact 2>/dev/null |
  grep -viE "mdns|bonjour|_tcp|_udp" | tail -40
echo
echo "== curl ifconfig.me: $(curl -s --max-time 10 https://ifconfig.me || echo failed)"

lookups() {
  sudo dscacheutil -flushcache; sudo killall -HUP mDNSResponder 2>/dev/null; sleep 1
  for name in ifconfig.me stun.l.google.com example.com; do
    echo "   $name: $(python3 "$root/scripts/resolve.py" "$name" 2>&1 | tail -1)"
  done
  echo "   curl ifconfig.me: $(curl -s --max-time 10 https://ifconfig.me || echo failed)"
}
ifname=$(grep -o 'tun device utun[0-9]*' "$log" | awk '{print $3}' | head -1)
ps_id=$(scutil <<< "show State:/Network/Global/IPv4" | awk '/PrimaryService/{print $3}')
orig_ns=$(scutil --dns | awk '/resolver #1/{f=1} f && /nameserver\[/{print $3} /resolver #2/{exit}' | tr '\n' ' ')
echo
echo "== experiment 1: primary service ($ps_id) DNS -> 198.18.0.2"
printf 'get State:/Network/Service/%s/DNS\nd.add ServerAddresses * 198.18.0.2\nset State:/Network/Service/%s/DNS\nquit\n' "$ps_id" "$ps_id" | sudo scutil
scutil --dns | grep -A4 "resolver #1" | sed 's/^/   /'
lookups
printf 'get State:/Network/Service/%s/DNS\nd.add ServerAddresses * %s\nset State:/Network/Service/%s/DNS\nquit\n' "$ps_id" "$orig_ns" "$ps_id" | sudo scutil
echo
echo "== experiment 2: own service on $ifname with DNS 198.18.0.2, OverridePrimary"
printf 'd.init\nd.add Addresses * 198.18.0.1\nd.add DestAddresses * 198.18.0.2\nd.add InterfaceName %s\nd.add Router 198.18.0.2\nd.add OverridePrimary # 1\nset State:/Network/Service/sshuttle2-test/IPv4\nd.init\nd.add ServerAddresses * 198.18.0.2\nset State:/Network/Service/sshuttle2-test/DNS\nquit\n' "$ifname" | sudo scutil
sleep 2
scutil --dns | grep -A4 "resolver #1" | sed 's/^/   /'
lookups
printf 'remove State:/Network/Service/sshuttle2-test/IPv4\nremove State:/Network/Service/sshuttle2-test/DNS\nquit\n' | sudo scutil
sleep 1
echo
echo "== restored:"
scutil --dns | grep -A4 "resolver #1" | sed 's/^/   /'
route -n get default | grep interface | sed 's/^/   /'
echo
echo "== tun counters"
grep -E "tun: " "$log" | tail -2
stop_tun
restart_sshuttle
echo
echo "done. Output saved in $out"
