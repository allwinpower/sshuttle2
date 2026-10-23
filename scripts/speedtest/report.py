#!/usr/bin/env python3
"""Turns run.sh's results.jsonl into the speed test report: one Markdown
table per server, each number the median over the rounds, and the same
numbers as JSON.

  report.py results.jsonl [--layout layout.json] [--md results.md] [--json results.json]

Without --md the Markdown goes to stdout. layout.json (from runner.sh
create) adds regions and packages.
"""
import argparse
import json
import re
import statistics

MODES = [
    ("direct", "No tunnel"),
    ("server", "On the server itself"),
    ("sshuttle", "sshuttle"),
    ("sshuttle-nlc", "sshuttle --no-latency-control"),
    ("sshuttle2-nft", "**sshuttle2** nft (TCP + DNS)"),
    ("sshuttle2-tun", "**sshuttle2** tun (TCP + UDP + DNS)"),
]
OOKLA = ["download_mbps", "upload_mbps", "ping_ms", "jitter_ms", "download_ping_ms", "upload_ping_ms"]
PROC = ["cpu_avg", "cpu_peak", "rss_idle_mb", "rss_peak_mb"]


def median(values):
    values = [v for v in values if isinstance(v, (int, float))]
    return round(statistics.median(values), 2) if values else None


def summarize(runs):
    ok = [r for r in runs if not r["ookla"].get("error")]

    def procs(key):
        got = [r[key] for r in ok if r.get(key) and not r[key].get("error")]
        return {k: median([p.get(k) for p in got]) for k in PROC} if got else None

    return {
        "runs": len(runs),
        "ok": len(ok),
        "errors": [r["ookla"]["error"] for r in runs if r["ookla"].get("error")],
        "ookla": {k: median([r["ookla"].get(k) for r in ok]) for k in OOKLA} if ok else None,
        "server_proc": procs("server_proc"),
        "client_proc": procs("client_proc"),
    }


def num(v, unit=""):
    if v is None:
        return "–"
    text = f"{v:.0f}" if abs(v) >= 10 else f"{v:.1f}"
    return text + unit


def size(info, vm=None):
    """'1 vCPU / 2 GB' as measured on the VM, else as ordered."""
    if info and "cpus" in info:
        gb = info["mem_mb"] / 1024
        return f"{info['cpus']} vCPU / {gb:.0f} GB" if gb >= 1.5 else f"{info['cpus']} vCPU / {info['mem_mb']} MB"
    if vm and vm.get("cpus"):
        return f"{vm['cpus']} vCPU / {vm['mem_gb']} GB"
    return ""


def version(text, name):
    """'sshuttle2 0.1.0' from 'sshuttle2 0.1.0' or '0.1.0'; Ookla's long line cut after its number."""
    m = re.match(r".*?\d+(\.\d+)+(-[0-9A-Za-z.]+)?", text or "")
    text = m.group(0) if m else (text or "?")
    return text if text.lower().startswith(name.lower()) else f"{name} {text}"


