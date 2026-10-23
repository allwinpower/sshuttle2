#!/usr/bin/env bash
# End-to-end check of the SOCKS5 path. Needs no root.
#
#   scripts/e2e.sh local    server runs as a local child process (-r local)
#   scripts/e2e.sh docker   server is uploaded over real ssh into an Alpine
#                           (busybox) container with a login MOTD
#
# Expects target/release/sshuttle2 (and, for docker, a client built by
# `cargo xtask dist` so it embeds the Linux servers).
set -euo pipefail

mode=${1:-local}
root=$(cd "$(dirname "$0")/.." && pwd)
bin=$root/target/release/sshuttle2
work=$(mktemp -d)
size=${E2E_BYTES:-300000000}
socks=127.0.0.1:${E2E_SOCKS_PORT:-11090}
# Override these to run several copies at once.
name=${E2E_PREFIX:-ts-e2e}-sshd
ssh_port=${E2E_SSH_PORT:-2299}
http_port=${E2E_HTTP_PORT:-18090}
pids=()

cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  [ "$mode" = docker ] && docker rm -f $name >/dev/null 2>&1
  rm -rf "$work"
}
trap cleanup EXIT

wait_port() {
  for _ in $(seq 1 100); do
    if (exec 3<>"/dev/tcp/${1%:*}/${1#*:}") 2>/dev/null; then return 0; fi
    sleep 0.1
  done
  echo "nothing listening on $1" >&2
  return 1
}

fetch() {  # url outfile -> prints speed in Mbit/s
  curl -sS --fail --max-time 120 --socks5-hostname "$socks" -o "$2" \
    -w '%{speed_download}' "$1" | awk '{printf "%.0f", $1 * 8 / 1e6}'
}

case "$mode" in
local)
  mkdir -p "$work/www"
  head -c "$size" /dev/urandom >"$work/www/big.bin"
  head -c 20000000 "$work/www/big.bin" >"$work/www/mid.bin"
  want=$(shasum -a 256 "$work/www/big.bin" | cut -c1-64)
  (cd "$work/www" && exec python3 -m http.server $http_port --bind 127.0.0.1) >/dev/null 2>&1 &
  pids+=($!)
  wait_port 127.0.0.1:$http_port
  "$bin" -r local --socks5 "$socks" >"$work/client.log" 2>&1 &
  pids+=($!)
  url=http://127.0.0.1:$http_port/big.bin
  ;;
docker)
  ssh-keygen -q -t ed25519 -N "" -f "$work/key"
  cat >"$work/Dockerfile" <<'EOF'
FROM alpine:3.20
RUN apk add --no-cache openssh python3 && ssh-keygen -A && \
    adduser -D -s /bin/sh tester && passwd -u tester && mkdir -p /home/tester/.ssh && \
    echo "Welcome (MOTD noise the client must skip)" > /etc/motd
COPY key.pub /home/tester/.ssh/authorized_keys
RUN chown -R tester /home/tester/.ssh && chmod 600 /home/tester/.ssh/authorized_keys
CMD ["/usr/sbin/sshd", "-D", "-e"]
EOF
  docker build -q -t $name "$work" >/dev/null
  docker rm -f $name >/dev/null 2>&1 || true
  docker run -d --name $name -p 127.0.0.1:$ssh_port:22 $name >/dev/null
  docker exec -d $name sh -c \
    "head -c $size /dev/urandom > /tmp/big.bin && head -c 20000000 /tmp/big.bin > /tmp/mid.bin && cd /tmp && exec python3 -m http.server 8000 --bind 127.0.0.1"
  wait_port 127.0.0.1:$ssh_port
  for _ in $(seq 1 100); do
    docker exec $name sh -c 'test -s /tmp/big.bin && wget -q -O /dev/null http://127.0.0.1:8000/' 2>/dev/null && break
    sleep 0.2
  done
  want=$(docker exec $name sha256sum /tmp/big.bin | cut -c1-64)
  ssh_cmd="ssh -i $work/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"
  "$bin" -r tester@127.0.0.1:$ssh_port -e "$ssh_cmd" --socks5 "$socks" >"$work/client.log" 2>&1 &
  pids+=($!)
  url=http://127.0.0.1:8000/big.bin
  ;;
*)
  echo "usage: $0 local|docker" >&2
  exit 2
  ;;
esac

wait_port "$socks"
sleep 1
fail() { echo "FAIL: $*" >&2; sed 's/\x1b\[[0-9;]*m//g' "$work/client.log" >&2; exit 1; }

speed=$(fetch "$url" "$work/got.bin") || fail "download through the tunnel failed"
got=$(shasum -a 256 "$work/got.bin" | cut -c1-64)
[ "$got" = "$want" ] || fail "downloaded bytes differ"
echo "ok: $mode, $((size / 1000000)) MB byte-exact at $speed Mbit/s"

# Ten downloads at once share the tunnel without corrupting each other.
# (python's http.server ignores Range, hence a separate 20 MB file.)
curls=()
for i in $(seq 1 10); do
  curl -sS --fail --max-time 120 --socks5-hostname "$socks" -o "$work/part$i" "${url%big.bin}mid.bin" &
  curls+=($!)
done
for c in "${curls[@]}"; do wait "$c" || fail "parallel download failed"; done
first=$(head -c 20000000 "$work/got.bin" | shasum -a 256 | cut -c1-64)
for i in $(seq 1 10); do
  [ "$(shasum -a 256 "$work/part$i" | cut -c1-64)" = "$first" ] || fail "parallel download $i differs"
done
echo "ok: 10 parallel downloads match"

# A refused connection fails fast with a SOCKS error, not a hang.
if curl -sS --max-time 10 --socks5-hostname "$socks" http://127.0.0.1:1/ 2>"$work/err"; then
  fail "connect to a closed port succeeded"
fi
grep -qiE "refused|socks" "$work/err" || fail "unexpected error: $(cat "$work/err")"
echo "ok: refused connection reported"
