#!/usr/bin/env bash
# End-to-end check of tun mode (TCP, UDP, DNS) in Docker.
#
#   scripts/e2e-tun.sh
#
# Three containers:
#   client  runs sshuttle2 as root with a tun device
#   sshd    the tunnel server; also on a private network the client is not on
#   target  on that private network only: a web server, a UDP echo and a
#           TCP echo
# The client can reach the target only through the tunnel.
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
work=$(mktemp -d)
# Override these to run several copies at once.
net=${E2E_NET:-250}
far=172.31.$net.0/24
target_ip=172.31.$net.10
p=${E2E_PREFIX:-ts-e2e-tun}

cleanup() {
  docker rm -f $p-client $p-sshd $p-target >/dev/null 2>&1 || true
  docker network rm $p-front $p-far >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
cleanup
work=$(mktemp -d)

ssh-keygen -q -t ed25519 -N "" -f "$work/key"
cp "$bin" "$work/sshuttle2"
cat >"$work/Dockerfile" <<'EOF'
FROM alpine:3.20
RUN apk add --no-cache openssh python3 curl iproute2 bind-tools && ssh-keygen -A && \
    adduser -D -s /bin/sh tester && passwd -u tester && mkdir -p /home/tester/.ssh
COPY key.pub /home/tester/.ssh/authorized_keys
COPY key /root/key
COPY sshuttle2 /usr/local/bin/sshuttle2
RUN chown -R tester /home/tester/.ssh && chmod 600 /home/tester/.ssh/authorized_keys /root/key
EOF
docker build -q -t $p "$work" >/dev/null

docker network create $p-front >/dev/null
docker network create --internal --subnet $far $p-far >/dev/null
docker run -d --name $p-target --network $p-far --ip $target_ip $p sh -c '
  head -c 50000000 /dev/urandom > /tmp/big.bin
  cd /tmp && python3 -m http.server 8000 &
  exec python3 -c "
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind((\"0.0.0.0\", 9999))
while True:
    d, a = s.recvfrom(65535)
    s.sendto(b\"echo:\" + d, a)
"' >/dev/null
# TCP echo: echoes until the client's FIN, then sends its own.
docker exec -d $p-target python3 -c '
import socket, threading
def serve(c):
    with c:
        while d := c.recv(1 << 16):
            c.sendall(d)
        c.shutdown(socket.SHUT_WR)
s = socket.create_server(("0.0.0.0", 7777))
while True:
    threading.Thread(target=serve, args=(s.accept()[0],), daemon=True).start()
'
docker run -d --name $p-sshd --network $p-front $p /usr/sbin/sshd -D -e >/dev/null
docker network connect $p-far $p-sshd
docker run -d --name $p-client --network $p-front --cap-add NET_ADMIN --device /dev/net/tun ${E2E_CLIENT_SYSCTL:+--sysctl $E2E_CLIENT_SYSCTL} \
  $p sleep infinity >/dev/null

fail() {
  echo "FAIL: $*" >&2
  docker exec $p-client cat /tmp/client.log >&2 || true
  exit 1
}

# Without the tunnel the target is unreachable.
if docker exec $p-client curl -s --max-time 3 -o /dev/null http://$target_ip:8000/; then
  fail "target reachable without the tunnel; the test proves nothing"
fi

docker exec -d $p-client sh -c "sshuttle2 -r tester@$p-sshd --method tun $far -vv \
  -e 'ssh -i /root/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR' \
  >/tmp/client.log 2>&1"
for _ in $(seq 1 100); do
  docker exec $p-client sh -c 'grep -q "connected to" /tmp/client.log' 2>/dev/null && break
  sleep 0.2
done
docker exec $p-client sh -c 'grep -q "connected to" /tmp/client.log' || fail "client did not connect"

# TCP: a 50 MB download arrives intact.
want=$(docker exec $p-target sha256sum /tmp/big.bin | cut -c1-64)
got=$(docker exec $p-client sh -c "curl -sS --max-time 60 http://$target_ip:8000/big.bin | sha256sum" | cut -c1-64) ||
  fail "download through the tunnel failed"
[ "$got" = "$want" ] || fail "downloaded bytes differ"
speed=$(docker exec $p-client curl -sS -o /dev/null -w '%{speed_download}' http://$target_ip:8000/big.bin)
echo "ok: TCP 50 MB byte-exact ($(awk -v s="$speed" 'BEGIN{printf "%.0f", s*8/1e6}') Mbit/s)"

# TCP half-close: the app sends, shuts down its write side, then reads the
# echo. Everything must come back before the FIN: a FIN from the tunnel side
# while the echo is still flowing cuts off its tail.
docker exec $p-client python3 -c '
import random, socket, sys, threading
def check(n):
    data = random.Random(n).randbytes(n)
    s = socket.create_connection((sys.argv[1], 7777), timeout=30)
    def send():
        s.sendall(data)
        s.shutdown(socket.SHUT_WR)
    t = threading.Thread(target=send)
    t.start()
    got = bytearray()
    while b := s.recv(1 << 16):
        got += b
    t.join()
    s.close()
    if got != data:
        i = next((i for i, (a, b) in enumerate(zip(got, data)) if a != b), min(len(got), n))
        sys.exit(f"sent {n} bytes, echo returned {len(got)}, first difference at byte {i}")
for n in (1000, 2_000_000):
    check(n)
' $target_ip || fail "TCP echo after a half-close lost data"
echo "ok: TCP half-close, 1 KB and 2 MB echoes byte-exact"

# UDP: the echo comes back, from the target's address.
docker exec $p-client python3 -c "
import socket, sys
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(5)
for i in range(3):
    s.sendto(b'ping%d' % i, ('$target_ip', 9999))
    d, a = s.recvfrom(65535)
    assert d == b'echo:ping%d' % i, d
    assert a == ('$target_ip', 9999), a
big = bytes(range(256)) * 32
s.sendto(big, ('$target_ip', 9999))
d, a = s.recvfrom(65535)
assert d == b'echo:' + big, len(d)
" || fail "UDP echo through the tunnel failed"
echo "ok: UDP echo (small and 8 KiB datagrams), replies from the target"

# DNS: port 53 to any tunneled address is answered by the server's resolver
# (Docker's, which knows the target's name).
ans=$(docker exec $p-client dig +short +time=5 +tries=1 @172.31.$net.53 $p-target) || fail "DNS query failed"
echo "$ans" | grep -qx "$target_ip" || fail "DNS answered '$ans', expected $target_ip"
echo "ok: DNS through the tunnel resolved $p-target to $target_ip"

# Shutdown removes the routes.
docker exec $p-client sh -c 'kill -INT $(pidof sshuttle2 | tr " " "\n" | head -1)'
for _ in $(seq 1 50); do
  docker exec $p-client sh -c "ip route | grep -q '$far'" || break
  sleep 0.2
done
if docker exec $p-client sh -c "ip route | grep -q '$far'"; then
  fail "route to $far still present after shutdown"
fi
echo "ok: routes removed on shutdown"