def row(label, s):
    if s["ok"] == 0:
        first = s["errors"][0] if s["errors"] else "no result"
        return f"| {label} | failed: {first} |" + " |" * 8
    o, sp, cp = s["ookla"], s["server_proc"], s["client_proc"]
    cells = [
        num(o["download_mbps"], " Mbit/s"),
        num(o["upload_mbps"], " Mbit/s"),
        num(o["ping_ms"], " ms"),
        num(o["download_ping_ms"], " ms"),
        num(o["upload_ping_ms"], " ms"),
        f"{num(sp['cpu_avg'])} % ({num(sp['cpu_peak'])} %)" if sp else "–",
        f"{num(sp['rss_idle_mb'])} → {num(sp['rss_peak_mb'])} MB" if sp else "–",
        f"{num(cp['cpu_avg'])} % ({num(cp['cpu_peak'])} %)" if cp else "–",
        f"{num(cp['rss_idle_mb'])} → {num(cp['rss_peak_mb'])} MB" if cp else "–",
    ]
    if s["ok"] < s["runs"]:
        label += f" ({s['ok']} of {s['runs']} runs)"
    return f"| {label} | " + " | ".join(cells) + " |"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("results")
    ap.add_argument("--layout")
    ap.add_argument("--md")
    ap.add_argument("--json")
    args = ap.parse_args()

    meta, servers, results = {}, {}, {}
    with open(args.results) as f:
        for line in f:
            if not line.strip():
                continue
            rec = json.loads(line)
            if rec["type"] == "meta":
                meta = rec
            elif rec["type"] == "server":
                servers[rec["server"]] = rec
            elif rec["type"] == "result":
                results.setdefault(rec["server"], {}).setdefault(rec["mode"], []).append(rec)
    layout = {}
    if args.layout:
        with open(args.layout) as f:
            layout = json.load(f)
    vms = layout.get("vms") or {}
    client_region = (vms.get("client") or {}).get("region")

    v = meta.get("versions", {})
    commit = layout.get("commit", "")
    md = ["## Speed test: sshuttle and sshuttle2", ""]
    md.append(
        version(v.get("sshuttle2"), "sshuttle2") + (f" (`{commit[:7]}`)" if commit else "")
        + f", {version(v.get('sshuttle'), 'sshuttle')}, {version(v.get('ookla'), 'Speedtest by Ookla')}."
    )
    client = meta.get("client") or {}
    md.append(
        f"Started {meta.get('started', '?')}; {meta.get('rounds', '?')} rounds per server; every number is the median."
        + (f" [Workflow run]({layout['run_url']})." if layout.get("run_url", "").startswith("http") else "")
    )
    md.append("")
    md.append(
        f"Client: {size(client, vms.get('client'))}" + (f" in {client_region}" if client_region else "")
        + (f", {client['os']}" if client.get("os") else "") + "."
    )

    report = {"meta": meta, "layout": layout, "servers": []}
    for name in list(servers) + [s for s in results if s not in servers]:
        info = servers.get(name, {})
        vm = vms.get(name) or {}
        where = ""
        if vm.get("region"):
            same = vm["region"] == client_region
            where = f", {'same region as the client' if same else 'another region'} ({vm['region']})"
        title = ": ".join(t for t in (name, size(info, vm)) if t)
        md += ["", f"### {title}{where}", ""]
        if info.get("error"):
            md.append(f"Skipped: {info['error']}.")
            report["servers"].append({"server": name, "info": info, "vm": vm, "modes": {}})
            continue
        o = info.get("ookla_server") or {}
        if o:
            md.append(f"Ookla server {o.get('id')}: {o.get('name')}, {o.get('location')}, {o.get('country')}.")
        if vm.get("package"):
            md.append(f"LightNode package {vm['package']} (bandwidth {vm.get('bandwidth', '?')}).")
        md += [
            "",
            "| | Download | Upload | Ping | Ping, downloading | Ping, uploading"
            " | Server CPU | Server RAM | Client CPU | Client RAM |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
        modes = {}
        for mode, label in MODES:
            runs = results.get(name, {}).get(mode)
            if runs:
                modes[mode] = summarize(runs)
                md.append(row(label, modes[mode]))
        report["servers"].append({"server": name, "info": info, "vm": vm, "modes": modes})

    md += [
        "",
        "Ping: Ookla's idle latency, then the latency while it downloads and while it uploads (interquartile mean).",
        "CPU: average while Ookla ran, in % of one core; in parentheses the busiest second."
        " RAM: resident memory, idle → peak.",
        "Server: sshd plus the tunnel's server process. Client: the tunnel client, its helpers and its ssh.",
    ]
    text = "\n".join(md) + "\n"
    if args.md:
        with open(args.md, "w") as f:
            f.write(text)
    else:
        print(text, end="")
    if args.json:
        with open(args.json, "w") as f:
            json.dump(report, f, indent=2)
            f.write("\n")


if __name__ == "__main__":
    main()
