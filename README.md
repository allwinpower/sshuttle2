# sshuttle2

[![CI](https://github.com/allwinpower/sshuttle2/actions/workflows/ci.yml/badge.svg)](https://github.com/allwinpower/sshuttle2/actions/workflows/ci.yml)
[![License: LGPL-2.1-or-later](https://img.shields.io/badge/license-LGPL--2.1--or--later-blue?style=flat-square)](LICENSE)
[![OpenRepos](https://img.shields.io/endpoint?url=https://openrepos.org/status/sshuttle2.openrepos.org.json&style=flat-square)](https://sshuttle2.openrepos.org/)

A VPN over a plain SSH login. Written in Rust, inspired by [sshuttle](https://github.com/sshuttle/sshuttle).

If you can log in to a server with SSH, sshuttle2 sends your traffic through it: TCP and DNS, and in tun mode all UDP too. You install nothing on the server and need no root there. On first run the client uploads a small server program over SSH.

**Why it exists:** a hospital Wi-Fi blocked AI tools, every VPN and Wikipedia. Only SSH got through, and sshuttle was too slow to work with. The story: **[sshuttle2.openrepos.org](https://sshuttle2.openrepos.org/)** (also in [STORY.md](STORY.md)).

## Benchmarks

Measured on a Mac on the hospital Wi-Fi from the story, 23 October 2026, through a cloud server with 2 vCPU and 4 GB.

| | Download | Page wait, idle | Page wait, during a download |
|---|---:|---:|---:|
| No tunnel (blocked sites stay blocked) | 91.9 Mbit/s | 128 ms | 65 ms |
| sshuttle | 12.8 Mbit/s | 1817 ms | 2281 ms |
| **sshuttle2**, pf mode (TCP + DNS) | **19.1 Mbit/s** | **131 ms** | 338 ms |
| **sshuttle2**, tun mode (TCP + UDP + DNS) | 17.4 Mbit/s | 211 ms | **223 ms** |

*Download: one 100 MB file. Page wait: time until `google.com/generate_204` sends its first byte.*

- Pages answered about **14 times faster** than with sshuttle, and about 10 times faster while a download ran.
- Downloads were about **1.5 times faster**. The bare Wi-Fi did 91.9 Mbit/s, so there is room left.

These are single samples on a shared Wi-Fi. A second run that evening, with the Wi-Fi under heavy load, put every SSH tunnel, sshuttle included, at 13–20 Mbit/s and about 200 ms. Plain SSH itself got only about 30 Mbit/s then. A repeat with medians, on a quiet network and two server sizes, is on the [TODO](TODO.md) list.

Reproduce on a Mac: `scripts/mac-speed-check.sh [user@]host...`. Measure a server in SOCKS5 mode, against OpenSSH's own `ssh -D`: `scripts/bench.sh [user@]host`.

Fresh numbers on rented servers, with server and client CPU and memory: the [Speed test](docs/SPEEDTEST.md) workflow (the owner starts it).

### Linux client and mimalloc

The Linux client is one static binary, built with musl, so it runs on every distribution. But musl's memory allocator is slow, and the tunnel allocates memory for the data it moves. So the Linux client uses [mimalloc](https://github.com/microsoft/mimalloc) instead. The macOS client keeps the system allocator.

Measured on a cloud server with 1 vCPU and 2 GB, 23 October 2026. All three clients were built from the same source. Client and server shared that one core, with `-r local` (no SSH, no network), so the CPU was the limit. That shows what each client build costs. Medians of 3 runs, each downloading a 300 MB file once on its own and then 4 times at once.

| Linux client | 1 download | 4 at once | Client CPU for 1.5 GB |
|---|---:|---:|---:|
| musl, its own allocator | 1514 Mbit/s | 1511 Mbit/s | 2.6 s |
| **musl + mimalloc** | **1900 Mbit/s** | **1773 Mbit/s** | **1.3 s** |
| glibc, for comparison | 1712 Mbit/s | 1621 Mbit/s | 1.6 s |

- mimalloc **halves the client's CPU** and moves 17–25% more data than musl's allocator. It also beats a glibc build.
- At everyday speeds the difference is small. At 100 Mbit/s, musl's allocator costs about 2.2% of one core and mimalloc about 1.1%. It matters on fast links and small CPUs.
- The cost is 128 KiB of binary size (5.7 MB in all).

Reproduce on Linux, with Linux builds of the server and of each client: `scripts/alloc-bench.sh SERVER_BIN CLIENT_BIN...`.

## Status

| Mode | Platform | Carries | Tested |
|---|---|---|---|
| pf | macOS | TCP, DNS | live, on a Mac |
| nft | Linux | TCP, DNS | Linux containers, in CI |
| tun | macOS, Linux | TCP, all UDP, DNS, IPv6 | live on a Mac; Linux containers, in CI |
| SOCKS5 | macOS, Linux | TCP (CONNECT), UDP (ASSOCIATE) | in CI |

There are no prebuilt binaries yet. Build from source (below).

## Quick start

```bash
git clone https://github.com/allwinpower/sshuttle2
```

```bash
cd sshuttle2 && cargo xtask dist
```

```bash
target/release/sshuttle2 -r user@host 0/0 --dns
```

`cargo xtask dist` needs Rust ([rustup](https://rustup.rs) installs the pinned toolchain), zig and cargo-zigbuild. See [Build](#build).

## Use

Send everything through `host`: all IPv4 TCP, plus DNS lookups.

```bash
sshuttle2 -r user@host 0/0 --dns
```

It asks for your password, because the firewall rules need root. The rules are removed when it exits, even if it crashes.

On macOS this uses pf. On Linux the same command uses nftables, so the `nft` command must be installed; with `--dns`, a local stub resolver such as systemd-resolved's `127.0.0.53` is redirected too.

Tunnel UDP too (QUIC/HTTP3, video calls, games). This uses a virtual network device (tun) instead of the firewall:

```bash
sshuttle2 -r user@host 0/0 --udp
```

In tun mode, `0/0` also covers IPv6. If the server has no IPv6, IPv6 connections are refused at once, so apps fall back to IPv4 without leaking. DNS queries from any app go to the server's resolver. On Linux, the system resolver is pointed at the tunnel with `resolvectl` when systemd-resolved is in use; otherwise a warning says that system DNS is not redirected.

Keep some networks out with `-x`, repeatable:

```bash
sshuttle2 -r user@host 0/0 -x 192.168.0.0/16 -x 10.0.0.0/8
```

The ssh server's own address is always kept out. If a session ever leaves rules behind, remove them:

```bash
sshuttle2 cleanup
```

**Proxy only, no root.** Run a SOCKS5 proxy instead:

```bash
sshuttle2 -r user@host --socks5 127.0.0.1:1080
```

Point apps at it. Names are resolved on the server:

```bash
curl --socks5-hostname 127.0.0.1:1080 https://ifconfig.me
```

**First run.** The client copies a small static server binary (about 1.3 MB) to `~/.cache/sshuttle2/` on the server. Later runs reuse it until the version changes.

The client runs your normal `ssh`, so `~/.ssh/config`, keys, the agent and ProxyJump all work.

| Option | Meaning |
|---|---|
| `-r HOST` | `[user@]host[:port]` or an ssh config alias. `local` runs the server as a local process instead (for testing). |
| `SUBNETS` | Networks to route: `0/0`, `10.0.0.0/8`, `10/8`, a host name. |
| `-x NET` | Keep a network out of the tunnel. Repeatable. |
| `--dns` | Send DNS queries to your system's nameservers through the tunnel. |
| `--udp` | tun mode: TCP, UDP and DNS. Same as `--method tun`. |
| `--method M` | `pf` (macOS, TCP and DNS), `nft` (Linux, TCP and DNS), `tun`, or `auto` (pf or nft unless `--udp`). |
| `--socks5 ADDR` | Run a SOCKS5 proxy instead of capturing traffic. Supports CONNECT and UDP ASSOCIATE. |
| `-e CMD` | ssh command and extra options, e.g. `-e "ssh -i ~/.ssh/work"`. |
| `--to-ns IP[:PORT]` | DNS server the remote side should use. |
| `-v` | Throughput and RTT every 5 s; `-vv` and `-vvv` for debugging. |

## How it works

- The client starts your `ssh` and runs `sshuttle2-server` on the server. That one SSH session carries every connection.
- Each connection gets its own lane with its own flow control, so one big download doesn't block the rest. Each lane's window grows while it limits the speed and shrinks when it only builds a queue.
- pf (macOS) or nftables (Linux) redirects TCP and DNS to the client. tun mode uses a virtual network device and a user-space TCP/IP stack ([ipstack](https://github.com/narrowlink/ipstack), vendored with a patch in [third_party/ipstack](third_party/ipstack/PATCHES.md)).
- No Python anywhere: both ends are compiled Rust. The server is a static Linux binary, embedded in the client.
- Wire format: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Build

You need Rust (rustup installs the pinned toolchain) and, for release builds, zig and cargo-zigbuild:

```bash
brew install zig cargo-zigbuild
```

On Linux, install zig from your package manager or [ziglang.org](https://ziglang.org/download/), then `cargo install cargo-zigbuild`.

Build the Linux servers, then a client that embeds them:

```bash
cargo xtask dist
```

The result is `target/release/sshuttle2`. A plain `cargo build` works too, but that client embeds no servers and needs `--server-bin PATH`.

## Test

```bash
cargo test --workspace
```

```bash
scripts/e2e.sh local
```

```bash
scripts/e2e.sh docker
```

`scripts/e2e-tun.sh` and `scripts/e2e-nft.sh` test tun and nft mode in Linux containers. They need a static Linux client; each script's header says how to build it.

`scripts/bench.sh HOST` measures a real server in SOCKS5 mode. `scripts/mac-speed-check.sh HOST...` compares direct, sshuttle2 and sshuttle on a Mac.

## Contributing

Pull requests are welcome: bug fixes, speed, new platforms. Before you open one, run:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace
```

CI runs these plus the end-to-end tests in Linux containers. CI checks that macOS code compiles but cannot run it; say in the pull request what you tested on a Mac. Bugs and ideas go in [issues](https://github.com/allwinpower/sshuttle2/issues).

## License

LGPL-2.1-or-later, the same as sshuttle, from which the firewall logic is ported. Thanks to the sshuttle authors. The vendored ipstack in `third_party/ipstack` keeps its Apache-2.0 license.
