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

  To do:
  1. Run the [Speed test](docs/SPEEDTEST.md) workflow: fresh servers of both sizes, near and far, medians of 3 rounds, with CPU and memory.
  2. Optional, for the Wi-Fi view: in `scripts/mac-speed-check.sh`, take the median of 5 page waits and of 2 or more downloads, and run it on a quiet network.
  3. Update the Benchmarks table in `README.md`, "Vitals after surgery" in `STORY.md`, and the table, charts and numbers in `docs/story/index.html`.
  4. Never commit server addresses. Name the servers by size only.

- [ ] **One download is slower than plain SSH.** On the same path, a single download got 22.9 Mbit/s through sshuttle2 and 37.9 Mbit/s through `ssh -D` (see above), while 4 at once got more than `ssh -D`. Find what holds one lane back, for example a window that stops growing too early, and fix it without losing the low page wait under load.


- [ ] **Try mimalloc in the server too, after 0.1.0.** The uploaded server is also built with musl, so mimalloc could speed it up as well, as it did the Linux client (README, "Linux client and mimalloc"). It would add about 10% to the 1.3 MB that is uploaded on first use, so it is a separate decision.
  To measure, run `scripts/alloc-bench.sh` once per server build, with the same client.
