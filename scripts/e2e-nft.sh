#!/usr/bin/env bash
# End-to-end check of nft mode (Linux: TCP and DNS through nftables) in Docker.
#
#   scripts/e2e-nft.sh
#
# Containers:
#   client  runs sshuttle2 as root, with NET_ADMIN and nftables
#   sshd    the tunnel server; also on a private network the client is not on
#   target  on that private network only: a web server, and a name only the
#           sshd side's DNS knows
#   direct  on the client's network, excluded with -x
#   via     on the client's network, not excluded
# Each also runs whoami.py, which answers with the caller's address: that
# shows which way a connection went (from the client directly, or from the
# sshd through the tunnel).
#
# Needs a Linux build of the client that embeds the servers:
#   cargo xtask servers
#   SSHUTTLE2_SERVER_DIR=$PWD/dist/server cargo zigbuild --release -p sshuttle2 --target <arch>-unknown-linux-musl
# (CI builds natively instead; set SSHUTTLE2_LINUX_BIN to the binary.)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
arch=$(docker info --format '{{.Architecture}}' 2>/dev/null)
case "$arch" in aarch64|arm64) arch=aarch64 ;; *) arch=x86_64 ;; esac
bin=${SSHUTTLE2_LINUX_BIN:-$root/target/$arch-unknown-linux-musl/release/sshuttle2}
[ -x "$bin" ] || { echo "missing Linux client binary $bin" >&2; exit 2; }
# Override these to run several copies at once.
net=${E2E_NET:-249}
far=172.31.$net.0/24
target_ip=172.31.$net.10
p=${E2E_PREFIX:-ts-e2e-nft}
# An IPv6 network routed nowhere: connections to it only show that the
# client recovers IPv6 destinations.
v6net=fd00:7e57::/64
v6dst=fd00:7e57::5
work=$(mktemp -d)

cleanup() {
  docker rm -f $p-client $p-sshd $p-target $p-direct $p-via >/dev/null 2>&1 || true
  docker network rm $p-front $p-far >/dev/null 2>&1 || true
}
trap 'cleanup; rm -rf "$work"' EXIT
cleanup

ssh-keygen -q -t ed25519 -N "" -f "$work/key"
cp "$bin" "$work/sshuttle2"
cat >"$work/whoami.py" <<'EOF'
import http.server

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = self.client_address[0].encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

http.server.ThreadingHTTPServer(("0.0.0.0", 8001), H).serve_forever()
EOF
cat >"$work/Dockerfile" <<'EOF'
FROM alpine:3.20
RUN apk add --no-cache openssh python3 curl iproute2 bind-tools nftables && ssh-keygen -A && \
    adduser -D -s /bin/sh tester && passwd -u tester && mkdir -p /home/tester/.ssh
COPY key.pub /home/tester/.ssh/authorized_keys
COPY key /root/key
COPY whoami.py /whoami.py
COPY sshuttle2 /usr/local/bin/sshuttle2
RUN chown -R tester /home/tester/.ssh && chmod 600 /home/tester/.ssh/authorized_keys /root/key
EOF
docker build -q -t $p-nft "$work" >/dev/null

docker network create $p-front >/dev/null
docker network create --internal --subnet $far $p-far >/dev/null
docker run -d --name $p-target --network $p-far --ip $target_ip $p-nft sh -c '
  head -c 50000000 /dev/urandom > /tmp/big.bin
  python3 /whoami.py &
  cd /tmp && exec python3 -m http.server 8000' >/dev/null
docker run -d --name $p-sshd --network $p-front $p-nft /usr/sbin/sshd -D -e >/dev/null
docker network connect $p-far $p-sshd
for c in direct via; do
  docker run -d --name $p-$c --network $p-front $p-nft python3 /whoami.py >/dev/null
done
docker run -d --name $p-client --network $p-front --cap-add NET_ADMIN \
  --sysctl net.ipv6.conf.all.disable_ipv6=0 --sysctl net.ipv6.conf.lo.disable_ipv6=0 \
  $p-nft sh -c 'python3 /whoami.py & exec sleep infinity' >/dev/null

ip_of() {
  docker inspect -f "{{(index .NetworkSettings.Networks \"$p-front\").IPAddress}}" "$1"
}
client_ip=$(ip_of $p-client)
sshd_ip=$(ip_of $p-sshd)
direct_ip=$(ip_of $p-direct)
via_ip=$(ip_of $p-via)

fail() {
  echo "FAIL: $*" >&2
  docker exec $p-client sh -c 'cat /tmp/client*.log' >&2 || true
  exit 1
}
seen_by() {  # host -> the address it saw the request come from
  docker exec $p-client curl -sS --max-time 10 "http://$1:8001/"
}
wait_connected() {  # log
  for _ in $(seq 1 100); do
    docker exec $p-client grep -q "connected to" "$1" 2>/dev/null && return 0
    sleep 0.2
  done
  fail "client did not connect"
}
tables() {
  docker exec $p-client nft list tables
}

# Without the tunnel the target and its name are out of reach, and the
# others see the client's own address.
for _ in $(seq 1 50); do
  docker exec $p-target sh -c 'test -s /tmp/big.bin && wget -q -O /dev/null http://127.0.0.1:8000/' 2>/dev/null && break
  sleep 0.2
