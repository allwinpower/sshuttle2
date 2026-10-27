# sshuttle2

[![CI](https://github.com/allwinpower/sshuttle2/actions/workflows/ci.yml/badge.svg)](https://github.com/allwinpower/sshuttle2/actions/workflows/ci.yml)
[![License: LGPL-2.1-or-later](https://img.shields.io/badge/license-LGPL--2.1--or--later-blue?style=flat-square)](LICENSE)
[![OpenRepos](https://img.shields.io/endpoint?url=https://openrepos.org/status/sshuttle2.openrepos.org.json&style=flat-square)](https://sshuttle2.openrepos.org/)

A VPN over a plain SSH login. Written in Rust, inspired by [sshuttle](https://github.com/sshuttle/sshuttle).

If you can log in to a server with SSH, sshuttle2 sends your traffic through it: TCP, all UDP, DNS and IPv6. You install nothing on the server and need no root there. On first run the client uploads a small server program over SSH. The server can be Linux or macOS.

**Why it exists:** a hospital Wi-Fi blocked AI tools, every VPN and Wikipedia. Only SSH got through, and sshuttle was too slow to work with. The story: **[allwinpower.github.io/sshuttle2](https://allwinpower.github.io/sshuttle2/)** (also in [STORY.md](STORY.md)).

## Benchmarks

Measured with Ookla's speedtest on rented servers: a client in Frankfurt, and servers in Frankfurt (3 ms away) and Washington (88 ms away). Every port there tops out at about 100 Mbit/s. Every mode, both server sizes and all numbers: [docs/speed-results.md](docs/speed-results.md).

### Near: client and server in Frankfurt

sshuttle2 0.1.0-rc.1, 23 October 2026, a 1 vCPU / 2 GB server. Medians of 3 rounds. A later single round, with the code of 0.1.1, got the same speeds.

| | Download | Upload | Ping, downloading | Server CPU | Server RAM |
|---|---:|---:|---:|---:|---:|
| sshuttle | 55 Mbit/s | 58 Mbit/s | **10 ms** | 61 % | 30 MB |
| **sshuttle2**, nft mode (TCP + DNS) | **97 Mbit/s** | **97 Mbit/s** | 47 ms | **26 %** | **13 MB** |
| **sshuttle2**, tun mode (TCP + UDP + DNS) | 96 Mbit/s | 95 Mbit/s | 38 ms | **26 %** | 14 MB |

*Server CPU: average while Ookla ran, in % of one core. Server RAM: peak.*

- sshuttle2 fills the port in both directions, with half of sshuttle's server CPU and memory.
- The price is delay while the link is full: 38–47 ms against sshuttle's 10 ms.

### Far: client in Frankfurt, server in Washington

sshuttle2 with the code of 0.1.1, 23 October 2026. One round on each server (1 vCPU / 2 GB and 2 vCPU / 4 GB), so single samples; the ranges span the two. Without a tunnel, this path does about 100 Mbit/s both ways.

| | Download | Upload | Ping, downloading |
|---|---:|---:|---:|
| sshuttle | 2.4 Mbit/s | 2.4–2.5 Mbit/s | **182–183 ms** |
| **sshuttle2**, nft mode (TCP + DNS) | **95–98 Mbit/s** | **39–79 Mbit/s** | 272–295 ms |
| **sshuttle2**, tun mode (TCP + UDP + DNS) | **97 Mbit/s** | failed before 0.1.2; fixed in 0.1.2 | 233–252 ms |

- sshuttle2 downloads about **40 times faster** than sshuttle here.
- Before 0.1.1, sshuttle2 stalled on this path too: about 2 Mbit/s, and Ookla gave up in every run. The window brake fixed that.
- tun mode failed halfway through the upload on both servers. 0.1.2 fixes that, and a 3-round rerun is on the [TODO](TODO.md) list.

Run it again: the [Speed test](docs/SPEEDTEST.md) workflow (the owner starts it).

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
| tun (the default) | macOS, Linux | TCP, all UDP, DNS, IPv6 | live on a Mac; Linux containers, in CI |
| pf (`--method pf`) | macOS | TCP, DNS | live, on a Mac |
| nft (`--method nft`) | Linux | TCP, DNS | Linux containers, in CI |
| SOCKS5 | macOS, Linux | TCP (CONNECT), UDP (ASSOCIATE) | in CI |

Every mode works with either server:

| Server | CPUs | Tested |
|---|---|---|
| Linux | x86_64, arm64 | in CI; rented servers |
| macOS | Apple Silicon, Intel | Linux client in every mode, on a Mac (`scripts/mac-server-check.sh`) |

A Mac server needs Remote Login on (System Settings → General → Sharing).

## Install

**macOS or Linux, with [Homebrew](https://brew.sh):**

```bash
brew install allwinpower/tap/sshuttle2
```

**Any Linux or macOS, with the install script.** It downloads the latest release, checks it against the release's `SHA256SUMS`, and puts `sshuttle2` in `/usr/local/bin`. For `~/.local/bin` instead, end the command with `sh -s -- --user`.

```bash
curl -fsSL https://github.com/allwinpower/sshuttle2/releases/latest/download/install.sh | sh
```

**Debian, Ubuntu, Fedora, RHEL and compatible:** download the `.deb` or `.rpm` for your CPU from the [latest release](https://github.com/allwinpower/sshuttle2/releases/latest), then install it:

```bash
sudo apt install ./sshuttle2_*_amd64.deb
```

```bash
sudo dnf install ./sshuttle2-*.x86_64.rpm
```

**Arch Linux:** download the `.pkg.tar.zst` for your CPU (`x86_64` or `aarch64`) from the [latest release](https://github.com/allwinpower/sshuttle2/releases/latest), then:

```bash
sudo pacman -U ./sshuttle2-*-x86_64.pkg.tar.zst
```

These packages do not update themselves: install the next release's file the same way. With Homebrew, `brew upgrade` does it. An AUR package (`sshuttle2-bin`) will follow.

**By hand:** every [release](https://github.com/allwinpower/sshuttle2/releases) has a `.tar.gz` for each system (`macos` or `linux`, `amd64` or `arm64`) and a `SHA256SUMS` file. The Linux binaries are static and run on any distribution. The macOS binaries are not notarized, so after a browser download, run `xattr -d com.apple.quarantine sshuttle2` once.

**From source:** see [Build](#build).

## Use

Send everything through `host`: TCP, all UDP (QUIC/HTTP3, video calls, games), DNS and IPv6.

```bash
sshuttle2 user@host
```

`sshuttle2 -r user@host` and `sshuttle2 ssh://user@host:2222` do the same. It asks for your password, because the virtual network device (tun) and the routes need root. They are removed when it exits, even if it crashes.

If the server has no IPv6, IPv6 connections are refused at once, so apps fall back to IPv4 without leaking. DNS queries from any app go to the server's resolver. On Linux, tun mode needs the `ip` command (iproute2), and the system resolver is pointed at the tunnel with `resolvectl` when systemd-resolved is in use; otherwise a warning says that system DNS is not redirected.

Route only some networks by naming them after the host:

```bash
sshuttle2 user@host 10.0.0.0/8 192.168.1.0/24
```

Keep some networks out with `-x`, repeatable:

```bash
sshuttle2 user@host -x 192.168.0.0/16 -x 10.0.0.0/8
```

The ssh server's own address is always kept out. If a session ever leaves rules behind, remove them:

```bash
sshuttle2 cleanup
```

**TCP-only firewall modes (`--method pf|nft`).** These capture traffic with the firewall instead of a tun device: pf on macOS, nftables on Linux. They carry TCP and DNS only: other UDP goes around the tunnel, and so does IPv6 unless you add `::/0`. Use them where tun does not work.

```bash
sshuttle2 user@host --method pf --dns     # macOS
sshuttle2 user@host --method nft --dns    # Linux
```

nft mode needs the `nft` command. With `--dns`, a local stub resolver such as systemd-resolved's `127.0.0.53` is redirected too.

**Proxy only, no root.** Run a SOCKS5 proxy instead:

```bash
sshuttle2 user@host --socks5 127.0.0.1:1080
```

Point apps at it. Names are resolved on the server:

```bash
curl --socks5-hostname 127.0.0.1:1080 https://ifconfig.me
```

**First run.** The client copies a small server binary (about 1.1–1.4 MB) for the server's system and CPU to `~/.cache/sshuttle2/` on the server. Later runs reuse it until the version changes.

The client runs your normal `ssh`, so `~/.ssh/config`, keys, the agent and ProxyJump all work.

| Option | Meaning |
|---|---|
| `HOST` or `-r HOST` | `[user@]host[:port]`, `ssh://[user@]host[:port]` or an ssh config alias. Without `-r`, the host is the first argument. `local` runs the server as a local process instead (for testing). |
| `SUBNETS` | Networks to route, after the host: `10.0.0.0/8`, `10/8`, `0/0`, a host name. None: everything. |
| `-x NET` | Keep a network out of the tunnel. Repeatable. |
| `--dns` | Send DNS queries to your system's nameservers through the tunnel. |
| `--method M` | `tun` (the default: TCP, all UDP, DNS, IPv6), or `pf` (macOS) or `nft` (Linux) for TCP and DNS only. |
| `--udp` | Does nothing: tun is the default. Kept so old command lines work. |
| `--socks5 ADDR` | Run a SOCKS5 proxy instead of capturing traffic. Supports CONNECT and UDP ASSOCIATE. |
| `-e CMD` | ssh command and extra options, e.g. `-e "ssh -i ~/.ssh/work"`. |
| `--to-ns IP[:PORT]` | DNS server the remote side should use. |
| `-v` | Throughput and RTT every 5 s; `-vv` and `-vvv` for debugging. |

## How it works

- The client starts your `ssh` and runs `sshuttle2-server` on the server. That one SSH session carries every connection.
- Each connection gets its own lane with its own flow control, so one big download doesn't block the rest. Each lane's window grows while it limits the speed and shrinks when it only builds a queue, so a slow or distant server does not leave seconds of data queued in front of every other connection.
- tun mode, the default, uses a virtual network device and a user-space TCP/IP stack ([ipstack](https://github.com/narrowlink/ipstack), vendored with a patch in [third_party/ipstack](third_party/ipstack/PATCHES.md)). In pf mode (macOS) or nft mode (Linux) the firewall redirects TCP and DNS to the client.
- No Python anywhere: both ends are compiled Rust. The servers (static Linux, and macOS) are embedded in the client.
- Wire format: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Build

You need Rust (rustup installs the pinned toolchain) and, for release builds, zig and cargo-zigbuild:

```bash
brew install zig cargo-zigbuild
```

On Linux, install zig from your package manager or [ziglang.org](https://ziglang.org/download/), then `cargo install cargo-zigbuild`.

Get the source, then build the servers (Linux and macOS, all with zig) and a client that embeds them:

```bash
git clone https://github.com/allwinpower/sshuttle2
```

```bash
cd sshuttle2 && cargo xtask dist
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

`scripts/mac-server-check.sh` runs on a Mac with Docker: Linux clients in containers use the Mac as their server, in every mode. It starts its own sshd on 127.0.0.1 and needs no sudo.

`scripts/bench.sh HOST` measures a real server in SOCKS5 mode. `scripts/mac-speed-check.sh HOST...` compares direct, sshuttle2 and sshuttle on a Mac.

## Contributing

Pull requests are welcome: bug fixes, speed, new platforms. Before you open one, run:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace
```

CI runs these plus the end-to-end tests in Linux containers. CI checks that macOS code compiles but cannot run it; say in the pull request what you tested on a Mac. Bugs and ideas go in [issues](https://github.com/allwinpower/sshuttle2/issues).

## License

LGPL-2.1-or-later, the same as sshuttle, from which the firewall logic is ported. Thanks to the sshuttle authors. The vendored ipstack in `third_party/ipstack` keeps its Apache-2.0 license.
