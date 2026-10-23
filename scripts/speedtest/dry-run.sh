#!/usr/bin/env bash
# The speed test's client side (run.sh, sample.py, report.py) in Docker, one
# round of every mode, with a stand-in for Ookla's CLI. No LightNode, no
# internet test: it checks that every tunnel starts and stops cleanly, that
# the samplers find the tunnel's processes, and that the report renders.
#
#   scripts/speedtest/dry-run.sh
#
# Three Ubuntu containers, as on the real VMs:
#   client  run.sh, sshuttle (PyPI) and sshuttle2
#   server  "near-1c": sshd; also on a private network the client is not on
#   target  on that private network only; the stand-in "speedtest" downloads
#           from and uploads to it
# So "direct" fails (the target is unreachable without a tunnel) and every
# other mode must work.
#
# Needs a static Linux client, as scripts/e2e-tun.sh does (its header says
# how to build one); SSHUTTLE2_LINUX_BIN points at it.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
arch=$(docker info --format '{{.Architecture}}' 2>/dev/null)
case "$arch" in aarch64|arm64) arch=aarch64 ;; *) arch=x86_64 ;; esac
bin=${SSHUTTLE2_LINUX_BIN:-$root/target/$arch-unknown-linux-musl/release/sshuttle2}
[ -x "$bin" ] || { echo "missing Linux client binary $bin" >&2; exit 2; }
sshuttle_version=${SSHUTTLE_VERSION:-$(awk -F'"' '/SSHUTTLE_VERSION:/ {print $2; exit}' "$root/.github/workflows/speedtest.yml")}
net=${E2E_NET:-248}
far=172.31.$net.0/24
target_ip=172.31.$net.10
p=${E2E_PREFIX:-ts-speedtest}

cleanup() {
  docker rm -f $p-client $p-server $p-target >/dev/null 2>&1 || true
  docker network rm $p-front $p-far >/dev/null 2>&1 || true
  rm -rf "${work:-}"
}
trap cleanup EXIT
cleanup
work=$(mktemp -d)

ssh-keygen -q -t ed25519 -N "" -f "$work/key"
cat >"$work/speedtest" <<EOF
#!/bin/bash
# Stand-in for Ookla's speedtest CLI: the same JSON shape, measured against
# the target container.
case " \$* " in
  *" --version "*) echo "Speedtest by Ookla 0.0.0 (stand-in)"; exit 0 ;;
  *" -L "*) echo '{"type":"serverList","servers":[{"id":1,"host":"target:8000","name":"Stand-in","location":"Docker","country":"Nowhere"}]}'; exit 0 ;;
esac
t=http://$target_ip:8000/
ping=\$(curl -sf -I -o /dev/null --max-time 5 -w '%{time_connect}' "\$t") || { echo "Cannot reach the test server" >&2; exit 2; }
read -r db ds <<<"\$(curl -sf --max-time 60 -o /dev/null -w '%{size_download} %{time_total}' "\$t")"
read -r ub us <<<"\$(head -c 20000000 /dev/zero | curl -sf --max-time 60 -o /dev/null --data-binary @- -w '%{size_upload} %{time_total}' "\$t")"
python3 -c "
import json, sys
p, db, ds, ub, us = map(float, sys.argv[1:])
lat = {'iqm': p * 1000}
print(json.dumps({'type': 'result', 'ping': {'latency': p * 1000, 'jitter': 0},
    'download': {'bandwidth': db / ds, 'bytes': db, 'latency': lat},
    'upload': {'bandwidth': ub / us, 'bytes': ub, 'latency': lat}, 'packetLoss': 0}))
" "\$ping" "\$db" "\$ds" "\$ub" "\$us"
EOF
cat >"$work/target.py" <<'EOF'
import http.server

SIZE = 50_000_000


