## Speed test: sshuttle and sshuttle2

sshuttle2 0.1.0-rc.1 (`0123456`), sshuttle 2.0.0, Speedtest by Ookla 1.2.0.84.
Started 2026-10-24T02:00:00Z; 2 rounds per server; every number is the median. [Workflow run](https://github.com/allwinpower/sshuttle2/actions/runs/1).

Client: 2 vCPU / 4 GB in region-a, Ubuntu 24.04.1 LTS, kernel 6.8.0-45-generic.

### near-1c: 1 vCPU / 2 GB, same region as the client (region-a)

Ookla server 12345: Example Net, Example City, Exampleland.
LightNode package pkg-c1m2 (bandwidth 1000).

| | Download | Upload | Ping | Ping, downloading | Ping, uploading | Server CPU | Server RAM | Client CPU | Client RAM |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| No tunnel | 900 Mbit/s | 850 Mbit/s | 2.1 ms | 13 ms | 19 ms | – | – | – | – |
| On the server itself | 950 Mbit/s | 900 Mbit/s | 1.2 ms | 7.2 ms | 11 ms | – | – | – | – |
| sshuttle | 180 Mbit/s | 160 Mbit/s | 3.0 ms | 18 ms | 27 ms | 98 % (100 %) | 21 → 24 MB | 96 % (100 %) | 30 → 34 MB |
| sshuttle --no-latency-control (1 of 2 runs) | 235 Mbit/s | 206 Mbit/s | 3.0 ms | 18 ms | 27 ms | 98 % (100 %) | 21 → 25 MB | 98 % (100 %) | 30 → 35 MB |
| **sshuttle2** nft (TCP + DNS) | 610 Mbit/s | 520 Mbit/s | 2.6 ms | 16 ms | 23 ms | 60 % (76 %) | 9.1 → 13 MB | 54 % (71 %) | 11 → 18 MB |
| **sshuttle2** tun (TCP + UDP + DNS) | 560 Mbit/s | 480 Mbit/s | 2.8 ms | 17 ms | 25 ms | 66 % (80 %) | 9.2 → 14 MB | 68 % (88 %) | 14 → 26 MB |

### far-1c: 1 vCPU / 2 GB, another region (region-b)

Skipped: no Ookla server found.

Ping: Ookla's idle latency, then the latency while it downloads and while it uploads (interquartile mean).
CPU: average while Ookla ran, in % of one core; in parentheses the busiest second. RAM: resident memory, idle → peak.
Server: sshd plus the tunnel's server process. Client: the tunnel client, its helpers and its ssh.
