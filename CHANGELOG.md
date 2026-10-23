# Changelog

What changed in each release of sshuttle2. A release's section here becomes its GitHub Release notes ([docs/RELEASING.md](docs/RELEASING.md)).

## [0.1.0] - Unreleased

The first release.

- **pf mode** (macOS) and **nft mode** (Linux): TCP and DNS through one SSH login.
- **tun mode** (macOS, Linux): TCP, all UDP, DNS and IPv6.
- **SOCKS5 proxy** (CONNECT and UDP ASSOCIATE), which needs no root.
- The server is a small static Linux program (x86_64, arm64), uploaded over SSH on first use.
- Prebuilt clients for macOS (Apple Silicon, Intel) and Linux (x86_64, arm64), as `.tar.gz`, `.deb` and `.rpm`, with an install script.
- Homebrew on macOS and Linux: `brew install allwinpower/tap/sshuttle2`.
