#!/usr/bin/env bash
# Linux client, macOS server: run on a Mac with Docker. CI has no Mac to ssh
# into, so this is the end-to-end check for the macOS servers.
#
#   scripts/mac-server-check.sh
#
# On the Mac (no sudo, nothing in /etc changes):
#   - a user-level sshd on 127.0.0.1, with its own keys and config in a temp
#     directory, which also holds the server cache (XDG_CACHE_HOME)
#   - test services on the Mac's loopback: a web server, a TCP echo and a UDP
#     echo, reachable only from the Mac itself
# In Docker: Linux clients on an internal network whose only way out is a
# forwarder to that sshd. Whatever they reach went through the Mac.
#
# Checks: the upload, then a cache hit; SOCKS5 TCP and UDP to the Mac's
# loopback; tun and nft mode to the internet (needs the Mac to be online).
#
# Needs a Linux client that embeds the macOS servers:
#   cargo xtask servers
#   cargo xtask client <arch>-unknown-linux-musl
set -euo pipefail

[ "$(uname -s)" = Darwin ] || { echo "run this on a Mac" >&2; exit 2; }
root=$(cd "$(dirname "$0")/.." && pwd)
arch=$(docker info --format '{{.Architecture}}' 2>/dev/null)
case "$arch" in aarch64|arm64) arch=aarch64 ;; *) arch=x86_64 ;; esac
bin=${SSHUTTLE2_LINUX_BIN:-$root/target/$arch-unknown-linux-musl/release/sshuttle2}
[ -x "$bin" ] || { echo "missing Linux client binary $bin" >&2; exit 2; }
# Override these if the ports are taken.
sshd_port=${MAC_SSHD_PORT:-22220}
http_port=${MAC_HTTP_PORT:-22281}
tcp_port=${MAC_TCP_PORT:-22282}
udp_port=${MAC_UDP_PORT:-22283}
p=${E2E_PREFIX:-ts-mac-server}
user=$(id -un)
work=$(mktemp -d)
pids=()

cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  docker rm -f $p-client $p-fwd >/dev/null 2>&1 || true
  docker network rm $p-int >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

# The Mac side.
ssh-keygen -q -t ed25519 -N "" -f "$work/host_key"
ssh-keygen -q -t ed25519 -N "" -f "$work/key"
cp "$work/key.pub" "$work/authorized_keys"
cat >"$work/sshd_config" <<EOF
ListenAddress 127.0.0.1
Port $sshd_port
HostKey $work/host_key
AuthorizedKeysFile $work/authorized_keys
PidFile $work/sshd.pid
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
AllowUsers $user
SetEnv XDG_CACHE_HOME=$work/cache
EOF
/usr/sbin/sshd -D -e -f "$work/sshd_config" 2>"$work/sshd.log" &
pids+=($!)

head -c 50000000 /dev/urandom >"$work/big.bin"
want=$(shasum -a 256 "$work/big.bin" | cut -c1-64)
python3 - "$work" "$http_port" "$tcp_port" "$udp_port" 2>"$work/services.log" <<'EOF' &
import functools, http.server, socket, sys, threading
work, http_port, tcp_port, udp_port = sys.argv[1], *map(int, sys.argv[2:])
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=work)
web = http.server.ThreadingHTTPServer(("127.0.0.1", http_port), handler)
threading.Thread(target=web.serve_forever, daemon=True).start()
def echo(c):
    with c:
        while d := c.recv(1 << 16):
            c.sendall(d)
        c.shutdown(socket.SHUT_WR)
def tcp():
    s = socket.create_server(("127.0.0.1", tcp_port))
    while True:
        threading.Thread(target=echo, args=(s.accept()[0],), daemon=True).start()
threading.Thread(target=tcp, daemon=True).start()
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.bind(("127.0.0.1", udp_port))
while True:
    d, a = u.recvfrom(65535)
    u.sendto(b"echo:" + d, a)
EOF
pids+=($!)

# The Linux side.
cp "$bin" "$work/sshuttle2"
cat >"$work/Dockerfile" <<'EOF'
FROM alpine:3.20
RUN apk add --no-cache openssh-client python3 curl bind-tools iproute2 nftables socat
COPY key /root/key
COPY sshuttle2 /usr/local/bin/sshuttle2
RUN chmod 600 /root/key
EOF
docker build -q -t $p "$work" >/dev/null
docker network create --internal $p-int >/dev/null
docker run -d --name $p-fwd $p \
  socat TCP-LISTEN:2222,fork,reuseaddr TCP:host.docker.internal:$sshd_port >/dev/null
docker network connect $p-int $p-fwd
docker run -d --name $p-client --network $p-int --cap-add NET_ADMIN --device /dev/net/tun \
  $p sleep infinity >/dev/null
# nft mode redirects only connections that have a route. The forwarder does
# not forward packets, so this default route leads nowhere by itself.
fwd_ip=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$p-int\").IPAddress}}" $p-fwd)
docker exec $p-client ip route add default via "$fwd_ip"

fail() {
  echo "FAIL: $*" >&2
  docker exec $p-client cat /tmp/client.log >&2 || true
  echo "--- sshd log" >&2
  cat "$work/sshd.log" >&2
  exit 1
}

# Without the tunnel the client reaches nothing.
if docker exec $p-client curl -s --max-time 3 -o /dev/null https://1.1.1.1/; then
  fail "the client reaches the internet without the tunnel; the test proves nothing"
fi

