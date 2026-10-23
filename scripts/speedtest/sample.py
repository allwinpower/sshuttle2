#!/usr/bin/env python3
"""CPU and memory of a tunnel's processes, sampled from /proc every 0.5 s
until a stop file appears (see run.sh).

  sample.py --pid PID --stop FILE
      the process and all its descendants (the client side: the tunnel
      client, its helpers and its ssh)
  sample.py --match REGEX --stop FILE
      processes whose command line matches, their descendants, and their
      sshd ancestors except the listener (the server side: sshd does the
      encryption, the tunnel server the rest)

Prints one JSON line: cpu_avg and cpu_peak (peak over 1 s) in % of one
core, rss_idle_mb (the first sample) and rss_peak_mb (the largest), each
summed over the processes. Linux only.
"""
import argparse
import json
import os
import re
import time

HZ = os.sysconf("SC_CLK_TCK")
PAGE_KB = os.sysconf("SC_PAGE_SIZE") // 1024
INTERVAL = 0.5


def processes():
    """pid -> (ppid, cpu ticks, rss kB, command line), for every process."""
    out = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat") as f:
                stat = f.read()
            with open(f"/proc/{name}/cmdline", "rb") as f:
                cmd = f.read().replace(b"\0", b" ").decode(errors="replace").strip()
        except OSError:
            continue
        # The command name in parentheses may hold spaces; fields after it
        # start with the state: ppid is [1], utime and stime [11] and [12],
        # rss (pages) [21].
        rest = stat[stat.rindex(")") + 2:].split()
        out[int(name)] = (int(rest[1]), int(rest[11]) + int(rest[12]), int(rest[21]) * PAGE_KB, cmd)
    return out


def ancestors(ps, pid):
    seen = []
    while pid in ps and pid > 1:
        pid = ps[pid][0]
        seen.append(pid)
    return seen


def select(ps, args, me):
    children = {}
    for pid, (ppid, _, _, _) in ps.items():
        children.setdefault(ppid, []).append(pid)
    if args.pid:
        roots = {args.pid} & ps.keys()
    else:
        rx = re.compile(args.match)
        roots = {p for p, v in ps.items() if rx.search(v[3]) and p not in me}
    chosen = set()
    todo = list(roots)
    while todo:
        p = todo.pop()
        if p not in chosen:
            chosen.add(p)
            todo.extend(children.get(p, []))
    if args.match:
        for r in roots:
            for a in ancestors(ps, r):
                cmd = ps.get(a, (0, 0, 0, ""))[3]
                if "sshd" in cmd and "[listener]" not in cmd:
                    chosen.add(a)
    return chosen


def main():
    ap = argparse.ArgumentParser()
    who = ap.add_mutually_exclusive_group(required=True)
    who.add_argument("--pid", type=int)
    who.add_argument("--match")
    ap.add_argument("--stop", required=True)
    ap.add_argument("--max-seconds", type=float, default=600)
    args = ap.parse_args()

    ps = processes()
    me = {os.getpid(), *ancestors(ps, os.getpid())}
    start = time.monotonic()
    prev = None  # pid -> ticks at the previous sample
    times, deltas, rss = [], [], []
    most = 0
    while True:
        ps = processes()
        chosen = select(ps, args, me)
        now = time.monotonic()
        ticks = {p: ps[p][1] for p in chosen}
        if chosen:
            if prev is not None:
                # A process new since the last sample counts from zero.
                deltas.append(sum(max(0, t - prev.get(p, 0)) for p, t in ticks.items()))
            times.append(now)
            prev = ticks
            rss.append(sum(ps[p][2] for p in chosen))
            most = max(most, len(chosen))
        if (os.path.exists(args.stop) and len(times) >= 2) or now - start > args.max_seconds:
            break
        time.sleep(INTERVAL)

    if len(times) < 2:
        print(json.dumps({"error": "no tunnel process found"}))
        return
    span = times[-1] - times[0]
    peak = 0.0
    for i in range(1, len(deltas)):
        window = times[i + 1] - times[i - 1]
        if window > 0:
            peak = max(peak, (deltas[i - 1] + deltas[i]) / HZ / window)
    if len(deltas) == 1:
        peak = deltas[0] / HZ / span
    print(json.dumps({
        "cpu_avg": round(100 * sum(deltas) / HZ / span, 1),
        "cpu_peak": round(100 * peak, 1),
        "rss_idle_mb": round(rss[0] / 1024, 1),
        "rss_peak_mb": round(max(rss) / 1024, 1),
        "processes": most,
        "seconds": round(span, 1),
    }))


if __name__ == "__main__":
    main()
