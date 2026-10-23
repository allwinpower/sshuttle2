# Speed test results

Measured on 23 October 2026 by the [Speed test](SPEEDTEST.md) workflow: a client VM in Frankfurt; servers with 1 vCPU / 2 GB and 2 vCPU / 4 GB in Frankfurt ("near", about 3 ms away) and in Washington ("far", about 88 ms away); Ookla's speedtest through each tunnel, 3 rounds, medians. Every LightNode port tops out at about 100 Mbit/s, so near the client a fast tunnel meets that ceiling.

## What it shows

- **Near, sshuttle2 fills the port both ways:** 96–97 Mbit/s down and up, where sshuttle reaches 55–65 down.
- **With half the server:** 26–31 % of one core against sshuttle's 61–74 %, and 13–14 MB of memory against 30–33 MB.
- **The price is delay under load:** 34–47 ms of ping while downloading and 79–94 ms while uploading, against about 9 ms with sshuttle. sshuttle2 moves more data and queues more of it.
- **tun mode's client grows to about 240 MB** of memory under load; nft mode's stays near 40 MB.
- **sshuttle --no-latency-control** never finished a test with the 1 vCPU server, and finished 1 of 3 with the 2 vCPU one: Ookla stalled.
- **Far apart, every tunnel stalls.** sshuttle got 1.3–1.7 Mbit/s; sshuttle2 got 1.8–2.6 Mbit/s, and Ookla gave up mid-test in every run. Without a tunnel the same path does 100 Mbit/s. This is the open flow-control problem in the [TODO](../TODO.md) list.

## Measurements

sshuttle2 0.1.0-rc.1 (`fbd6f56`), sshuttle 2.0.0, Speedtest by Ookla 1.2.0.84.
Started 2026-10-23T18:54:22Z; 3 rounds per server; every number is the median. [Workflow run](https://github.com/allwinpower/sshuttle2/actions/runs/40006537926).

Client: 2 vCPU / 4 GB in ger-frankfurt-1, Ubuntu 24.04.2 LTS, kernel 6.8.0-62-generic.

### near-1c: 1 vCPU / 2 GB, same region as the client (ger-frankfurt-1)

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

### near-2c: 2 vCPU / 4 GB, same region as the client (ger-frankfurt-1)

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

### far-1c: 1 vCPU / 2 GB, another region (us-washington-1)

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

### far-2c: 2 vCPU / 4 GB, another region (us-washington-1)

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
