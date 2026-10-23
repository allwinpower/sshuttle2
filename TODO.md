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

- [ ] **One download is slower than plain SSH.** On the same path, a single download got 22.9 Mbit/s through sshuttle2 and 37.9 Mbit/s through `ssh -D` (see above), while 4 at once got more than `ssh -D`. Find what holds one lane back, for example a window that stops growing too early, and fix it without losing the low page wait under load.
  The Speed test shows it far worse at a distance ([docs/speed-results.md](docs/speed-results.md)): with the server 88 ms away, sshuttle2 got 1.8–2.6 Mbit/s and Ookla stalled in every run, while the same path does 100 Mbit/s without a tunnel (sshuttle: 1.3–1.7 Mbit/s). In one earlier single round on that path, sshuttle2 nft downloaded at 93 Mbit/s but uploaded at 3.3 Mbit/s, with 2.2 s of ping during the upload. A test for a fix: the far rows of the Speed test, 3 rounds.


- [ ] **Try mimalloc in the server too, after 0.1.0.** The uploaded server is also built with musl, so mimalloc could speed it up as well, as it did the Linux client (README, "Linux client and mimalloc"). It would add about 10% to the 1.3 MB that is uploaded on first use, so it is a separate decision.
  To measure, run `scripts/alloc-bench.sh` once per server build, with the same client.
