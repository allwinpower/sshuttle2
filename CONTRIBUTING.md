# Contributing

Bug fixes, speed improvements and new platforms are welcome. For a larger change, open an [issue](https://github.com/allwinpower/sshuttle2/issues) first to agree on the approach. Questions go in [Discussions](https://github.com/allwinpower/sshuttle2/discussions). Security problems go through [SECURITY.md](SECURITY.md), not a public issue.

## Setup

Rust comes from [rustup](https://rustup.rs). The first `cargo` command in the repository installs the toolchain pinned in `rust-toolchain.toml`: stable, with clippy, rustfmt and the Linux and macOS targets.

`cargo xtask dist` builds the servers (Linux and macOS) and a client that embeds them. It needs zig and cargo-zigbuild:

```bash
brew install zig cargo-zigbuild
```

On Linux, install zig from your package manager or [ziglang.org](https://ziglang.org/download/), then:

```bash
cargo install cargo-zigbuild
```

A plain `cargo build` needs neither, but that client embeds no servers and needs `--server-bin PATH`.

The end-to-end scripts, except `scripts/e2e.sh local`, need Docker.

## Checks

Run this before you open a pull request:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace
```

CI builds with `RUSTFLAGS=-D warnings`, so any warning fails it.

## End-to-end tests

| Script | What it tests | Needs |
|---|---|---|
| `scripts/e2e.sh local` | SOCKS5 through a server run as a local child process | `cargo build --release` |
| `scripts/e2e.sh docker` | SOCKS5 over real ssh into an Alpine container | `cargo xtask dist`, Docker |
| `scripts/e2e-tun.sh` | tun mode (TCP, UDP, DNS) in Linux containers | a static Linux client, Docker |
| `scripts/e2e-nft.sh` | nft mode (TCP, DNS) in Linux containers | a static Linux client, Docker |
| `scripts/mac-server-check.sh` | Linux clients in containers using a Mac as their server, every mode | a Mac, a static Linux client, Docker |

Each script's header says how to build the client it needs. CI runs all of them except `mac-server-check.sh`.

## Fuzzing

`fuzz/` holds [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for input that a hostile server or a local app controls:

| Target | Input |
|---|---|
| `frame` | frames in any chunk size, then every payload decoder |
| `handshake` | the server's startup lines and HELLO frame |
| `ipfrag` | IPv4 fragments into tun mode's reassembler |
| `tcp_stack` | TCP segments into the vendored ipstack, configured as tun mode does |

They need nightly Rust and cargo-fuzz (`cargo install cargo-fuzz`, or `brew install cargo-fuzz`):

```bash
cargo +nightly fuzz run frame -- -max_total_time=600
```

A crash leaves its input in `fuzz/artifacts/`. Add it as a unit test with the fix. CI runs each target for 60 seconds when the code it covers changes, and weekly.

## macOS

CI cannot run macOS code. It checks that the code compiles for macOS, and nothing more. If your change touches macOS, say in the pull request what you tested on a Mac and how.

## Pull requests

Pull requests are squash-merged, so the title becomes the commit message. Write it as `area: what changed`, for example `tun mode: keep the device of a link-local IPv6 gateway`.

A change users notice gets a line under `[Unreleased]` in [CHANGELOG.md](CHANGELOG.md).

## Doc style

- Present tense: say what the code does now.
- History goes in CHANGELOG.md only.
- No jokes or metaphors.
- Bold only for key facts.

## License

Contributions are licensed under LGPL-2.1-or-later, the same as the project. Code under `third_party/` keeps its own license.
