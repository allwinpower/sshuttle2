#!/usr/bin/env bash
# LightNode API client for the speed test (see docs/SPEEDTEST.md).
#
#   lightnode.sh list [REGION...]          regions; packages and images of each REGION
#   lightnode.sh zone REGION               the region's first zone
#   lightnode.sh package REGION ZONE CPU GB
#                                          "CODE TRAFFIC" of the package with that size,
#                                          billed by traffic (no bandwidth cap), with the
#                                          least traffic a month and the smallest data disk
#   lightnode.sh image REGION REGEX        UUID of the first system image matching REGEX
#   lightnode.sh create NAME REGION ZONE PACKAGE IMAGE
#                                          "TASK UUID"; root password from VM_PASSWORD
#   lightnode.sh wait TASK                 until the async task has finished
#   lightnode.sh ip UUID                   the public IPv4 address, once the VM runs
#   lightnode.sh delete UUID               deletes one VM
#   lightnode.sh sweep PREFIX REGION       deletes every VM named PREFIX* in REGION
#
# The token comes from LIGHTNODE_TOKEN. It reaches curl through a file
# descriptor, never a command line, and nothing here prints it.
# API reference: https://apidoc.lightnode.com/en/8495480m0
set -euo pipefail

api=${LIGHTNODE_API:-https://openapi.lightnode.com}

# call METHOD PATH [JSON-BODY]: prints the response body; fails on an HTTP
# error, after 3 tries for a GET (a POST is never repeated).
call() {
  : "${LIGHTNODE_TOKEN:?LIGHTNODE_TOKEN is not set}"
  local method=$1 path=$2 tries=1 out code
  [ "$method" = GET ] && tries=3
  for i in $(seq 1 "$tries"); do
    if [ $# -ge 3 ]; then
      out=$(curl -sS --max-time 60 -X "$method" -w '\n%{http_code}' \
        -H @<(printf 'x-open-token: %s\n' "$LIGHTNODE_TOKEN") \
        -H 'Content-Type: application/json' --data-binary @- "$api$path" <<<"$3") || out=$'\n000'
    else
      out=$(curl -sS --max-time 60 -X "$method" -w '\n%{http_code}' \
        -H @<(printf 'x-open-token: %s\n' "$LIGHTNODE_TOKEN") "$api$path") || out=$'\n000'
    fi
    code=${out##*$'\n'}
    out=${out%$'\n'*}
    if [[ $code == 2* ]] && [ "$(jq -r '.success // true' <<<"$out" 2>/dev/null)" != false ]; then
      printf '%s\n' "$out"
      return 0
    fi
    [ "$i" -lt "$tries" ] && sleep 5
  done
  echo "lightnode: $method ${path%%\?*} failed (HTTP $code): $out" >&2
  return 1
}

# fields: the names (never the values) of every field in a response, for an
# error message when the API answers in a shape this script does not expect.
fields() {
  jq -r '[paths | map(select(type == "string")) | join(".")] | unique | map(select(. != "")) | join(", ")'
}

zones() {
  call GET /region/list | jq -r --arg r "$1" '.regions[] | select(.regionCode == $r) | .zones[].zoneCode'
}

# images REGION: "UUID<tab>OS<tab>NAME" for every system image, all pages
images() {
  local page=1 out
  while :; do
    out=$(call GET "/image/list?regionCode=$1&imageType=System&page=$page&pageSize=50")
    jq -r '.images[] | [.imageResourceUUID, .osVersionDetail, .imageName] | map(. // "-" | tostring) | @tsv' <<<"$out"
    [ "$(jq '.images | length' <<<"$out")" -lt 50 ] && break
    page=$((page + 1))
  done
}

# instances REGION: "UUID<tab>NAME<tab>STATUS" for every VM in every zone
instances() {
  local zone page out
  for zone in $(zones "$1"); do
    page=1
    while :; do
      out=$(call GET "/instance/list?regionCode=$1&zoneCode=$zone&page=$page&pageSize=50")
      jq -r '.instances[] | [.ecsResourceUUID, .instanceName, .ecsStatus] | @tsv' <<<"$out"
      [ "$(jq '.instances | length' <<<"$out")" -lt 50 ] && break
      page=$((page + 1))
    done
  done
}

wait_task() {
  local out
  for _ in $(seq 1 120); do
    out=$(call GET "/asynctask/getResult?asyncTaskUUID=$1")
    if [ "$(jq -r .asyncTaskInfo.taskStatus <<<"$out")" = FINISHED ]; then
      case $(jq -r .asyncTaskInfo.processResult <<<"$out") in
        SUCCESS) return 0 ;;
        *) echo "lightnode: task $1 ended with $(jq -r .asyncTaskInfo.processResult <<<"$out")" >&2; return 1 ;;
      esac
    fi
    sleep 5
  done
  echo "lightnode: task $1 did not finish in 10 minutes; its fields: $(fields <<<"$out")" >&2
  return 1
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case $cmd in
list)
  echo "== regions (code, name, zones)"
  call GET /region/list | jq -r '.regions[] | [.regionCode, .regionName, ([.zones[].zoneCode] | join(","))] | @tsv'
  for region in "$@"; do
    [ -n "$region" ] || continue
    echo
    echo "== $region: packages (code, zone, vCPU, GB, traffic GB a month, billing)"
    out=$(call GET "/package/list?regionCode=$region")
    echo "fields: $(fields <<<"$out")"
    jq -r '.packages | sort_by(.cpu, .memory)[]
      | [.packageCode, .zoneCode, .cpu, .memory, .freeFlow, .publicIpChargeMode]
      | map(. // "-" | tostring) | @tsv' <<<"$out"
    echo
    echo "== $region: Linux system images (uuid, os, name)"
    images "$region" | grep -i -E 'ubuntu|debian' || true
  done
  ;;
zone)
  z=$(zones "$1" | head -1)
  [ -n "$z" ] || { echo "lightnode: no region $1 (see: lightnode.sh list)" >&2; exit 1; }
  echo "$z"
  ;;
package)
  # Packages billed by bandwidth (PayByBandwidth, "-b5-") are capped at a few
  # Mbit/s; those billed by traffic ("-t1000-") are not. Numbers may come as
  # strings, and memory in MB.
  out=$(call GET "/package/list?regionCode=$1&zoneCode=$2")
  p=$(jq -r --argjson c "$3" --argjson m "$4" '
    def num: if type == "string" then (tonumber? // null) else . end;
    [.packages[] | select((.cpu | num) == $c and ((.memory | num) == $m or (.memory | num) == $m * 1024))
      | select(.publicIpChargeMode == "PayByTraffic" and (.freeFlow | num) != null)]
    | sort_by((.freeFlow | num), ((.dataDiskSize | num) // 0)) | .[0] // empty
    | "\(.packageCode) \(.freeFlow)"' <<<"$out")
  if [ -z "$p" ]; then
    echo "lightnode: no $3 vCPU / $4 GB package billed by traffic in $1 ($2). There:" >&2
    jq -r '.packages[] | "  \(.packageCode): \(.cpu) vCPU, \(.memory) GB, traffic \(.freeFlow // "-")"' <<<"$out" >&2
    echo "  fields: $(fields <<<"$out")" >&2
    exit 1
  fi
  echo "$p"
  ;;
image)
  i=$(images "$1" | awk -F'\t' -v re="$2" '$2 ~ re || $3 ~ re {print $1; exit}')
  if [ -z "$i" ]; then
    echo "lightnode: no image matching '$2' in $1. Images there:" >&2
    images "$1" | cut -f2,3 | sort -u | head -40 >&2
    exit 1
  fi
  echo "$i"
  ;;
create)
  : "${VM_PASSWORD:?VM_PASSWORD is not set}"
  # env.VM_PASSWORD: jq reads it from its environment, not its arguments.
  body=$(jq -n --arg name "$1" --arg region "$2" --arg zone "$3" --arg pkg "$4" --arg image "$5" \
    '{packageConfig: {instanceName: $name, regionCode: $region, zoneCode: $zone,
      packageCode: $pkg, imageResourceUUID: $image, password: env.VM_PASSWORD}}')
  out=$(call POST /instance/create "$body")
  r=$(jq -r '.asyncTaskInfo | "\(.asyncTaskUUID // "") \(.ecsResourceUUID // "")"' <<<"$out")
  [[ $r != " "* && $r != *" " ]] || { echo "lightnode: create answered without a task; fields: $(fields <<<"$out")" >&2; exit 1; }
  echo "$r"
  ;;
wait)
  wait_task "$1"
  ;;