done
if docker exec $p-client curl -s --max-time 3 -o /dev/null http://$target_ip:8000/; then
  fail "target reachable without the tunnel; the test proves nothing"
fi
[ -z "$(docker exec $p-client dig +short +time=3 +tries=1 $p-target)" ] ||
  fail "$p-target resolves without the tunnel; the DNS check proves nothing"
[ "$(seen_by $via_ip)" = "$client_ip" ] || fail "via does not see the client's address"
# A route for the IPv6 test network, so connecting to it gets as far as nft.
docker exec $p-client ip -6 addr add fd00:7e57::1/64 dev lo

# nft is no longer the default; ask for it.
docker exec -d $p-client sh -c "NO_COLOR=1 sshuttle2 -r tester@$p-sshd --method nft 0/0 $v6net -x $direct_ip --dns -vv \
  -e 'ssh -i /root/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR' \
  >/tmp/client.log 2>&1"
wait_connected /tmp/client.log
docker exec $p-client grep -q "capture method: Nft" /tmp/client.log || fail "--method nft did not choose nft"
table=$(tables | awk '/sshuttle2-/ {print $3}')
[ -n "$table" ] || fail "no sshuttle2 table loaded"
echo "ok: auto chose nft; table $table loaded"
# The ssh connection's peer, found through /proc, is kept out.
docker exec $p-client grep -q "ssh connection peers peers=\[$sshd_ip\]" /tmp/client.log ||
  fail "ssh connection peer $sshd_ip not found"
echo "ok: ssh connection peer $sshd_ip found in /proc and excluded"

# TCP: a 50 MB download arrives intact.
want=$(docker exec $p-target sha256sum /tmp/big.bin | cut -c1-64)
got=$(docker exec $p-client sh -c "curl -sS --max-time 60 http://$target_ip:8000/big.bin | sha256sum" | cut -c1-64) ||
  fail "download through the tunnel failed"
[ "$got" = "$want" ] || fail "downloaded bytes differ"
speed=$(docker exec $p-client curl -sS -o /dev/null -w '%{speed_download}' http://$target_ip:8000/big.bin)
echo "ok: TCP 50 MB byte-exact ($(awk -v s="$speed" 'BEGIN{printf "%.0f", s*8/1e6}') Mbit/s)"

# Routing: 0/0 takes the client's own network too; -x and local addresses
# stay out.
seen=$(seen_by $via_ip) || fail "via unreachable"
[ "$seen" = "$sshd_ip" ] || fail "via saw $seen, expected the sshd ($sshd_ip): not tunneled"
echo "ok: $via_ip went through the tunnel"
seen=$(seen_by $direct_ip) || fail "direct unreachable"
[ "$seen" = "$client_ip" ] || fail "direct saw $seen, expected the client ($client_ip): -x ignored"
echo "ok: -x $direct_ip went direct"
seen=$(seen_by $client_ip) || fail "own address unreachable"
[ "$seen" = "$client_ip" ] || fail "own address saw $seen: local traffic was redirected"
echo "ok: the client's own address stayed local"

# DNS: the system resolver (Docker's, at 127.0.0.11) is redirected, and the
# sshd side's resolver knows the target's name.
ans=$(docker exec $p-client dig +short +time=5 +tries=1 $p-target) || fail "DNS query failed"
echo "$ans" | grep -qx "$target_ip" || fail "DNS answered '$ans', expected $target_ip"
echo "ok: DNS via --dns resolved $p-target to $target_ip"

# IPv6: the client recovers the original destination (the server cannot
# reach it, so the connection then fails).
docker exec $p-client curl -s --max-time 5 -o /dev/null "http://[$v6dst]:8000/" || true
docker exec $p-client grep -q -- "-> \[$v6dst\]:8000" /tmp/client.log ||
  fail "IPv6 original destination not recovered"
echo "ok: IPv6 original destination [$v6dst]:8000 recovered"

# Shutdown removes the table.
docker exec $p-client pkill -INT -f 'sshuttle2 -r'
for _ in $(seq 1 50); do
  tables | grep -q sshuttle2 || break
  sleep 0.2
done
tables | grep -q sshuttle2 && fail "table still loaded after shutdown"
if docker exec $p-client curl -s --max-time 3 -o /dev/null http://$target_ip:8000/; then
  fail "target still reachable after shutdown"
fi
echo "ok: table removed on shutdown"

# A helper killed outright leaves the table; `sshuttle2 cleanup` finds
# it in the state file and removes it.
docker exec -d $p-client sh -c "NO_COLOR=1 sshuttle2 -r tester@$p-sshd --method nft 0/0 -vv \
  -e 'ssh -i /root/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR' \
  >/tmp/client2.log 2>&1"
wait_connected /tmp/client2.log
docker exec $p-client pkill -KILL -f 'sshuttle2 helper'
for _ in $(seq 1 50); do
  docker exec $p-client pgrep -f 'sshuttle2 -r' >/dev/null || break
  sleep 0.2
done
tables | grep -q sshuttle2 || fail "table gone without cleanup; the check proves nothing"
docker exec $p-client sshuttle2 cleanup | grep -q "removed sshuttle2-" || fail "cleanup removed nothing"
tables | grep -q sshuttle2 && fail "table still loaded after cleanup"
echo "ok: cleanup removed the table a killed helper left"
