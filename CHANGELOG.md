# Changelog

All notable changes to sshuttle2. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- The firewall helper, which runs as root, starts `nft` and `resolvectl` from the system directories only, not from the user's `PATH`.
- `--socks5` on an address other than loopback warns that the proxy has no authentication.

### Fixed

- A server could make the client queue PONG, EOF and DNS replies without limit, or read a startup line that never ends.
- Control characters in the server's startup messages reached the terminal unescaped.
- In tun mode, UDP replies from a loopback, broadcast or multicast source reached local apps.
- The SOCKS5 UDP relay took datagrams from any local address, not only from the client that asked for it.
- Programs the client starts no longer inherit the tun device.

### Security

- In tun mode, a local process with root or `CAP_NET_RAW` could crash the client with crafted IPv4 fragments. The routes then came down and traffic went around the tunnel ([GHSA-2c7x-2cp2-g77m](https://github.com/allwinpower/sshuttle2/security/advisories/GHSA-2c7x-2cp2-g77m)).

## [0.3.0] - 2026-10-27

### Added

- `ssh://` destinations, such as `sshuttle2 ssh://user@host:2222`.
- A man page and shell completions for bash, zsh and fish, in every tarball, Linux package and the Homebrew formula.

### Changed

- tun mode is the default on macOS and Linux, so UDP and IPv6 no longer bypass the tunnel. Use `--method pf` or `--method nft` for the previous behaviour.
- The host can be the first argument: `sshuttle2 user@host`. `-r user@host` still works.
- With no networks given, sshuttle2 routes everything (`0/0`, and in tun mode also `::/0` and system DNS) instead of exiting with an error.
- `--dns` with no networks routes everything. The DNS-only mode is removed.
- Linux tun mode sets addresses and routes over netlink and no longer needs iproute2.

### Deprecated

- `--method auto`, which now means tun, and `--udp`, which has no effect. Both are hidden from `--help`.

### Fixed

- tun mode did not start when the route to the SSH server or to an `-x` network went through a link-local IPv6 router such as `fe80::1`.

## [0.2.0] - 2026-10-26

### Added

- macOS servers: the server can be a Mac (Apple Silicon or Intel) with Remote Login turned on. The client picks the server binary by the remote system and CPU, and is about 2.2 MB larger.

## [0.1.2] - 2026-10-24

### Fixed

- tun mode: uploads ran ahead of the tunnel. The TCP stack acknowledged data before the tunnel had sent it, so uploads over slow paths stalled and client memory could grow without bound. The receive window now closes once 256 KiB of a connection is waiting.

## [0.1.1] - 2026-10-23

### Added

- Arch Linux packages (`.pkg.tar.zst`, x86_64 and aarch64) with every final release.

### Fixed

- Several connections to a slow or distant server queued seconds of data in front of each other. A connection's window now shrinks below its initial 256 KiB while it only adds delay, and grows back when the path speeds up.

## [0.1.0] - 2026-10-23

First release.

### Added

- tun mode (macOS, Linux): TCP, UDP, DNS and IPv6.
- pf mode (macOS) and nft mode (Linux): TCP and DNS.
- SOCKS5 proxy with CONNECT and UDP ASSOCIATE, without root.
- A static Linux server (x86_64, arm64), uploaded over SSH on first use.
- Prebuilt clients for macOS and Linux (amd64, arm64) as `.tar.gz`, `.deb` and `.rpm`, and an install script.
- Homebrew: `brew install allwinpower/tap/sshuttle2`.
- The Linux client uses mimalloc instead of musl's allocator: half the CPU and 17–25% more throughput when the CPU is the limit.

[Unreleased]: https://github.com/allwinpower/sshuttle2/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/allwinpower/sshuttle2/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/allwinpower/sshuttle2/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/allwinpower/sshuttle2/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/allwinpower/sshuttle2/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/allwinpower/sshuttle2/releases/tag/v0.1.0
