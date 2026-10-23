# TODO

- [ ] **Repeat the speed test on a quiet network, and update the numbers.**
  The numbers in the README and the story are from the first run on 23 October 2026: single samples, hospital Wi-Fi, a 2 vCPU / 4 GB server.
  A second run that evening, against a 2 vCPU / 4 GB and a 1 vCPU / 2 GB server, came out flat: every tunnel, sshuttle included, at 13–20 Mbit/s and about 200 ms. The Wi-Fi was crowded. What we measured then:
  - Plain SSH from the Mac (no tunnel software) got about 30 Mbit/s, while direct HTTPS got 88 Mbit/s. On that network, SSH itself was the limit.
  - The servers' own downloads from proof.ovh.net were 42 and 80 Mbit/s. The test file mixes the server's internet path into the result.
  - From the servers, `google.com/generate_204` takes 160–220 ms. That is a floor for the page wait through any tunnel.
  - One sample per test is noisy: the same mode gave 131 ms and 578 ms on the two servers.

  - `scripts/bench.sh` (SOCKS5 mode, a 50 MB file served by the server) on the same crowded Wi-Fi:

    | | `ssh -D` | sshuttle2 `--socks5` |
    |---|---:|---:|
    | 1 download | 37.9 Mbit/s | 22.9 Mbit/s |
    | 4 at once | 30.8 Mbit/s | 37.6 Mbit/s |
    | first byte under load | 1713 ms | 296 ms |

  Done on rented servers: [docs/speed-results.md](docs/speed-results.md), from the [Speed test](docs/SPEEDTEST.md) workflow, linked from the README and the story. Still open, for the Wi-Fi view only: in `scripts/mac-speed-check.sh`, take the median of 5 page waits and of 2 or more downloads, and run it on a quiet network. Never commit server addresses; name the servers by size only.

- [ ] **One download is slower than plain SSH.** On the same path, a single download got 22.9 Mbit/s through sshuttle2 and 37.9 Mbit/s through `ssh -D` (see above), while 4 at once got more than `ssh -D`. Find what holds one lane back, and fix it without losing the low page wait under load.
  Likely cause: a window grows past its starting 256 KiB only while a quarter of it drains in under half the smallest RTT seen ([docs/PROTOCOL.md](docs/PROTOCOL.md), section 3). Once the RTT under load is more than twice the idle one, as on a busy Wi-Fi, one lane stays at 256 KiB per round trip.

- [ ] **Confirm the far-path fix on real servers.** With the server 88 ms away, sshuttle2 got 1.8–2.6 Mbit/s and Ookla stalled in every run ([docs/speed-results.md](docs/speed-results.md)). The single SSH connection got only a few Mbit/s on that path (sshuttle: 1.3–1.7 Mbit/s), and windows never shrank below 256 KiB, so Ookla's 8 connections queued 5–7 s. The brake (PROTOCOL.md, section 3) bounds that queue: in an emulated 88 ms / 3 Mbit/s path, ping under load fell from 5.1 s to 0.29 s.
  Speed test with the brake ([run 40032571340](https://github.com/allwinpower/sshuttle2/actions/runs/40032571340), 1 round, 23 October 2026), far rows:

  | | 1 vCPU server | 2 vCPU server |
  |---|---|---|
  | sshuttle2 nft | 95 down / 79 up Mbit/s, ping 272 / 292 ms | 98 down / 39 up Mbit/s, ping 295 / 382 ms |
  | sshuttle2 tun | 97 down, ping 233 ms; failed in the upload at 53 % | 97 down, ping 252 ms; failed in the upload at 51 % |
  | sshuttle | 2.4 / 2.5 Mbit/s | 2.4 / 2.4 Mbit/s |

  nft no longer stalls. The path was better than in the 3-round run (sshuttle got 2.4 Mbit/s there, against 1.3–1.7). tun failed in the upload because its TCP stack let an app upload far ahead of the tunnel; that is fixed in the receive window patch (third_party/ipstack/PATCHES.md). To do: rerun with 3 rounds after that fix, and update docs/speed-results.md.


- [ ] **Try mimalloc in the server too, after 0.1.0.** The uploaded server is also built with musl, so mimalloc could speed it up as well, as it did the Linux client (README, "Linux client and mimalloc"). It would add about 10% to the 1.3 MB that is uploaded on first use, so it is a separate decision.
  To measure, run `scripts/alloc-bench.sh` once per server build, with the same client.