class Handler(http.server.BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.send_response(200)
        self.send_header("Content-Length", str(SIZE))
        self.end_headers()

    def do_GET(self):
        self.do_HEAD()
        chunk, left = bytes(65536), SIZE
        while left > 0:
            self.wfile.write(chunk[:min(left, len(chunk))])
            left -= len(chunk)

    def do_POST(self):
        left = int(self.headers.get("Content-Length", 0))
        while left > 0:
            data = self.rfile.read(min(left, 65536))
            if not data:
                break
            left -= len(data)
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("0.0.0.0", 8000), Handler).serve_forever()
EOF
cp "$bin" "$work/sshuttle2"
cp "$here/run.sh" "$here/sample.py" "$work/"
cat >"$work/Dockerfile" <<EOF
FROM ubuntu:24.04
RUN apt-get update -q && DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
      openssh-server openssh-client python3 python3-venv jq curl iproute2 iptables nftables procps ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && python3 -m venv /opt/sshuttle && /opt/sshuttle/bin/pip install -q --disable-pip-version-check sshuttle==$sshuttle_version \
    && ln -s /opt/sshuttle/bin/sshuttle /usr/local/bin/sshuttle \
    && mkdir -p /run/sshd /root/.ssh && chmod 700 /root/.ssh
COPY key.pub /root/.ssh/authorized_keys
COPY key /root/.ssh/id_ed25519
COPY speedtest /usr/local/bin/speedtest
COPY run.sh sample.py sshuttle2 target.py /root/
RUN chmod 755 /usr/local/bin/speedtest /root/run.sh /root/sshuttle2 && chmod 600 /root/.ssh/*
EOF
docker build -q -t $p "$work" >/dev/null

docker network create $p-front >/dev/null
docker network create --internal --subnet $far $p-far >/dev/null
docker run -d --name $p-target --network $p-far --ip $target_ip $p python3 /root/target.py >/dev/null
docker run -d --name $p-server --network $p-front $p /usr/sbin/sshd -D -e >/dev/null
docker network connect $p-far $p-server
docker run -d --name $p-client --network $p-front --cap-add NET_ADMIN --device /dev/net/tun $p sleep infinity >/dev/null
server_ip=$(docker inspect -f "{{(index .NetworkSettings.Networks \"$p-front\").IPAddress}}" $p-server)
docker exec -i $p-client sh -c 'cat >/root/.ssh/config' <<EOF
Host near-1c
  HostName $server_ip
Host *
  User root
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
EOF

fail() {
  echo "FAIL: $*" >&2
  docker exec $p-client cat /root/run.log >&2 || true
  exit 1
}

docker exec -e SERVERS=near-1c -e ROUNDS=1 -e RUNNER_IP=192.0.2.1 $p-client /root/run.sh ||
  fail "run.sh exited with $?"
docker exec $p-client cat /root/run.done | grep -qx ok || fail "run.done is not ok"
docker cp -q $p-client:/root/results.jsonl "$work/results.jsonl"

check() { jq -se "$1" "$work/results.jsonl" >/dev/null || fail "$2"; }
check '[.[] | select(.type == "result")] | length == 6' "expected 6 results"
check '.[] | select(.type == "result" and .mode == "direct") | .ookla.error != null' \
  "direct reached the target without a tunnel; the test proves nothing"
check '.[] | select(.type == "result" and .mode == "server") | .ookla.download_mbps > 0' "server mode failed"
for mode in sshuttle sshuttle-nlc sshuttle2-nft sshuttle2-tun; do
  check ".[] | select(.type == \"result\" and .mode == \"$mode\") |
    .ookla.download_mbps > 0 and .ookla.upload_mbps > 0" "$mode: no download or upload through the tunnel"
  check ".[] | select(.type == \"result\" and .mode == \"$mode\") |
    .server_proc.cpu_avg > 0 and .server_proc.rss_peak_mb > 0 and .client_proc.cpu_avg > 0 and .client_proc.rss_peak_mb > 0" \
    "$mode: the samplers found no tunnel processes"
  echo "ok: $mode"
done
python3 "$here/report.py" "$work/results.jsonl" >"$work/results.md" || fail "report.py failed"
cat "$work/results.md"
echo "ok: report"
