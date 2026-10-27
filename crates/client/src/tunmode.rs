//! Tun mode: IP packets from the tun device are turned back into
//! connections and flows, which go through the tunnel.
//!
//! - TCP goes to a userspace TCP stack (ipstack), which terminates each
//!   connection locally; every stream becomes a tunnel channel.
//! - UDP is handled here, before the stack, so a flow's mapping is
//!   endpoint-independent: one tunnel channel per local source port, used for
//!   every destination, with replies from any remote address delivered back.
//!   STUN, WebRTC and QUIC connection migration need that. Port 53 is
//!   answered by the server's resolver.
//! - Anything else (ICMP) is dropped.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use etherparse::PacketBuilder;
use ipstack::{IpStack, IpStackConfig, IpStackStream, TcpConfig};
use sshuttle2_core::fdio::{FdReader, FdWriter};
use sshuttle2_core::mux::ChanTx;
use sshuttle2_core::{Addr, Inbound, Mux, pump};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use crate::tundev::{HEADER_LEN, header_for};

const DNS_TIMEOUT: Duration = Duration::from_secs(10);
const QUEUE: usize = 4096;

/// Packet counters, shown every few seconds with -v.
pub struct Stats {
    pub from_dev: AtomicU64,
    pub to_dev: AtomicU64,
    pub write_errors: AtomicU64,
    pub dropped: AtomicU64,
    pub tcp_streams: AtomicU64,
    pub udp_packets: AtomicU64,
    pub dns_ok: AtomicU64,
    pub dns_failed: AtomicU64,
}

pub static STATS: Stats = Stats {
    from_dev: AtomicU64::new(0),
    to_dev: AtomicU64::new(0),
    write_errors: AtomicU64::new(0),
    dropped: AtomicU64::new(0),
    tcp_streams: AtomicU64::new(0),
    udp_packets: AtomicU64::new(0),
    dns_ok: AtomicU64::new(0),
    dns_failed: AtomicU64::new(0),
};

impl Stats {
    pub fn summary(&self) -> String {
        let g = |a: &AtomicU64| a.load(Relaxed);
        format!(
            "tun: {} packets in, {} out, {} write errors, {} dropped; {} TCP streams, {} UDP packets, DNS {} ok / {} failed",
            g(&self.from_dev),
            g(&self.to_dev),
            g(&self.write_errors),
            g(&self.dropped),
            g(&self.tcp_streams),
            g(&self.udp_packets),
            g(&self.dns_ok),
            g(&self.dns_failed)
        )
    }
}