# start ARGS...: runs the client in the background and waits until it is up.
start() {
  docker exec -d $p-client sh -c "NO_COLOR=1 sshuttle2 -r $user@$p-fwd:2222 $* -vv \
    -e 'ssh -i /root/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR' \
    >/tmp/client.log 2>&1"
  for _ in $(seq 1 150); do
    docker exec $p-client grep -q "connected to" /tmp/client.log 2>/dev/null && return
    sleep 0.2
  done
  fail "client did not connect"
}
stop() {
  docker exec $p-client pkill -INT -f 'sshuttle2 -r' || true
  for _ in $(seq 1 50); do
    docker exec $p-client pgrep -f 'sshuttle2 -r' >/dev/null || return 0
    sleep 0.2
  done
  fail "client did not exit"
}

# SOCKS5, first run: the client uploads the macOS server.
start --socks5 127.0.0.1:1080
docker exec $p-client grep -q "uploading server" /tmp/client.log || fail "first run did not upload the server"
cached=$(ls "$work/cache/sshuttle2/")
file "$work/cache/sshuttle2/$cached" | grep -q Mach-O || fail "cached server is not a macOS binary: $cached"
echo "ok: uploaded the macOS server ($cached)"

got=$(docker exec $p-client sh -c \
  "curl -sS --max-time 60 --socks5 127.0.0.1:1080 http://127.0.0.1:$http_port/big.bin | sha256sum" | cut -c1-64) ||
  fail "download from the Mac's loopback failed"
[ "$got" = "$want" ] || fail "downloaded bytes differ"
speed=$(docker exec $p-client curl -sS -o /dev/null -w '%{speed_download}' \
  --socks5 127.0.0.1:1080 http://127.0.0.1:$http_port/big.bin)
echo "ok: SOCKS5 TCP, 50 MB from the Mac's loopback byte-exact ($(awk -v s="$speed" 'BEGIN{printf "%.0f", s*8/1e6}') Mbit/s)"

# TCP half-close: the echo's tail must arrive before the FIN.
docker exec $p-client python3 - "$tcp_port" <<'EOF' || fail "SOCKS5 TCP echo after a half-close lost data"
import random, socket, struct, sys, threading
def check(n):
    s = socket.create_connection(("127.0.0.1", 1080), timeout=30)
    s.sendall(b"\x05\x01\x00")
    assert s.recv(2) == b"\x05\x00"
    s.sendall(b"\x05\x01\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack(">H", int(sys.argv[1])))
    r = s.recv(10)
    assert r[1] == 0, r
    data = random.Random(n).randbytes(n)
    def send():
        s.sendall(data)
        s.shutdown(socket.SHUT_WR)
    t = threading.Thread(target=send)
    t.start()
    got = bytearray()
    while b := s.recv(1 << 16):
        got += b
    t.join()
    if got != data:
        sys.exit(f"sent {n} bytes, echo returned {len(got)}")
for n in (1000, 2_000_000):
    check(n)
EOF
echo "ok: SOCKS5 TCP half-close, 1 KB and 2 MB echoes byte-exact"

docker exec $p-client python3 - "$udp_port" <<'EOF' || fail "SOCKS5 UDP echo failed"
import socket, struct, sys
port = int(sys.argv[1])
c = socket.create_connection(("127.0.0.1", 1080), timeout=10)
c.sendall(b"\x05\x01\x00")
assert c.recv(2) == b"\x05\x00"
c.sendall(b"\x05\x03\x00\x01\x00\x00\x00\x00\x00\x00")
r = c.recv(10)
assert r[1] == 0, r
relay = (socket.inet_ntoa(r[4:8]), struct.unpack(">H", r[8:10])[0])
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.settimeout(5)
head = b"\x00\x00\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack(">H", port)
for d in (b"ping", bytes(range(256)) * 32):
    u.sendto(head + d, relay)
    got = u.recvfrom(65535)[0]
    assert got == head + b"echo:" + d, got[:40]
EOF
echo "ok: SOCKS5 UDP, small and 8 KiB datagrams echoed by the Mac's loopback"
stop

# The second run finds the server in the cache.
start --socks5 127.0.0.1:1080
if docker exec $p-client grep -q "uploading server" /tmp/client.log; then
  fail "second run uploaded again instead of using the cache"
fi
echo "ok: second run used the cached server"
stop

# tun mode: TCP and DNS to the internet, through the Mac.
start --method tun 0/0
ip=$(docker exec $p-client dig +short +time=5 +tries=1 @1.1.1.1 example.com A | grep -E '^[0-9.]+$' | head -1) ||
  true
[ -n "$ip" ] || fail "tun: DNS through the Mac got no answer"
code=$(docker exec $p-client curl -sS --max-time 20 -o /dev/null -w '%{http_code}' \
  --resolve "example.com:443:$ip" https://example.com/) || fail "tun: HTTPS through the Mac failed"
echo "ok: tun mode, DNS (example.com is $ip) and HTTPS ($code) through the Mac"
stop
if docker exec $p-client ip route show table all | grep -q tun; then
  fail "tun: routes left after shutdown"
fi

# nft mode: TCP and DNS (the container's resolver) through the Mac.
start 0/0 --dns
code=$(docker exec $p-client curl -sS --max-time 20 -o /dev/null -w '%{http_code}' https://example.com/) ||
  fail "nft: HTTPS through the Mac failed"
echo "ok: nft mode, DNS and HTTPS ($code) through the Mac"
stop
if docker exec $p-client nft list ruleset | grep -q sshuttle2; then
  fail "nft: rules left after shutdown"
fi
echo "all ok"
