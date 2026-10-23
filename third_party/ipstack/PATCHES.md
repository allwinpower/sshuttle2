# ipstack, patched

ipstack 1.0.1 as published on crates.io (upstream commit `a343ea8`,
<https://github.com/narrowlink/ipstack>), Apache-2.0 (see `LICENSE`). Left out
of the published crate: `Cargo.lock`, `.github/`, `scripts/`, `.gitignore`.

The workspace `Cargo.toml` swaps it in with `[patch.crates-io]`; it is not a
workspace member, so it keeps its own lints and formatting (`rustfmt.toml`).
Changed code is marked `sshuttle2 patch`. To drop the copy once upstream
fixes this: delete the `[patch.crates-io]` entry and this directory.

## Half-close (`src/stream/tcp.rs`, `src/stream/tcb.rs`)

An app that sends, shuts down its write side (`shutdown(SHUT_WR)`), then reads
the reply lost the reply's tail, or all of it. Upstream:

- On the app's FIN in Established, with none of our data in flight, the stack
  sent its own FIN at once and went to LastAck; otherwise on the first ACK in
  CloseWait with nothing in flight, or after `close_wait_timeout` (5 s). What
  the upper layer wrote after that went out behind the FIN, and once the app
  acknowledged the FIN, writes failed with `BrokenPipe`.
- `poll_read` returned EOF only once Closed, never on the app's FIN, so the
  upper layer could not tell the app had half-closed; and when Closed, it
  returned EOF ahead of data still queued.
- A FIN on a data segment (FIN|PSH|ACK, which Linux sends when the last
  segment is still queued at shutdown) matched none of the exact `flags ==`
  tests and was ignored: the connection stalled.
- Only 8 KiB of in-order data was taken per incoming segment. With MTU 9000
  each segment left a tail behind, and the FIN after the last one was accepted
  with the wrong sequence number.
- `poll_shutdown` sent the FIN only from Established, and only if nothing was
  in flight at that moment; nothing retried it later.

Now:

- The app's FIN, with or without data, is accepted once everything before it
  has arrived, and only acknowledged. The reader gets what is queued, then EOF.
- `poll_shutdown` records the request (`Tcb::request_fin`). The FIN goes out,
  from Established or CloseWait, as soon as none of our data is in flight: at
  once, or from the main loop on the ACK that covers the last of it. CloseWait
  lasts until then, as in a kernel stack (`close_wait_timeout` is unused).
- Every incoming segment takes all in-order data.
- In CloseWait the stack answers a retransmission request as in Established,
  and acknowledges a repeated FIN.
- A stream dropped before a clean close sends RST. Without the spontaneous FIN,
  an upper layer that gives up (a tunnel error) would otherwise leave a
  half-closed app waiting forever.

Tests: `crates/client/src/tunmode.rs` (`half_close_*`,
`dropped_stream_resets_half_closed_app`) drives the stack from a hand-written
TCP peer; `scripts/e2e-tun.sh` runs a half-close echo through a real tun
device in Docker.

## Known gaps, not patched

- The SYN-ACK carries no window scale option, so windows stay under 64 KiB.
- Other flag tests still use `==` (an ACK with ECE or URG set is ignored).
- No retransmission timer: lost segments are resent only when a packet comes
  in, so losing the last segment stalls until the app sends something.
- FinWait2 does not acknowledge a FIN that arrives with data; a RST closes the
  connection without waking a pending reader or writer.
