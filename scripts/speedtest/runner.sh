#!/usr/bin/env bash
# The GitHub runner's side of the speed test (.github/workflows/speedtest.yml),
# one subcommand per workflow step:
#
#   runner.sh init                        per-run root password, ssh keys, runner address
#   runner.sh create REGION_A [REGION_B]  the VMs (needs LIGHTNODE_TOKEN)
#   runner.sh fetch-build ARTIFACT DIR    the build job's artifact, once uploaded (needs GH_TOKEN)
#   runner.sh setup SSHUTTLE2             logs in once with the password, then installs everything
#   runner.sh run ROUNDS                  starts run.sh on the client VM and follows its log
#   runner.sh collect DIR                 results.jsonl, layout.json, results.md, results.json
#   runner.sh cleanup REGION...           deletes this run's VMs (needs LIGHTNODE_TOKEN)
#
# VMs: a client (2 vCPU / 4 GB) and two servers (1 vCPU / 2 GB, 2 vCPU / 4 GB)
# in REGION_A ("near"); two more servers in REGION_B ("far") if given.
# The repository is public and so are its workflow logs: the password and
# every address are masked (::add-mask::) as soon as they are known, and
# collect removes the addresses from the result files.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
state=${SPEEDTEST_STATE:-${RUNNER_TEMP:-/tmp}/speedtest}
prefix=s2bench-${GITHUB_RUN_ID:-local}
ln=$here/lightnode.sh
ssh_opts=(-F "$state/ssh_config")
mkdir -p "$state"
chmod 700 "$state"

mask() { if [ "${GITHUB_ACTIONS:-}" = true ]; then echo "::add-mask::$1"; fi; }

ssh_config() {
  local role ip
  while read -r role ip; do
    printf 'Host %s\n  HostName %s\n' "$role" "$ip"
  done
  printf 'Host *\n  User root\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n'
  printf '  LogLevel ERROR\n  ConnectTimeout 15\n  ServerAliveInterval 15\n  ServerAliveCountMax 4\n'
  [ $# -eq 0 ] || printf '  IdentityFile %s\n  IdentitiesOnly yes\n' "$1"
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case $cmd in
init)
  # LightNode wants letters, digits and a special character.
  pw="Aa1@$(openssl rand -hex 12)"
  mask "$pw"
  (umask 077 && printf '%s' "$pw" >"$state/password")
  rm -f "$state"/key* "$state"/peer*
  ssh-keygen -q -t ed25519 -N '' -C s2bench-runner -f "$state/key"
  ssh-keygen -q -t ed25519 -N '' -C s2bench-client -f "$state/peer"
  ip=$(curl -fsS --max-time 20 --retry 3 https://api.ipify.org)
  mask "$ip"
  echo "$ip" >"$state/runner-ip"
  ;;

create)
  region_a=${1:?region A} region_b=${2:-}
  # Leftovers of an earlier run that could not clean up.
  for r in $region_a $region_b; do "$ln" sweep s2bench- "$r"; done
  roles="client:$region_a:2:4 near-1c:$region_a:1:2 near-2c:$region_a:2:4"
  [ -z "$region_b" ] || roles+=" far-1c:$region_b:1:2 far-2c:$region_b:2:4"
  : >"$state/vms"
  for spec in $roles; do
    IFS=: read -r role region cpu mem <<<"$spec"
    zone=$("$ln" zone "$region")
    found=$("$ln" package "$region" "$zone" "$cpu" "$mem" "${TRAFFIC_GB:-1000}")
    read -r pkg traffic <<<"$found"
    image=$("$ln" image "$region" "${IMAGE:-Ubuntu 24.04}")
    created=$(VM_PASSWORD=$(cat "$state/password") "$ln" create "$prefix-$role" "$region" "$zone" "$pkg" "$image")
    read -r task uuid <<<"$created"
    echo "$role: $region, $cpu vCPU / $mem GB, package $pkg ($traffic GB traffic), $uuid"
    echo "$role $region $uuid $task $pkg $traffic $cpu $mem" >>"$state/vms"
  done
  while read -r -u 3 role _ _ task _; do
    "$ln" wait "$task"
    echo "$role: created"
  done 3<"$state/vms"
  : >"$state/hosts"
  while read -r -u 3 role _ uuid _; do
    ip=$("$ln" ip "$uuid")
    mask "$ip"
    echo "$role $ip" >>"$state/hosts"
    echo "$role: running"
  done 3<"$state/vms"
  # What the report says about the VMs; no addresses.
  jq -Rn --arg commit "${GITHUB_SHA:-}" \
    --arg run "${GITHUB_SERVER_URL:-}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}" \
    '{commit: $commit, run_url: $run,
      vms: ([inputs | split(" ") | {(.[0]): {region: .[1], package: .[4], traffic_gb: .[5],
        cpus: (.[6] | tonumber), mem_gb: (.[7] | tonumber)}}] | add)}' \
    <"$state/vms" >"$state/layout.json"
  ;;

fetch-build)
  name=${1:?artifact} dir=${2:?directory}
  run=repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID
  for _ in $(seq 1 160); do
    id=$(gh api "$run/artifacts" --jq ".artifacts[] | select(.name == \"$name\") | .id" | head -1)
    if [ -n "$id" ]; then
      mkdir -p "$dir"
      gh api "repos/$GITHUB_REPOSITORY/actions/artifacts/$id/zip" >"$state/build.zip"
      unzip -o -q "$state/build.zip" -d "$dir"
      ls -l "$dir"
      exit 0
    fi
    case $(gh api "$run/jobs" --jq '.jobs[] | select(.name == "build") | .conclusion // ""') in
      failure | cancelled | skipped)
        echo "the build job did not succeed" >&2
        exit 1
        ;;
    esac
    sleep 15
  done
  echo "no $name artifact after 40 minutes" >&2
  exit 1
  ;;

