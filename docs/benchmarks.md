# Benchmarks

Throughput and latency through sshuttle2 and sshuttle on rented cloud servers, and the cost of the Linux client's memory allocator.

## Through the internet

Ookla's speedtest through each tunnel. The client VM is in Frankfurt; the servers are in Frankfurt (about 3 ms away) and Washington (about 88 ms away). Every port on these VMs tops out at about 100 Mbit/s. Raw data, CPU and memory for every mode and server size: [speed-results.md](speed-results.md). How to run it: [SPEEDTEST.md](SPEEDTEST.md).

### Near: client and server in Frankfurt

sshuttle2 0.1.0-rc.1, sshuttle 2.0.0, a 1 vCPU / 2 GB server, medians of 3 rounds. A later round with 0.1.1 gave the same results.

| | Download | Upload | Ping while downloading | Server CPU | Server RAM |
|---|---:|---:|---:|---:|---:|
| sshuttle | 55 Mbit/s | 58 Mbit/s | 10 ms | 61 % | 30 MB |
| sshuttle2, nft mode (TCP + DNS) | 97 Mbit/s | 97 Mbit/s | 47 ms | 26 % | 13 MB |
| sshuttle2, tun mode (TCP + UDP + DNS) | 96 Mbit/s | 95 Mbit/s | 38 ms | 26 % | 14 MB |

Server CPU is the average while the test ran, in % of one core. Server RAM is the peak.

sshuttle2 fills the 100 Mbit/s port in both directions with less than half of sshuttle's server CPU and memory. sshuttle keeps a lower ping while the link is full.

### Far: client in Frankfurt, server in Washington

sshuttle2 0.1.1, sshuttle 2.0.0. One round on each of two servers (1 vCPU / 2 GB and 2 vCPU / 4 GB); ranges span the two. Without a tunnel, this path does about 100 Mbit/s in both directions.

| | Download | Upload | Ping while downloading |
|---|---:|---:|---:|
| sshuttle | 2.4 Mbit/s | 2.4–2.5 Mbit/s | 182–183 ms |
| sshuttle2, nft mode (TCP + DNS) | 95–98 Mbit/s | 39–79 Mbit/s | 272–295 ms |
| sshuttle2, tun mode (TCP + UDP + DNS) | 97 Mbit/s | not measured | 233–252 ms |

On this path sshuttle2 downloads about 40 times faster than sshuttle. The tun mode upload failed in this run because of a bug fixed in 0.1.2; a 3-round rerun on the current release is tracked in [#36](https://github.com/allwinpower/sshuttle2/issues/36).

## Linux client allocator

The Linux client is a static musl binary, so it runs on any distribution. musl's memory allocator is slow, so the Linux client uses [mimalloc](https://github.com/microsoft/mimalloc) instead. The macOS client uses the system allocator.

Three clients built from the same source, on a 1 vCPU / 2 GB cloud server, with client and server sharing the core over `-r local` (no SSH, no network), so the CPU is the limit. Each run downloads a 300 MB file once, then 4 times in parallel. Medians of 3 runs.

| Linux client | 1 download | 4 in parallel | Client CPU for 1.5 GB |
|---|---:|---:|---:|
| musl, default allocator | 1514 Mbit/s | 1511 Mbit/s | 2.6 s |
| musl + mimalloc | 1900 Mbit/s | 1773 Mbit/s | 1.3 s |
| glibc, for comparison | 1712 Mbit/s | 1621 Mbit/s | 1.6 s |

mimalloc halves the client's CPU time and moves 17–25% more data than musl's allocator, ahead of glibc too. At 100 Mbit/s the difference is about 1% of one core; it matters on fast links and small CPUs. It adds 128 KiB to the binary.

To reproduce on Linux, with Linux builds of the server and of each client:

```bash
scripts/alloc-bench.sh SERVER_BIN CLIENT_BIN...
```
