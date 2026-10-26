# Changelog

What changed in each release of sshuttle2. A release's section here becomes its GitHub Release notes ([docs/RELEASING.md](docs/RELEASING.md)).

## [0.2.0] - 2026-10-26

- **macOS servers.** The server can now be a Mac (Apple Silicon or Intel) with Remote Login on, not only Linux. The client picks the server binary by the remote host's system and CPU. Each client is about 2.2 MB larger, as it carries the two macOS servers too.

## [0.1.2] - 2026-10-24

- **tun mode: uploads wait for the tunnel.** The built-in TCP stack acknowledged every byte an app sent at once and queued it unread. So an upload through a slower tunnel looked sent long before it was, Ookla's speedtest gave up waiting for the server's answer (it failed in the upload with the server far away), and the client could grow by gigabytes. Now the receive window closes once 256 KiB of a connection wait unread, and reopens as the tunnel takes them.

## [0.1.1] - 2026-10-23

- **Slow or distant servers no longer stall the tunnel.** Each connection kept at least 256 KiB in flight, so when the path was much slower than that (as across an ocean, where Ookla's speedtest stalled in every run), several downloads together queued seconds of data in front of every other connection. A connection's window now also shrinks below its starting size while it only builds a queue, and grows back when the path gets faster.
- An Arch Linux package (`.pkg.tar.zst`, x86_64 and aarch64) comes with every final release: `sudo pacman -U` its link. The AUR package `sshuttle2-bin` follows when the AUR takes new accounts again.

## [0.1.0] - 2026-10-23

The first release.

- **pf mode** (macOS) and **nft mode** (Linux): TCP and DNS through one SSH login.
- **tun mode** (macOS, Linux): TCP, all UDP, DNS and IPv6.
- **SOCKS5 proxy** (CONNECT and UDP ASSOCIATE), which needs no root.
- The server is a small static Linux program (x86_64, arm64), uploaded over SSH on first use.
- Prebuilt clients for macOS (Apple Silicon, Intel) and Linux (x86_64, arm64), as `.tar.gz`, `.deb` and `.rpm`, with an install script.
- Homebrew on macOS and Linux: `brew install allwinpower/tap/sshuttle2`.
- The Linux client uses mimalloc instead of musl's allocator. When the CPU is the limit, it moves 17–25% more data at half the CPU. The README's Benchmarks section has the numbers.