setup)
  bin=${1:?sshuttle2 binary}
  ssh_config "$state/key" <"$state/hosts" >"$state/ssh_config"
  # The one password login: it installs the runner's key.
  export SSHPASS
  SSHPASS=$(cat "$state/password")
  while read -r -u 3 role ip; do
    for i in $(seq 1 60); do
      if sshpass -e ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
        -o ConnectTimeout=10 -o PreferredAuthentications=password -o PubkeyAuthentication=no "root@$ip" \
        'umask 077 && mkdir -p ~/.ssh && cat >>~/.ssh/authorized_keys' <"$state/key.pub"; then
        echo "$role: logged in"
        break
      fi
      [ "$i" -lt 60 ] || { echo "$role: no ssh login after 10 minutes" >&2; exit 1; }
      sleep 10
    done
  done 3<"$state/hosts"
  unset SSHPASS
  # The client logs in to the servers with its own key.
  grep -v '^client ' "$state/hosts" | ssh_config >"$state/client_ssh_config"
  scp "${ssh_opts[@]}" -q "$here/setup-vm.sh" "$here/sample.py" "$here/run.sh" "$bin" client:
  scp "${ssh_opts[@]}" -q "$state/peer" client:.ssh/id_ed25519
  scp "${ssh_opts[@]}" -q "$state/client_ssh_config" client:.ssh/config
  pids=()
  while read -r -u 3 role _; do
    kind=client
    if [ "$role" != client ]; then
      kind=server
      scp "${ssh_opts[@]}" -q "$here/setup-vm.sh" "$here/sample.py" "$role":
      ssh -n "${ssh_opts[@]}" "$role" "cat >>~/.ssh/authorized_keys <<'EOF'
$(cat "$state/peer.pub")
EOF"
    fi
    ssh -n "${ssh_opts[@]}" "$role" "chmod +x setup-vm.sh sample.py run.sh sshuttle2 2>/dev/null
      SSHUTTLE_VERSION='$SSHUTTLE_VERSION' OOKLA_VERSION='$OOKLA_VERSION' OOKLA_SHA256='${OOKLA_SHA256:-}' ./setup-vm.sh $kind" \
      >"$state/setup-$role.log" 2>&1 &
    pids+=("$!:$role")
  done 3<"$state/hosts"
  failed=0
  for p in "${pids[@]}"; do
    wait "${p%%:*}" || failed=1
    sed "s/^/${p#*:}: /" "$state/setup-${p#*:}.log"
  done
  exit $failed
  ;;

run)
  rounds=${1:?rounds}
  [[ $rounds =~ ^[1-9][0-9]?$ ]] || { echo "rounds must be a number from 1 to 99" >&2; exit 1; }
  servers=$(awk '$1 != "client" {printf "%s ", $1}' "$state/hosts")
  ssh -n "${ssh_opts[@]}" client "SERVERS='$servers' ROUNDS=$rounds RUNNER_IP=$(cat "$state/runner-ip") ${MODES:+MODES='$MODES'} \
    setsid nohup ./run.sh >run.log 2>&1 </dev/null &"
  # Follow run.log until run.done appears. The test runs on even if this
  # connection drops for a while.
  off=0 misses=0
  while :; do
    sleep 20
    if ssh -n "${ssh_opts[@]}" client "tail -c +$((off + 1)) run.log" >"$state/chunk" 2>/dev/null; then
      misses=0
      cat "$state/chunk"
      off=$((off + $(wc -c <"$state/chunk")))
      status=$(ssh -n "${ssh_opts[@]}" client \
        'if [ -f run.done ]; then cat run.done; elif pgrep -f "[r]un.sh" >/dev/null; then echo running; else echo "failed: run.sh stopped"; fi' ||
        echo running)
      case $status in
        ok) exit 0 ;;
        failed*) echo "$status" >&2; exit 1 ;;
      esac
    else
      misses=$((misses + 1))
      echo "(no answer from the client VM, $misses in a row)"
      [ "$misses" -lt 15 ] || { echo "lost the client VM" >&2; exit 1; }
    fi
  done
  ;;

collect)
  dir=${1:?directory}
  [ -s "$state/hosts" ] || { echo "no VMs, no results"; exit 0; }
  mkdir -p "$dir"
  scp "${ssh_opts[@]}" -q client:results.jsonl "$dir/results.jsonl"
  cp "$state/layout.json" "$dir/"
  # run.sh writes no addresses; this makes sure.
  { awk '{print $2}' "$state/hosts"; cat "$state/runner-ip"; } >"$state/addresses"
  while read -r ip; do
    sed -i "s/${ip//./\\.}/<ip>/g" "$dir/results.jsonl"
  done <"$state/addresses"
  python3 "$here/report.py" "$dir/results.jsonl" --layout "$dir/layout.json" \
    --md "$dir/results.md" --json "$dir/results.json"
  if grep -rqF -f "$state/addresses" "$dir"; then
    echo "an address is still in the result files; not publishing them" >&2
    rm -rf "$dir"
    exit 1
  fi
  ;;

cleanup)
  failed=0
  # First every VM this run created, by its ID; then by name, for one whose
  # create call failed before it answered.
  if [ -s "$state/vms" ]; then
    while read -r -u 3 role _ uuid _; do
      echo "deleting $role ($uuid)"
      "$ln" delete "$uuid" || failed=1
    done 3<"$state/vms"
  fi
  for r in "$@"; do
    [ -z "$r" ] || "$ln" sweep "$prefix-" "$r" || failed=1
  done
  exit $failed
  ;;

*)
  sed -n '2,14p' "$0" >&2
  exit 2
  ;;
esac
