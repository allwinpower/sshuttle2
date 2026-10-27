# Why sshuttle2 exists

A hospital's Wi-Fi blocked everything except SSH. To get my work done, I first had to fix my internet connection. This is why I wrote sshuttle2, a Rust implementation of the idea behind [sshuttle](https://github.com/sshuttle/sshuttle).

Also on the web: [allwinpower.github.io/sshuttle2](https://allwinpower.github.io/sshuttle2/).

## The network

I was in a hospital for a while, with my Mac, my phone and work to do. The patient Wi-Fi runs through a Fortinet firewall that inspects HTTPS. The screenshots are from the phone (Brave on Android); the Mac got the same errors.

| claude.ai | chatgpt.com | en.wikipedia.org |
|---|---|---|
| ![claude.ai blocked by Fortinet](docs/story/img/blocked-claude.png) | ![chatgpt.com blocked by Fortinet](docs/story/img/blocked-chatgpt.png) | ![Wikipedia blocked by Fortinet](docs/story/img/blocked-wikipedia.png) |

What the firewall blocked, on both devices:

- Claude, ChatGPT and every other AI tool I tried.
- Every VPN I tried: WireGuard, Shadowsocks, Mullvad.
- Wikipedia.

The error is `NET::ERR_CERT_AUTHORITY_INVALID`. The firewall decrypts each HTTPS connection and signs it again with its own certificate, which my devices do not trust. Brave suggests disabling Fortinet or connecting to another network. A patient can do neither.

## What got through

SSH on port 22 was not blocked, so I could log in to my own server from the Mac.

```
          Fortinet
             |
 [Mac]   ----X---> claude.ai, chatgpt.com
 [phone] ----X---> wikipedia.org
         ----X---> WireGuard, Shadowsocks,
                   Mullvad, ...

 [Mac] == ssh :22 ==> [my server] --> internet
```

A network that allows SSH allows a tunnel. That is the idea behind [sshuttle](https://github.com/sshuttle/sshuttle): a VPN over a plain SSH login, with nothing to install on the server.

## sshuttle

sshuttle worked on the Mac, but slowly:

- Downloads ran at 12.8 Mbit/s, on a Wi-Fi that does 91.9 Mbit/s to sites it does not block.
- A small page request took 1.8 seconds, and 2.3 seconds while a download was running.

sshuttle relays every connection through one single-threaded Python process on the server, and all packets share one queue. It has served people well for years, but it was too slow for a working day.

## sshuttle2

So I wrote sshuttle2 in Rust. Both ends are new code, and the idea is the same: the server needs nothing but an SSH login. On first run, the client uploads a small server binary (about 1.3 MB) over SSH and reuses it after that. It runs on macOS and Linux.

What is different:

- **Rust at both ends.** No Python in the data path.
- **One SSH session, one channel per connection.** Each channel has its own flow control, so a large download does not hold up other connections.
- **Self-sizing windows.** A channel's window grows while it limits throughput and shrinks when it only adds queueing delay.
- **TCP, DNS and UDP.** In tun mode, video calls, QUIC and games go through the tunnel too.
- **Clean exit.** Firewall rules and routes are removed on exit, also after a crash.

## Results

Measured on the Mac, with the same Wi-Fi and the same server (2 vCPU, 4 GB) on the same day. Single samples. Repeated measurements on rented servers: [benchmarks](docs/benchmarks.md).

| | Download | Page wait, idle | Page wait, during a download |
|---|---:|---:|---:|
| No tunnel (blocked sites stay blocked) | 91.9 Mbit/s | 128 ms | 65 ms |
| sshuttle | 12.8 Mbit/s | 1817 ms | 2281 ms |
| sshuttle2, pf mode (TCP + DNS) | 19.1 Mbit/s | 131 ms | 338 ms |
| sshuttle2, tun mode (TCP + UDP + DNS) | 17.4 Mbit/s | 211 ms | 223 ms |

Download: one 100 MB file. Page wait: time until `google.com/generate_204` sends its first byte.

- Pages respond about 14 times faster than with sshuttle (1817 ms → 131 ms), and about 10 times faster while a download runs.
- Downloads are about 1.5 times faster.
- Downloads are still far below the 91.9 Mbit/s of the bare Wi-Fi. On rented servers, sshuttle2 fills a 100 Mbit/s link.

## Links

I wrote sshuttle2 to get back to work. If you are behind a firewall that lets only SSH through, it may help you too. Bug reports and pull requests are welcome: [github.com/allwinpower/sshuttle2](https://github.com/allwinpower/sshuttle2).

---

sshuttle2 ports its firewall logic from [sshuttle](https://github.com/sshuttle/sshuttle) and is licensed LGPL-2.1-or-later, like sshuttle.