/// One-line summary of an IPv4 TCP or UDP packet.
pub fn describe(pkt: &[u8]) -> Option<String> {
    if pkt.len() >= 28 && pkt[0] >> 4 == 4 && pkt[9] == 17 {
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        let u = pkt.get(ihl..ihl + 8)?;
        return Some(format!(
            "{}.{}.{}.{}:{} -> {}.{}.{}.{}:{} [UDP] len={}",
            pkt[12],
            pkt[13],
            pkt[14],
            pkt[15],
            u16::from_be_bytes([u[0], u[1]]),
            pkt[16],
            pkt[17],
            pkt[18],
            pkt[19],
            u16::from_be_bytes([u[2], u[3]]),
            u16::from_be_bytes([u[4], u[5]]).saturating_sub(8)
        ));
    }
    if pkt.len() < 40 || pkt[0] >> 4 != 4 || pkt[9] != 6 {
        return None;
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    let t = &pkt[ihl..];
    let f = t[13];
    let names = [
        (0x02, "SYN"),
        (0x10, "ACK"),
        (0x08, "PSH"),
        (0x01, "FIN"),
        (0x04, "RST"),
        (0x40, "ECE"),
        (0x80, "CWR"),
    ];
    let flags: Vec<&str> = names
        .iter()
        .filter(|(b, _)| f & b != 0)
        .map(|(_, n)| *n)
        .collect();
    let doff = (t[12] >> 4) as usize * 4;
    Some(format!(
        "{}.{}.{}.{}:{} -> {}.{}.{}.{}:{} [{}] seq={} ack={} win={} len={}",
        pkt[12],
        pkt[13],
        pkt[14],
        pkt[15],
        u16::from_be_bytes([t[0], t[1]]),
        pkt[16],
        pkt[17],
        pkt[18],
        pkt[19],
        u16::from_be_bytes([t[2], t[3]]),
        flags.join(","),
        u32::from_be_bytes([t[4], t[5], t[6], t[7]]),
        u32::from_be_bytes([t[8], t[9], t[10], t[11]]),
        u16::from_be_bytes([t[14], t[15]]),
        pkt.len().saturating_sub(ihl + doff)
    ))
}

/// `SSHUTTLE2_TUN_TRACE=N` prints the first N TCP packets each way.
static TRACE_LEFT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

fn trace_packet(dir: &str, pkt: &[u8]) {
    if TRACE_LEFT.load(Relaxed) > 0
        && let Some(d) = describe(pkt)
        && TRACE_LEFT.fetch_sub(1, Relaxed) > 0
    {
        tracing::info!("{dir} {d}");
    }
}

fn bump(a: &AtomicU64) {
    a.fetch_add(1, Relaxed);
}

pub struct Opts {
    pub mtu: u16,
    /// The server can reach IPv6. If not, IPv6 connections are refused at
    /// once so apps fall back to IPv4 instead of waiting.
    pub ipv6: bool,
}

/// Runs the packet engine until the device or the tunnel fails.
pub async fn run(dev: OwnedFd, mux: Mux, opts: Opts) -> Result<()> {
    if let Some(n) = std::env::var("SSHUTTLE2_TUN_TRACE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        TRACE_LEFT.store(n, Relaxed);
    }
    let mut rd = FdReader::new(dev.try_clone()?)?;
    let mut wr = FdWriter::new(dev)?;

    // Everything bound for the device goes through one writer.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let writer = tokio::spawn(async move {
        let mut framed = Vec::with_capacity(65_536 + HEADER_LEN);
        while let Some(pkt) = out_rx.recv().await {
            framed.clear();
            if cfg!(target_os = "macos") {
                framed.extend_from_slice(&header_for(pkt.first().copied().unwrap_or(0x45)));
            }
            framed.extend_from_slice(&pkt);
            match wr.write(&framed).await {
                Ok(_) => {
                    bump(&STATS.to_dev);
                    trace_packet("to app  ", &pkt);
                }
                Err(e) => {
                    // A packet the kernel refuses (say, too big) is dropped,
                    // as a router would; only a dead device ends the loop.
                    if STATS.write_errors.fetch_add(1, Relaxed) == 0 {
                        warn!("writing to the tun device: {e}");
                    }
                    if e.raw_os_error() == Some(libc::EBADF) {
                        return;
                    }
                    trace!("tun write: {e}");
                }
            }
        }
    });

    let (stack_tx, stack_rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let mut stack = IpStack::new(
        stack_config(opts.mtu),
        StackDevice {
            rx: stack_rx,
            tx: out_tx.clone(),
            pending: None,
        },
    );
    let tcp_mux = mux.clone();
    let ipv6 = opts.ipv6;
    tokio::spawn(async move {
        while let Ok(s) = stack.accept().await {
            if let IpStackStream::Tcp(s) = s {
                let mux = tcp_mux.clone();
                bump(&STATS.tcp_streams);
                tokio::spawn(async move {
                    let dst = s.peer_addr();
                    if dst.is_ipv6() && !ipv6 {
                        // Dropping the stream resets it: the app falls back
                        // to IPv4 right away.
                        return;
                    }
                    match mux.open_tcp(&Addr::Ip(dst)).await {
                        Ok(chan) => {
                            let (r, w) = tokio::io::split(s);
                            let end = pump(chan, r, w).await;
                            debug!("tcp {dst}: {end:?}");
                        }
                        Err(e) => debug!("tcp {dst}: {e:#}"),
                    }
                });
            }
        }
    });

    let udp = Arc::new(UdpNat {
        mux: mux.clone(),
        out: out_tx.clone(),
        flows: Mutex::default(),
        ipv6,
        mtu: opts.mtu as usize,
    });
    let mut buf = vec![0u8; opts.mtu as usize + HEADER_LEN + 4096];
    let mut reassembler = crate::ipfrag::Reassembler::default();
    let res = loop {
        let n = match rd.read(&mut buf).await {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) => break Err(e).context("reading the tun device"),
        };
        // Shorter than a frame prefix plus an IPv4 header: not a packet.
        if n < HEADER_LEN + 20 {
            continue;
        }
        let pkt = &buf[HEADER_LEN..n];
        bump(&STATS.from_dev);
        trace_packet("from app", pkt);
        // UDP datagrams larger than the MTU arrive as IPv4 fragments.
        let whole;
        let pkt = if crate::ipfrag::is_fragment(pkt) && pkt[9] == 17 {
            match reassembler.add(pkt) {
                Some(p) => {
                    whole = p;
                    &whole[..]
                }
                None => continue,
            }
        } else {
            pkt
        };
        match parse_udp(pkt) {
            Some((src, dst, payload)) => udp.clone().handle(src, dst, payload).await,
            None => {
                // TCP (and anything else) to the stack; drop if it is
                // backed up, as a full router queue would.
                if stack_tx.try_send(pkt.to_vec()).is_err() {
                    bump(&STATS.dropped);
                }
            }
        }
        if mux.is_closed() {
            break Ok(());
        }
    };
    writer.abort();
    res
}

