# sshuttle2 wire protocol (version 1)

The client runs `ssh HOST -- /bin/sh -c '<bootstrap>'`. Everything below travels over that one ssh session's stdin/stdout.

## 1. Bootstrap (text lines)

Every line the remote script prints starts with a nonce the client picked for this run. The client ignores lines without it, which covers MOTD and shell rc output.

| Step | Who | Line |
|---|---|---|
| 1 | remote | `<nonce> ARCH <x86_64\|aarch64\|…>` |
| 2 | client | `<sha256-hex> <size>` (the server binary for that arch) |
| 3a | remote | `<nonce> NEED` → client sends exactly `<size>` raw bytes |
| 3b | remote | `<nonce> ERR <message>` → client gives up |
| 4 | remote | `<nonce> GO <path>` and the script `exec`s the server |
| 5 | server | `<nonce> SSHUTTLE2 <protocol-version>` |

**Lockstep.** Neither side sends anything the other has not asked for. `head -c` may read past its count on a pipe, so pipelining would lose bytes.

**Self-check.** The script verifies the binary with `<binary> --self-check <sha256>`, which hashes its own executable. That covers cache hits and fresh uploads, and the server needs no `sha256sum` installed.

**Cache directories, first usable wins:**
1. `${XDG_CACHE_HOME:-~/.cache}/sshuttle2`
2. `$XDG_RUNTIME_DIR/sshuttle2`
3. `/tmp/sshuttle2-$UID`
4. `/dev/shm/sshuttle2-$UID`

The shared `/tmp` and `/dev/shm` directories must be real directories owned by the user, mode 0700. If the self-check exits 126, the mount is noexec, and the script copies the binary to the next directory.

After line 5 both sides speak frames.

## 2. Frames

```
+--------+-----------+-----------+------------------+
| kind u8| chan u32  | len u32   | payload (len)    |
+--------+-----------+-----------+------------------+
```

- Integers are big-endian.
- `len` is at most 66 048 bytes, enough for a 64 KiB UDP datagram plus its address.
- DATA frames carry at most 32 KiB.
- Only the client allocates channel ids. They start at 1 and are never reused.

### Address encoding

Addresses use SOCKS5's layout, then a `u16` port:
- `1` + 4 bytes: IPv4
- `4` + 16 bytes: IPv6
- `3` + `len u8` + name: a domain name, resolved by the server

### Kinds

| # | Kind | Dir | Payload | Meaning |
|---|---|---|---|---|
| 1 | HELLO | both | `key=value\n` lines | First frame each way. Unknown keys are ignored. Client keys: `version`, `dns`. Server keys: `version`, `ipv6`. |
| 2 | PING | both | `u64` stamp | Peer echoes it in PONG; drives RTT measurement. |
| 3 | PONG | both | the stamp | |
| 4 | TCP_CONNECT | c→s | address | Open a TCP connection. The client may send DATA before CONNECT_OK. |
| 5 | CONNECT_OK | s→c | — | Always precedes the channel's first DATA. |
| 6 | CONNECT_ERR | s→c | `code u8` + message | Code is a SOCKS5 reply code (5 = refused, 6 = timed out, …). |
| 7 | DATA | both | bytes | Counts against the sender's credit. |
| 8 | WINDOW | both | `u32` | Grants the peer that many more bytes of credit. |
| 9 | EOF | both | — | Half-close: the sender will send no more DATA. Ordered after its DATA. |
| 10 | RST | both | — | The channel is dead; drop buffered data. |
| 11 | UDP_OPEN | c→s | `4` or `6` | One flow: one client source port, one server socket. |
| 12 | UDP_DATA | both | address + datagram | Destination going out, source coming back. |
| 13 | UDP_CLOSE | both | — | Sent after 60 s idle, or when the client side goes away. |
| 14 | DNS_REQ | c→s | DNS message | Answered with the server's resolvers. |
| 15 | DNS_RESP | s→c | DNS message | |
| 16 | EXIT | both | — | Orderly shutdown. |

## 3. Flow control

**Credit.**
- Each TCP channel starts with 256 KiB of send credit in each direction.
- A sender may have at most its credit in flight.
- The receiver grants more with WINDOW once it has delivered half its window to the local socket.

**Autotuning.** If a window fills up in less than one measured RTT, the receiver doubles it: it grants the extra credit and keeps the bigger window. The ceiling is 16 MiB per channel, and a cap on the sum of all windows bounds total buffering.

**Overruns.** A peer that sends past its credit gets that channel reset. The reader therefore never waits on a slow consumer, and one stalled channel never blocks the others.

**UDP and DNS** have no credit. A receiver that is behind drops datagrams (1 MiB queue per flow), as a router would.

## 4. Ordering and priority

- WINDOW, PONG, PING, RST and EXIT may overtake queued DATA.
- EOF, CONNECT_OK, CONNECT_ERR and DNS_RESP are queued behind their channel's DATA.
- The shared DATA queue is short (16 frames) and senders take turns, so an interactive channel waits behind at most a few hundred KiB of bulk data.
