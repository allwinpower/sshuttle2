# sshuttle2

[![CI](https://github.com/allwinpower/sshuttle2/actions/workflows/ci.yml/badge.svg)](https://github.com/allwinpower/sshuttle2/actions/workflows/ci.yml)
[![License: LGPL-2.1-or-later](https://img.shields.io/badge/license-LGPL--2.1--or--later-blue?style=flat-square)](LICENSE)
[![OpenRepos](https://img.shields.io/endpoint?url=https://openrepos.org/status/sshuttle2.openrepos.org.json&style=flat-square)](https://sshuttle2.openrepos.org/)

A VPN over a plain SSH login. If you can `ssh` to a server, sshuttle2 sends your traffic through it: TCP, UDP, DNS and IPv6. Nothing is installed on the server, and you need no root there.

sshuttle2 is a Rust implementation of the idea behind [sshuttle](https://github.com/sshuttle/sshuttle). It is a separate project, not affiliated with sshuttle. It was written to work from a network that blocked everything except SSH: [the story](https://allwinpower.github.io/sshuttle2/).

| | sshuttle2 | sshuttle | `ssh -D` | WireGuard |
|---|---|---|---|---|
| Server needs | an SSH login | an SSH login and Python | an SSH login | root, WireGuard and an open UDP port |
| Works where only SSH gets through | yes | yes | yes | no |
| Captures every app | yes | yes | no, apps must use SOCKS | yes |
| UDP | yes | Linux, tproxy method only | no | yes |
| Root on the client | yes, or none as a SOCKS5 proxy | yes | no | yes |

## Install

macOS or Linux, with [Homebrew](https://brew.sh):

```bash
brew install allwinpower/tap/sshuttle2
```

Or with the install script, which downloads the latest release, checks it against `SHA256SUMS` and installs to `/usr/local/bin` (add `sh -s -- --user` for `~/.local/bin`):

```bash
curl -fsSL https://github.com/allwinpower/sshuttle2/releases/latest/download/install.sh | sh
```

Packages for each release are on the [releases page](https://github.com/allwinpower/sshuttle2/releases/latest):

- Debian, Ubuntu: `sudo apt install ./sshuttle2_*_amd64.deb`
- Fedora, RHEL: `sudo dnf install ./sshuttle2-*.x86_64.rpm`
- Arch Linux: `sudo pacman -U ./sshuttle2-*-x86_64.pkg.tar.zst`
- Tarballs for macOS and Linux, amd64 and arm64. The Linux binaries are static. The macOS binaries are not notarized; after a browser download, run `xattr -d com.apple.quarantine sshuttle2`.

Only Homebrew updates itself. With the other packages, install the next release the same way.

## Usage

```bash
sshuttle2 user@host                             # everything: TCP, UDP, DNS, IPv6
sshuttle2 user@host 10.0.0.0/8 192.168.1.0/24   # only these networks
sshuttle2 user@host -x 192.168.0.0/16           # everything except this network
sshuttle2 user@host --socks5 127.0.0.1:1080     # a SOCKS5 proxy, no root needed
```

The host can also be `ssh://user@host:2222`, an alias from `~/.ssh/config`, or given with `-r`. sshuttle2 runs your own `ssh`, so your config, keys, agent and `ProxyJump` work as usual.

Capturing traffic needs root for the tun device and routes, so sshuttle2 asks for your password. Everything it sets up is removed when it exits, also after a crash. `sshuttle2 cleanup` removes anything left behind.

On first run, the client uploads a small server binary (about 1.3 MB) to `~/.cache/sshuttle2/` on the server and reuses it until the version changes. The server can be Linux (x86_64, arm64) or macOS (Apple Silicon, Intel; with Remote Login turned on).

DNS queries from every app go to the server's resolver. On Linux this needs systemd-resolved; without it, sshuttle2 warns that system DNS is not redirected. If the server has no IPv6, IPv6 connections are refused at once, so apps fall back to IPv4 instead of leaking.

### Modes

| Mode | Client | Carries | Root |
|---|---|---|---|
| tun (default) | macOS, Linux | TCP, UDP, DNS, IPv6 | yes |
| `--method pf` | macOS | TCP; DNS with `--dns` | yes |
| `--method nft` | Linux, needs `nft` | TCP; DNS with `--dns` | yes |
| `--socks5 ADDR` | macOS, Linux | TCP (CONNECT), UDP (ASSOCIATE) | no |

The pf and nft modes capture traffic with the firewall instead of a tun device. Other UDP traffic, and IPv6 unless you add `::/0`, goes around the tunnel. Use them where tun mode does not work. With SOCKS5, use `curl --socks5-hostname` or similar so names are resolved on the server.

### Options

| Option | Meaning |
|---|---|
| `HOST`, `-r HOST` | `[user@]host[:port]`, `ssh://[user@]host[:port]` or an ssh config alias. `local` runs the server as a local process, for testing. |
| `SUBNETS` | Networks to route, after the host: `10.0.0.0/8`, `10/8`, `0/0` or a host name. Default: everything. |
| `-x NET` | Keep a network out of the tunnel. Repeatable. The SSH server's own address is always excluded. |
| `--dns` | Send DNS queries to your system's nameservers through the tunnel. |
| `--method M` | `tun` (default), `pf` (macOS) or `nft` (Linux). |
| `--socks5 ADDR` | Run a SOCKS5 proxy instead of capturing traffic. |
| `-e CMD` | The ssh command and extra options, such as `-e "ssh -i ~/.ssh/work"`. |
| `--to-ns IP[:PORT]` | The DNS server the remote side uses. |
| `-v` | Throughput and RTT every 5 seconds. `-vv` and `-vvv` for debugging. |

## Performance

Ookla's speedtest, client and a 1 vCPU / 2 GB server both in Frankfurt, on ports limited to about 100 Mbit/s. Medians of 3 rounds.

| | Download | Upload | Ping while downloading | Server CPU | Server RAM |
|---|---:|---:|---:|---:|---:|
| sshuttle 2.0.0 | 55 Mbit/s | 58 Mbit/s | 10 ms | 61 % | 30 MB |
| sshuttle2, tun mode | 96 Mbit/s | 95 Mbit/s | 38 ms | 26 % | 14 MB |

With the server 88 ms away, sshuttle2 downloads at 95–98 Mbit/s and sshuttle at 2.4 Mbit/s. The far path, the other modes and the method: [docs/benchmarks.md](docs/benchmarks.md).

## How it works

- The client starts `ssh` and runs `sshuttle2-server` on the server. One SSH session carries every connection.
- Each connection is a separate channel with its own flow control, so a large download does not hold up other connections. A channel's window grows while it limits throughput and shrinks when it only adds queueing delay.
- tun mode uses a virtual network device and a user-space TCP/IP stack ([ipstack](https://github.com/narrowlink/ipstack), vendored with [patches](third_party/ipstack/PATCHES.md)). pf and nft modes redirect TCP and DNS with the firewall.
- Both ends are Rust. The server binaries (static Linux, and macOS) are embedded in the client.
- Wire format: [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Building from source

You need Rust (rustup installs the pinned toolchain), [zig](https://ziglang.org/download/) and cargo-zigbuild, which build the embedded servers for every target.

```bash
git clone https://github.com/allwinpower/sshuttle2
cd sshuttle2
cargo xtask dist
```

The result is `target/release/sshuttle2`. A plain `cargo build` works too, but that client embeds no servers and needs `--server-bin PATH`.

On macOS, `brew install zig cargo-zigbuild`. On Linux, install zig from your package manager, then `cargo install cargo-zigbuild`.

## Contributing

Bug reports, fixes and new platforms are welcome. Before opening a pull request, run:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace
```

CI also runs end-to-end tests in Linux containers (`scripts/e2e.sh`, `scripts/e2e-tun.sh`, `scripts/e2e-nft.sh`). It cannot run macOS code, so say in the pull request what you tested on a Mac. Issues labelled [good first issue](https://github.com/allwinpower/sshuttle2/labels/good%20first%20issue) are a good place to start.

## License

LGPL-2.1-or-later, like sshuttle, whose firewall rules sshuttle2 ports. Thanks to the sshuttle authors for the original design. The vendored ipstack in `third_party/ipstack` keeps its Apache-2.0 license.
