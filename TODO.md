# TODO

- [ ] **Repeat the speed test on a quiet network, and update the numbers.**
  The numbers in the story are from the first run on 23 October 2026: single samples on a shared Wi-Fi, a 2 vCPU / 4 GB server.
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
  The 1-round run with the brake ([run 40032571340](https://github.com/allwinpower/sshuttle2/actions/runs/40032571340), 23 October 2026) is in [docs/speed-results.md](docs/speed-results.md#after-the-far-path-fix-1-round). nft no longer stalls: 95–98 Mbit/s down and 39–79 Mbit/s up. tun failed halfway through the upload, because its TCP stack let an app upload far ahead of the tunnel; 0.1.2 fixes that with the receive window patch (third_party/ipstack/PATCHES.md).
  To do: rerun with 3 rounds on 0.1.2, then update docs/speed-results.md and the README's far table. The first try ([run 40043580748](https://github.com/allwinpower/sshuttle2/actions/runs/40043580748)) could not create its VMs: the VM provider pauses new VMs after many create/delete runs. Earliest retry: 31 October 2026.

- [ ] **Try mimalloc in the server too.** The uploaded server is also built with musl, so mimalloc could speed it up as well, as it did the Linux client (README, "Linux client and mimalloc"). It would add about 10% to the 1.3 MB that is uploaded on first use, so it is a separate decision.
  To measure, run `scripts/alloc-bench.sh` once per server build, with the same client.

- [ ] **Test two clients on one server at once.** Two devices can use the same server: each SSH login starts its own server process, and the only thing they share is the cached server binary, which each upload writes to its own temp file and then renames into place. Two bootstrap scripts racing into one empty cache passed 20 rounds out of 20 (27 October 2026). But no test runs two full clients at the same time.
  To do: in `scripts/e2e.sh docker`, start a second client on its own SOCKS5 port against the same container, download through both at once, and check both finish.

- [ ] **apt and dnf repositories, when users ask for them.** The `.deb` and `.rpm` from a release install with `sudo apt install ./file.deb` or `sudo dnf install ./file.rpm`, but they never update, because apt and dnf do not know where they came from. A package repository fixes that: users add it once, then `sudo apt install sshuttle2`, and `apt upgrade` or `dnf upgrade` brings each new release.
  The cost is a GPG signing key to keep safe and renew, hosting (for example a separate repo served by GitHub Pages), and a publish step in `release.yml` on every release. Build the APT repository with [aptly](https://www.aptly.info/doc/aptly/publish/) and the RPM one with `createrepo_c`, and sign both. That is about 1–2 days to set up, then about 1–2 hours a month. Start when about 5 users ask. Until then, Homebrew on Linux (and the AUR, once it takes new accounts) gives automatic updates.

- [ ] **AUR package `sshuttle2-bin`, once the AUR takes new accounts.** The README promises it. The package and its workflow are ready (`packaging/aur`, `.github/workflows/aur.yml`), and every release build already checks it on Arch Linux. Missing: an AUR account, an SSH key in it, the `AUR_SSH_KEY` secret, then `gh workflow run aur.yml -f version=X` ([docs/RELEASING.md](docs/RELEASING.md)) and an AUR line in the README's install section.

- [ ] **Story page on sshuttle2.openrepos.org, once the claim merges.** The claim ([openrepos-org/register#26](https://github.com/openrepos-org/register/pull/26)) is still open. Until it merges, the name only redirects to openrepos.org, and the README and STORY.md link to the story at allwinpower.github.io/sshuttle2. After it merges: `gh api -X PUT repos/allwinpower/sshuttle2/pages -f cname=sshuttle2.openrepos.org`, then `-F https_enforced=true` once the certificate exists, then point those two links back to sshuttle2.openrepos.org. Not before: GitHub would send github.io visitors to a name that does not serve the story yet.