/// The device as ipstack sees it: packets we chose to give it, and its
/// output going to the shared writer.
struct StackDevice {
    rx: mpsc::Receiver<Vec<u8>>,
    tx: mpsc::Sender<Vec<u8>>,
    pending: Option<Vec<u8>>,
}

impl AsyncRead for StackDevice {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let pkt = match self.pending.take() {
            Some(p) => p,
            None => match self.rx.poll_recv(cx) {
                Poll::Ready(Some(p)) => p,
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            },
        };
        if pkt.len() > buf.remaining() {
            // ipstack reads with an MTU-sized buffer; an oversized packet
            // cannot be delivered whole, so drop it.
            trace!(
                "dropping {}-byte packet larger than the stack buffer",
                pkt.len()
            );
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        buf.put_slice(&pkt);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for StackDevice {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // Full queue: drop, and let TCP retransmit.
        let _ = self.tx.try_send(buf.to_vec());
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Source, destination and payload of an unfragmented UDP packet.
fn parse_udp(pkt: &[u8]) -> Option<(SocketAddr, SocketAddr, &[u8])> {
    let (src, dst, l4) = match pkt.first()? >> 4 {
        4 => {
            let ihl = (pkt.first()? & 0x0f) as usize * 4;
            if pkt.len() < 20 || pkt[9] != 17 || ihl < 20 {
                return None;
            }
            // More-fragments flag or a non-zero offset: let it go (rare for UDP).
            if u16::from_be_bytes([pkt[6], pkt[7]]) & 0x3fff != 0 {
                return None;
            }
            let total = (u16::from_be_bytes([pkt[2], pkt[3]]) as usize).min(pkt.len());
            let src = IpAddr::from(<[u8; 4]>::try_from(&pkt[12..16]).ok()?);
            let dst = IpAddr::from(<[u8; 4]>::try_from(&pkt[16..20]).ok()?);
            (src, dst, pkt.get(ihl..total)?)
        }
        6 => {
            if pkt.len() < 40 || pkt[6] != 17 {
                return None;
            }
            let total = (40 + u16::from_be_bytes([pkt[4], pkt[5]]) as usize).min(pkt.len());
            let src = IpAddr::from(<[u8; 16]>::try_from(&pkt[8..24]).ok()?);
            let dst = IpAddr::from(<[u8; 16]>::try_from(&pkt[24..40]).ok()?);
            (src, dst, pkt.get(40..total)?)
        }
        _ => return None,
    };
    if l4.len() < 8 {
        return None;
    }
    let sport = u16::from_be_bytes([l4[0], l4[1]]);
    let dport = u16::from_be_bytes([l4[2], l4[3]]);
    let ulen = (u16::from_be_bytes([l4[4], l4[5]]) as usize).clamp(8, l4.len());
    Some((
        SocketAddr::new(src, sport),
        SocketAddr::new(dst, dport),
        &l4[8..ulen],
    ))
}

/// The stack exactly as `run` configures it, over the given channels.
pub fn test_stack(rx: mpsc::Receiver<Vec<u8>>, tx: mpsc::Sender<Vec<u8>>, mtu: u16) -> IpStack {
    IpStack::new(
        stack_config(mtu),
        StackDevice {
            rx,
            tx,
            pending: None,
        },
    )
}

const READ_BUFFER: usize = 256 * 1024;

fn stack_config(mtu: u16) -> IpStackConfig {
    let mut cfg = IpStackConfig::default();
    cfg.mtu_unchecked(mtu);
    let mut tcp = TcpConfig::default();
    // Defaults (16 KiB) would cap each connection far below the link.
    tcp.max_unacked_bytes = 1 << 20;
    // What an app may send ahead of the tunnel. The window closes once this
    // much waits unread (third_party/ipstack/PATCHES.md, "Receive window").
    tcp.read_buffer_size = READ_BUFFER;
    // An idle timeout: long-lived quiet connections (ssh, websockets) must
    // survive, as they would through a NAT.
    tcp.timeout = Duration::from_secs(2 * 3600);
    // Announce our segment size: without it macOS assumes 512 bytes.
    tcp.options = Some(vec![ipstack::TcpOptions::MaximumSegmentSize(
        mtu.saturating_sub(40),
    )]);
    cfg.with_tcp_config(tcp);
    cfg
}

pub fn build_udp_for_test(from: SocketAddr, to: SocketAddr, payload: &[u8]) -> Option<Vec<u8>> {
    build_udp(from, to, payload)
}

/// Builds the IP+UDP packet that delivers `payload` from `from` to `to`.
fn build_udp(from: SocketAddr, to: SocketAddr, payload: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(payload.len() + 48);
    let res = match (from.ip(), to.ip()) {
        (IpAddr::V4(f), IpAddr::V4(t)) => PacketBuilder::ipv4(f.octets(), t.octets(), 64)
            .udp(from.port(), to.port())
            .write(&mut out, payload),
        (IpAddr::V6(f), IpAddr::V6(t)) => PacketBuilder::ipv6(f.octets(), t.octets(), 64)
            .udp(from.port(), to.port())
            .write(&mut out, payload),
        // Our flows are per family, so this does not happen.
        _ => return None,
    };
    res.ok()?;
    Some(out)
}

struct UdpNat {
    mux: Mux,
    out: mpsc::Sender<Vec<u8>>,
    /// Local source → its tunnel channel.
    flows: Mutex<HashMap<SocketAddr, ChanTx>>,
    ipv6: bool,
    mtu: usize,
}

impl UdpNat {
    /// Queues a reply, split into fragments if it exceeds the MTU.
    async fn deliver(&self, pkt: Vec<u8>) {
        for f in crate::ipfrag::fragment(pkt, self.mtu) {
            let _ = self.out.send(f).await;
        }
    }
}

impl UdpNat {
    async fn handle(self: Arc<Self>, src: SocketAddr, dst: SocketAddr, payload: &[u8]) {
        if src.is_ipv6() && !self.ipv6 {
            return;
        }
        bump(&STATS.udp_packets);
        if dst.port() == 53 {
            let query = Bytes::copy_from_slice(payload);
            let me = self.clone();
            tokio::spawn(async move {
                match me.mux.dns(query, DNS_TIMEOUT).await {
                    // The answer appears to come from the server the app asked.
                    Ok(resp) => {
                        bump(&STATS.dns_ok);
                        if let Some(p) = build_udp(dst, src, &resp) {
                            me.deliver(p).await;
                        }
                    }
                    Err(e) => {
                        bump(&STATS.dns_failed);
                        debug!("DNS from {src}: {e:#}");
                    }
                }
            });
            return;
        }
        let existing = self.flows.lock().unwrap().get(&src).cloned();
        let tx = match existing {
            Some(tx) => tx,
            None => match self.open(src).await {
                Some(tx) => tx,
                None => return,
            },
        };
        tx.send_udp(&Addr::Ip(dst), payload);
    }

    /// Opens a channel for a new local source and starts delivering replies.
    async fn open(self: &Arc<Self>, src: SocketAddr) -> Option<ChanTx> {
        let chan = match self.mux.open_udp(src.is_ipv6()).await {
            Ok(c) => c,
            Err(e) => {
                warn!("UDP from {src}: {e:#}");
                return None;
            }
        };
        let (tx, mut rx) = chan.split();
        self.flows.lock().unwrap().insert(src, tx.clone());
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Some(Inbound::Udp(Addr::Ip(from), data)) => {
                        rx.udp_consumed(data.len());
                        if let Some(p) = build_udp(from, src, &data) {
                            me.deliver(p).await;
                        }
                    }
                    Some(Inbound::Udp(..)) => {}
                    // The server ends idle flows; the next packet reopens.
                    _ => break,
                }
            }
            if let Some(tx) = me.flows.lock().unwrap().remove(&src) {
                tx.mark_finished();
            }
        });
        Some(tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_round_trip_v4() {
        let a: SocketAddr = "10.1.2.3:5353".parse().unwrap();
        let b: SocketAddr = "1.1.1.1:443".parse().unwrap();
        let pkt = build_udp(a, b, b"hello").unwrap();
        let (src, dst, payload) = parse_udp(&pkt).unwrap();
        assert_eq!((src, dst, payload), (a, b, &b"hello"[..]));
    }

    #[test]
    fn udp_round_trip_v6() {
        let a: SocketAddr = "[fd00::1]:5000".parse().unwrap();
        let b: SocketAddr = "[2001:db8::2]:3478".parse().unwrap();
        let pkt = build_udp(a, b, &[7u8; 1200]).unwrap();
        let (src, dst, payload) = parse_udp(&pkt).unwrap();
        assert_eq!((src, dst, payload.len()), (a, b, 1200));
    }

    #[test]
    fn ignores_tcp_and_fragments() {
        let a: SocketAddr = "10.1.2.3:5353".parse().unwrap();
        let b: SocketAddr = "1.1.1.1:443".parse().unwrap();
        let mut pkt = build_udp(a, b, b"x").unwrap();
        pkt[9] = 6; // TCP
        assert!(parse_udp(&pkt).is_none());
        let mut pkt = build_udp(a, b, b"x").unwrap();
        pkt[6] = 0x20; // more fragments
        assert!(parse_udp(&pkt).is_none());
    }

    /// The app's end of one TCP connection to the stack, written by hand: it
    /// plays the kernel on the far side of the tun device.
    struct App {
        to_stack: mpsc::Sender<Vec<u8>>,
        from_stack: mpsc::Receiver<Vec<u8>>,
        /// Next sequence number to send.
        seq: u32,
        /// Next sequence number expected from the stack.
        ack: u32,
    }

    struct Segment {
        seq: u32,
        window: u16,
        syn: bool,
        fin: bool,
        rst: bool,
        payload: Vec<u8>,
    }

    impl App {
        async fn send(&mut self, syn: bool, fin: bool, payload: &[u8]) {
            let b = PacketBuilder::ipv4([10, 0, 0, 2], [10, 0, 0, 1], 64).tcp(
                40000,
                80,
                self.seq,
                u16::MAX,
            );
            let b = if syn { b.syn() } else { b.ack(self.ack) };
            let b = if payload.is_empty() { b } else { b.psh() };
            let b = if fin { b.fin() } else { b };
            let mut pkt = Vec::new();
            b.write(&mut pkt, payload).unwrap();
            self.to_stack.send(pkt).await.unwrap();
            let len = payload.len() as u32 + u32::from(syn) + u32::from(fin);
            self.seq = self.seq.wrapping_add(len);
        }

        fn parse(pkt: &[u8]) -> Segment {
            let p = etherparse::SlicedPacket::from_ip(pkt).unwrap();
            let Some(etherparse::TransportSlice::Tcp(t)) = p.transport else {
                panic!("the stack sent a packet that is not TCP");
            };
            Segment {
                seq: t.sequence_number(),
                window: t.window_size(),
                syn: t.syn(),
                fin: t.fin(),
                rst: t.rst(),
                payload: t.payload().to_vec(),
            }
        }

        async fn recv(&mut self) -> Segment {
            let pkt = tokio::time::timeout(Duration::from_secs(5), self.from_stack.recv())
                .await
                .expect("the stack went quiet")
                .unwrap();
            Self::parse(&pkt)
        }

        fn try_recv(&mut self) -> Option<Segment> {
            self.from_stack.try_recv().ok().map(|p| Self::parse(&p))
        }
    }

    /// A connection to a fresh stack: the app's end, the stack (which must
    /// stay alive) and the stream.
    async fn connect(mtu: u16) -> (App, IpStack, ipstack::IpStackTcpStream) {
        let (to_stack, rx) = mpsc::channel(QUEUE);
        let (tx, from_stack) = mpsc::channel(QUEUE);
        let mut stack = test_stack(rx, tx, mtu);
        let mut app = App {
            to_stack,
            from_stack,
            seq: 1000,
            ack: 0,
        };
        app.send(true, false, &[]).await;
        let syn_ack = app.recv().await;
        assert!(syn_ack.syn);
        app.ack = syn_ack.seq.wrapping_add(1);
        app.send(false, false, &[]).await;
        let Ok(IpStackStream::Tcp(s)) = stack.accept().await else {
            panic!("no TCP stream");
        };
        (app, stack, s)
    }

    /// The app sends `n` bytes and its FIN, then reads the echo, which must
    /// all arrive before the stack's FIN. The upper layer behaves as `pump`
    /// does: it writes everything, then shuts down. `store_and_forward` makes
    /// it read to EOF before writing anything, as a request/response server.
    async fn echo_after_half_close(
        mtu: u16,
        n: usize,
        fin_with_data: bool,
        store_and_forward: bool,
    ) {
        let (mut app, _stack, s) = connect(mtu).await;
        let echo = tokio::spawn(async move {
            let (mut r, mut w) = tokio::io::split(s);
            if store_and_forward {
                let mut all = Vec::new();
                r.read_to_end(&mut all).await?;
                w.write_all(&all).await?;
            } else {
                tokio::io::copy(&mut r, &mut w).await?;
            }
            w.shutdown().await
        });

        let mut x = 0x9e37_79b9u32;
        let data: Vec<u8> = (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let mss = usize::from(mtu) - 40;
        let mut sent = 0;
        let mut got = Vec::new();
        loop {
            if sent < n {
                let end = (sent + mss).min(n);
                // Linux puts the FIN on the last data segment if that is
                // still queued when the app shuts down.
                app.send(false, end == n && fin_with_data, &data[sent..end])
                    .await;
                sent = end;
                if sent == n && !fin_with_data {
                    app.send(false, true, &[]).await;
                }
            }
            let seg = if sent < n {
                match app.try_recv() {
                    Some(seg) => seg,
                    None => continue,
                }
            } else {
                app.recv().await
            };
            assert!(!seg.rst, "the stack reset the connection");
            if seg.seq == app.ack {
                got.extend_from_slice(&seg.payload);
                app.ack = app.ack.wrapping_add(seg.payload.len() as u32);
                if seg.fin {
                    assert_eq!(got.len(), n, "the stack sent its FIN before the whole echo");
                    app.ack = app.ack.wrapping_add(1);
                    app.send(false, false, &[]).await;
                    break;
                }
            }
            if !seg.payload.is_empty() {
                app.send(false, false, &[]).await;
            }
        }
        assert!(got == data, "the echo differs from what was sent");
        // The app acknowledged the FIN: the upper layer's shutdown completes.
        tokio::time::timeout(Duration::from_secs(5), echo)
            .await
            .expect("shutdown did not complete")
            .unwrap()
            .unwrap();
    }

    // The stream's Drop blocks in place, which needs the multi-thread runtime.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn half_close_then_response() {
        echo_after_half_close(1500, 1000, false, true).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn half_close_fin_on_last_segment() {
        echo_after_half_close(1500, 300_000, true, false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn half_close_jumbo_segments() {
        echo_after_half_close(9000, 300_000, false, false).await;
    }

    /// An app that uploads faster than the upper layer reads must be held back
    /// by the window, and told when there is room again. Before, the stack
    /// acknowledged everything with a full window and queued it unread: an
    /// upload through a slow tunnel grew the client by gigabytes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn window_closes_until_the_upper_layer_reads() {
        let (mut app, _stack, mut s) = connect(1500).await;
        let mss = 1460;
        let chunk = vec![7u8; mss];
        // Send as a kernel would: only within the window.
        let mut window = usize::from(u16::MAX);
        let mut sent = 0;
        while window >= mss {
            app.send(false, false, &chunk).await;
            sent += mss;
            assert!(sent <= 2 * READ_BUFFER, "the window never closed");
            let ack = app.recv().await;
            assert!(!ack.rst);
            window = usize::from(ack.window);
        }
        assert!(
            sent <= READ_BUFFER + mss,
            "{sent} bytes accepted with a {READ_BUFFER}-byte buffer"
        );
        assert_eq!(window, 0, "a window under one segment should be 0");
        // Nothing read, so nothing more is announced.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(app.try_recv().is_none(), "the stack spoke without reading");
        // Reading makes room, and the stack says so by itself.
        let mut buf = vec![0u8; 64 * 1024];
        s.read_exact(&mut buf).await.unwrap();
        let update = app.recv().await;
        assert!(
            usize::from(update.window) >= 2 * mss,
            "window update too small: {}",
            update.window
        );
        assert_eq!(update.seq, app.ack, "a window update carries no data");
    }

    /// The stack no longer sends a FIN of its own after the app's, so an upper
    /// layer that gives up must reset the connection, or the app waits forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_stream_resets_half_closed_app() {
        let (mut app, _stack, s) = connect(1500).await;
        app.send(false, true, &[]).await;
        let ack = app.recv().await;
        assert!(
            !ack.fin && !ack.rst,
            "the stack should only acknowledge the FIN"
        );
        drop(s);
        assert!(app.recv().await.rst, "dropping the stream should reset it");
    }
}
