# Speed test results

Measured by the [Speed test](SPEEDTEST.md) workflow: a client VM in Frankfurt; servers with 1 vCPU / 2 GB and 2 vCPU / 4 GB in Frankfurt ("near", about 3 ms away) and in Washington ("far", about 88 ms away); Ookla's speedtest through each tunnel. Every LightNode port tops out at about 100 Mbit/s, so near the client a fast tunnel meets that ceiling.

Two runs so far, both on 23 October 2026:

- [After the far-path fix](#after-the-far-path-fix-1-round): 1 round, with the window brake (#15, released in 0.1.1). Single samples.
- [Before it](#before-the-far-path-fix-3-rounds): 3 rounds, medians.

A 3-round run with 0.1.2 is still to come (see the [TODO](../TODO.md) list). It will show whether 0.1.2 fixes tun mode's far upload, which so far is confirmed only in an emulated path.

## After the far-path fix (1 round)

### What it shows

- **Far apart, nft mode no longer stalls:** 95–98 Mbit/s down and 39–79 Mbit/s up, with 272–382 ms of ping under load, and no failed test. Before the fix it got 2.3–2.6 Mbit/s, and Ookla failed in every run.
- **The path was better that evening:** sshuttle got 2.4 Mbit/s, against 1.3–1.7 in the first run. So compare within one run, not across runs.
- **tun mode, far apart:** 97 Mbit/s down with 233–252 ms of ping under load. But Ookla failed halfway through the upload on both servers. Its TCP stack let an app upload far ahead of the tunnel, and 0.1.2 fixes that ([third_party/ipstack/PATCHES.md](../third_party/ipstack/PATCHES.md), "Receive window").
- **Near, nothing changed:** 96–97 Mbit/s both ways, 17–46 ms of ping while downloading and 71–90 ms while uploading.

### Measurements

sshuttle2 at `d169f10` (version 0.1.0 plus the window brake; 0.1.1 is this code), sshuttle 2.0.0, Speedtest by Ookla 1.2.0.84.
Started 2026-10-23T22:49:22Z; 1 round per server. [Workflow run](https://github.com/allwinpower/sshuttle2/actions/runs/40032571340).

Client: 2 vCPU / 4 GB in ger-frankfurt-1, Ubuntu 24.04.2 LTS, kernel 6.8.0-62-generic.

#### near-1c: 1 vCPU / 2 GB, same region as the client (ger-frankfurt-1)

Ookla server 44477: TELE AG, Frankfurt, Germany.
LightNode package intel-c1m2-bgp-t1000-s50-d0 (1000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 98 Mbit/s | 92 Mbit/s | 0.7 ms | 1.4 ms | 0.9 ms | – | – | – | – |
| On the server itself | 99 Mbit/s | 91 Mbit/s | 0.6 ms | 1.4 ms | 1.0 ms | – | – | – | – |
| sshuttle | 57 Mbit/s | 59 Mbit/s | 2.4 ms | 8.1 ms | 7.2 ms | 58 % (75 %) | 26 → 26 MB | 68 % (99 %) | 39 → 40 MB |
| sshuttle --no-latency-control (failed 1 of 1, during the upload) | 68 Mbit/s | failed | failed | 1048 ms | failed | – | – | – | – |
| **sshuttle2** nft (TCP + DNS) | 97 Mbit/s | 97 Mbit/s | 2.3 ms | 32 ms | 79 ms | 27 % (54 %) | 12 → 13 MB | 20 % (35 %) | 23 → 40 MB |
| **sshuttle2** tun (TCP + UDP + DNS) | 96 Mbit/s | 95 Mbit/s | 2.3 ms | 46 ms | 90 ms | 26 % (48 %) | 12 → 14 MB | 42 % (132 %) | 24 → 238 MB |

#### near-2c: 2 vCPU / 4 GB, same region as the client (ger-frankfurt-1)

Ookla server 44477: TELE AG, Frankfurt, Germany.
LightNode package intel-c2m4-bgp-t2000-s50-d100 (2000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 99 Mbit/s | 95 Mbit/s | 1.1 ms | 1.5 ms | 0.9 ms | – | – | – | – |
| On the server itself | 98 Mbit/s | 92 Mbit/s | 0.9 ms | 1.4 ms | 0.9 ms | – | – | – | – |
| sshuttle | 65 Mbit/s | 62 Mbit/s | 5.6 ms | 7.0 ms | 6.2 ms | 74 % (97 %) | 25 → 26 MB | 76 % (96 %) | 39 → 39 MB |
| sshuttle --no-latency-control (failed 1 of 1, during the upload) | 95 Mbit/s | failed | failed | 124 ms | failed | – | – | – | – |
| **sshuttle2** nft (TCP + DNS) | 97 Mbit/s | 97 Mbit/s | 1.8 ms | 33 ms | 76 ms | 31 % (68 %) | 12 → 13 MB | 20 % (40 %) | 23 → 40 MB |
| **sshuttle2** tun (TCP + UDP + DNS) | 96 Mbit/s | 96 Mbit/s | 2.1 ms | 17 ms | 71 ms | 36 % (69 %) | 11 → 13 MB | 42 % (126 %) | 24 → 161 MB |

#### far-1c: 1 vCPU / 2 GB, another region (us-washington-1)

Ookla server 30823: ETTE, Silver Spring, MD, United States.
LightNode package intel-c1m2-bgp-t1000-s50-d0 (1000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 99 Mbit/s | 101 Mbit/s | 88 ms | 135 ms | 132 ms | – | – | – | – |
| On the server itself | 98 Mbit/s | 101 Mbit/s | 2.7 ms | 3.8 ms | 3.2 ms | – | – | – | – |
| sshuttle | 2.4 Mbit/s | 2.5 Mbit/s | 113 ms | 183 ms | 182 ms | 6.7 % (12 %) | 26 → 26 MB | 7.0 % (13 %) | 39 → 40 MB |
| sshuttle --no-latency-control (failed 1 of 1, during the download) | 38 Mbit/s | failed | failed | 1490 ms | failed | – | – | – | – |
| **sshuttle2** nft (TCP + DNS) | 95 Mbit/s | 79 Mbit/s | 112 ms | 272 ms | 292 ms | 18 % (33 %) | 12 → 15 MB | 16 % (37 %) | 23 → 42 MB |
| **sshuttle2** tun (TCP + UDP + DNS) (failed 1 of 1, during the upload) | 97 Mbit/s | failed | failed | 233 ms | failed | – | – | – | – |

#### far-2c: 2 vCPU / 4 GB, another region (us-washington-1)

Ookla server 30823: ETTE, Silver Spring, MD, United States.
LightNode package intel-c2m4-bgp-t2000-s50-d100 (2000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 99 Mbit/s | 100 Mbit/s | 88 ms | 133 ms | 133 ms | – | – | – | – |
| On the server itself | 98 Mbit/s | 101 Mbit/s | 2.8 ms | 3.6 ms | 2.8 ms | – | – | – | – |
| sshuttle | 2.4 Mbit/s | 2.4 Mbit/s | 113 ms | 182 ms | 182 ms | 8.1 % (15 %) | 25 → 26 MB | 7.1 % (13 %) | 39 → 40 MB |
| sshuttle --no-latency-control (failed 1 of 1, during the download) | 59 Mbit/s | failed | failed | 2034 ms | failed | – | – | – | – |
| **sshuttle2** nft (TCP + DNS) | 98 Mbit/s | 39 Mbit/s | 112 ms | 295 ms | 382 ms | 22 % (45 %) | 12 → 15 MB | 12 % (29 %) | 26 → 48 MB |
| **sshuttle2** tun (TCP + UDP + DNS) (failed 1 of 1, during the upload) | 97 Mbit/s | failed | failed | 252 ms | failed | – | – | – | – |

Ping: Ookla's idle latency, then the latency while it downloads and while it uploads (interquartile mean).
CPU: average while Ookla ran, in % of one core; in parentheses the busiest second. RAM: resident memory, idle → peak.
Server: sshd plus the tunnel's server process. Client: the tunnel client, its helpers and its ssh.
Failed: Ookla stopped with an error, mostly "Cannot read from socket" when its connection stalled. A failed run still counts for what Ookla finished before (the download); CPU and RAM count only finished runs.

## Before the far-path fix (3 rounds)

### What it shows

- **Near, sshuttle2 fills the port both ways:** 96–97 Mbit/s down and up, where sshuttle reaches 55–65 down.
- **With half the server:** 26–31 % of one core against sshuttle's 61–74 %, and 13–14 MB of memory against 30–33 MB.
- **The price is delay under load:** 34–47 ms of ping while downloading and 79–94 ms while uploading, against about 9 ms with sshuttle. sshuttle2 moves more data and queues more of it.
- **tun mode's client grew to about 240 MB** of memory under load; nft mode's stays near 40 MB. Most likely those were uploads queued in tun mode's TCP stack, which 0.1.2 bounds.
- **sshuttle --no-latency-control** never finished a test with the 1 vCPU server, and finished 1 of 3 with the 2 vCPU one: Ookla stalled.
- **Far apart, every tunnel stalled.** sshuttle got 1.3–1.7 Mbit/s; sshuttle2 got 1.8–2.6 Mbit/s, and Ookla gave up mid-test in every run. Without a tunnel the same path does 100 Mbit/s. The window brake fixed this for nft mode (above).

### Measurements

sshuttle2 0.1.0-rc.1 (`fbd6f56`), sshuttle 2.0.0, Speedtest by Ookla 1.2.0.84.
Started 2026-10-23T18:54:22Z; 3 rounds per server; every number is the median. [Workflow run](https://github.com/allwinpower/sshuttle2/actions/runs/40006537926).

Client: 2 vCPU / 4 GB in ger-frankfurt-1, Ubuntu 24.04.2 LTS, kernel 6.8.0-62-generic.

#### near-1c: 1 vCPU / 2 GB, same region as the client (ger-frankfurt-1)

Ookla server 44477: TELE AG, Frankfurt, Germany.
LightNode package intel-c1m2-bgp-t1000-s50-d0 (1000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 99 Mbit/s | 64 Mbit/s | 2.0 ms | 3.7 ms | 3.2 ms | – | – | – | – |
| On the server itself | 98 Mbit/s | 59 Mbit/s | 4.5 ms | 3.7 ms | 3.2 ms | – | – | – | – |
| sshuttle | 55 Mbit/s | 58 Mbit/s | 4.0 ms | 10 ms | 8.5 ms | 61 % (78 %) | 26 → 30 MB | 74 % (101 %) | 39 → 40 MB |
| sshuttle --no-latency-control (failed 3 of 3, during the download) | 68 Mbit/s | failed | failed | 1436 ms | failed | – | – | – | – |
| **sshuttle2** nft (TCP + DNS) | 97 Mbit/s | 97 Mbit/s | 4.5 ms | 47 ms | 82 ms | 26 % (51 %) | 12 → 13 MB | 21 % (38 %) | 24 → 42 MB |
| **sshuttle2** tun (TCP + UDP + DNS) | 96 Mbit/s | 95 Mbit/s | 6.3 ms | 38 ms | 93 ms | 26 % (47 %) | 12 → 14 MB | 37 % (122 %) | 24 → 241 MB |

#### near-2c: 2 vCPU / 4 GB, same region as the client (ger-frankfurt-1)

Ookla server 44477: TELE AG, Frankfurt, Germany.
LightNode package intel-c2m4-bgp-t2000-s50-d100 (2000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 98 Mbit/s | 58 Mbit/s | 3.5 ms | 3.4 ms | 3.2 ms | – | – | – | – |
| On the server itself | 98 Mbit/s | 59 Mbit/s | 4.5 ms | 3.5 ms | 2.7 ms | – | – | – | – |
| sshuttle | 65 Mbit/s | 54 Mbit/s | 5.4 ms | 8.6 ms | 8.4 ms | 74 % (93 %) | 25 → 33 MB | 83 % (105 %) | 39 → 40 MB |
| sshuttle --no-latency-control (failed 2 of 3, during the upload) | 92 Mbit/s | 90 Mbit/s | 6.0 ms | 249 ms | 1585 ms | 77 % (139 %) | 25 → 29 MB | 77 % (130 %) | 39 → 164 MB |
| **sshuttle2** nft (TCP + DNS) | 96 Mbit/s | 97 Mbit/s | 3.3 ms | 34 ms | 79 ms | 28 % (59 %) | 12 → 13 MB | 20 % (39 %) | 22 → 38 MB |
| **sshuttle2** tun (TCP + UDP + DNS) | 96 Mbit/s | 96 Mbit/s | 6.3 ms | 36 ms | 94 ms | 31 % (65 %) | 12 → 13 MB | 41 % (133 %) | 24 → 236 MB |

#### far-1c: 1 vCPU / 2 GB, another region (us-washington-1)

Ookla server 30823: ETTE, Silver Spring, MD, United States.
LightNode package intel-c1m2-bgp-t1000-s50-d0 (1000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 100 Mbit/s | 102 Mbit/s | 88 ms | 133 ms | 133 ms | – | – | – | – |
| On the server itself | 98 Mbit/s | 101 Mbit/s | 2.4 ms | 3.0 ms | 2.8 ms | – | – | – | – |
| sshuttle | 1.3 Mbit/s | 0.8 Mbit/s | 114 ms | 274 ms | 372 ms | 4.8 % (9.8 %) | 26 → 26 MB | 4.7 % (8.8 %) | 39 → 40 MB |
| sshuttle --no-latency-control (failed 3 of 3, during the download) | 7.6 Mbit/s | failed | failed | 2000 ms | failed | – | – | – | – |
| **sshuttle2** nft (TCP + DNS) (failed 3 of 3, during the download) | 2.3 Mbit/s | failed | failed | 1779 ms | failed | – | – | – | – |
| **sshuttle2** tun (TCP + UDP + DNS) (failed 3 of 3, during the download) | 1.8 Mbit/s | failed | failed | 3959 ms | failed | – | – | – | – |

#### far-2c: 2 vCPU / 4 GB, another region (us-washington-1)

Ookla server 30823: ETTE, Silver Spring, MD, United States.
LightNode package intel-c2m4-bgp-t2000-s50-d100 (2000 GB traffic a month).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 99 Mbit/s | 102 Mbit/s | 88 ms | 133 ms | 132 ms | – | – | – | – |
| On the server itself | 98 Mbit/s | 101 Mbit/s | 2.6 ms | 3.4 ms | 2.9 ms | – | – | – | – |
| sshuttle | 1.7 Mbit/s | 1.1 Mbit/s | 114 ms | 236 ms | 308 ms | 5.4 % (12 %) | 25 → 26 MB | 5.3 % (9.8 %) | 39 → 40 MB |
| sshuttle --no-latency-control (failed 3 of 3, during the download) | 16 Mbit/s | failed | failed | 3574 ms | failed | – | – | – | – |
| **sshuttle2** nft (TCP + DNS) (failed 3 of 3, during the download, during the upload) | 2.6 Mbit/s | failed | failed | 2167 ms | failed | – | – | – | – |
| **sshuttle2** tun (TCP + UDP + DNS) (failed 3 of 3, during the download, during the upload) | 2.6 Mbit/s | failed | failed | 2468 ms | failed | – | – | – | – |

Ping: Ookla's idle latency, then the latency while it downloads and while it uploads (interquartile mean).
CPU: average while Ookla ran, in % of one core; in parentheses the busiest second. RAM: resident memory, idle → peak.
Server: sshd plus the tunnel's server process. Client: the tunnel client, its helpers and its ssh.
Failed: Ookla stopped with an error, mostly "Cannot read from socket" when its connection stalled. A failed run still counts for what Ookla finished before (the download); CPU and RAM count only finished runs.