ip)
  for _ in $(seq 1 120); do
    out=$(call GET "/instance/detail?ecsResourceUUID=$1")
    ip=$(jq -r '.instance.publicIpAddress // empty' <<<"$out")
    status=$(jq -r '.instance.ecsStatus // empty' <<<"$out")
    if [ -n "$ip" ] && [[ $status == *START* || $status == *RUN* ]]; then
      echo "$ip"
      exit 0
    fi
    sleep 5
  done
  # Field names only: a value could be the address, and this log is public.
  echo "lightnode: VM $1 has no public address after 10 minutes (status ${status:-unknown}); fields: $(fields <<<"$out")" >&2
  exit 1
  ;;
delete)
  out=$(call POST /instance/release "$(jq -n --arg u "$1" '{ecsResourceUUID: $u}')")
  task=$(jq -r '.asyncTaskInfo.asyncTaskUUID // empty' <<<"$out")
  [ -n "$task" ] || { echo "lightnode: delete of $1 answered without a task; fields: $(fields <<<"$out")" >&2; exit 1; }
  wait_task "$task"
  ;;
sweep)
  prefix=$1 region=$2
  # Delete, then look again: a VM still being created refuses to be deleted,
  # and a deleted one may stay listed for a while with a final status.
  for attempt in $(seq 1 20); do
    left=$(instances "$region" | awk -F'\t' -v p="$prefix" \
      'index($2, p) == 1 && toupper($3) !~ /RELEAS|DESTROY|DELET|TERMINAT/')
    [ -n "$left" ] || exit 0
    while IFS=$'\t' read -r uuid name status; do
      echo "deleting $name ($status)"
      if task=$(call POST /instance/release "$(jq -n --arg u "$uuid" '{ecsResourceUUID: $u}')" |
        jq -r .asyncTaskInfo.asyncTaskUUID); then
        wait_task "$task" || true
      fi
    done <<<"$left"
    [ "$attempt" -lt 20 ] && sleep 15
  done
  echo "lightnode: VMs named $prefix* are still there in $region; delete them in the console" >&2
  exit 1
  ;;
*)
  sed -n '2,15p' "$0" >&2
  exit 2
  ;;
esac
