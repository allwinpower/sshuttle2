# Changelog

What changed in each release of sshuttle2. A release's section here becomes its GitHub Release notes ([docs/RELEASING.md](docs/RELEASING.md)).

## [Unreleased]

- **Slow or distant servers no longer stall the tunnel.** Each connection kept at least 256 KiB in flight, so when the path was much slower than that (as across an ocean, where Ookla's speedtest stalled in every run), several downloads together queued seconds of data in front of every other connection. A connection's window now also shrinks below its starting size while it only builds a queue, and grows back when the path gets faster.

## [0.1.0] - 2026-10-23

The first release.

- **pf mode** (macOS) and **nft mode** (Linux): TCP and DNS through one SSH login.
- **tun mode** (macOS, Linux): TCP, all UDP, DNS and IPv6.
- **SOCKS5 proxy** (CONNECT and UDP ASSOCIATE), which needs no root.
- The server is a small static Linux program (x86_64, arm64), uploaded over SSH on first use.
- Prebuilt clients for macOS (Apple Silicon, Intel) and Linux (x86_64, arm64), as `.tar.gz`, `.deb` and `.rpm`, with an install script.
- Homebrew on macOS and Linux: `brew install allwinpower/tap/sshuttle2`.
- The Linux client uses mimalloc instead of musl's allocator. When the CPU is the limit, it moves 17–25% more data at half the CPU. The README's Benchmarks section has the numbers.
