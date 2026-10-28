#![no_main]
//! Packets from local apps into the user-space TCP/IP stack, configured as
//! tun mode configures it. Sequence and acknowledgment numbers are offsets
//! from the stack's own, learned from what it sends back, so inputs can get
//! past the handshake.
//!
//! A multi-threaded runtime, as in the client: a dropped TCP stream waits for
//! its session task with `block_in_place`, which a current-thread runtime
//! does not allow. Streams are dropped before the runtime shuts down.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use etherparse::{PacketBuilder, SlicedPacket, TcpOptionElement, TransportSlice};
use ipstack::{IpStack, IpStackConfig, IpStackStream, TcpConfig, TcpOptions};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{mpsc, watch};

const MTU: u16 = 1500;
const OUR_ISN: u32 = 1_000_000;

#[derive(Arbitrary, Debug)]
enum Pkt {
    Tcp {
        v6: bool,
        port: u8,
        flags: u8,
        options: u8,
        seq: u16,
        ack: u16,
        window: u16,
        payload: Vec<u8>,
    },
    Raw(Vec<u8>),
}

struct Dev {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl AsyncRead for Dev {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(p)) => {
                let n = p.len().min(buf.remaining());
                buf.put_slice(&p[..n]);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for Dev {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let _ = self.tx.send(buf.to_vec());
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// As `stack_config` in crates/client/src/tunmode.rs.
fn config() -> IpStackConfig {
    let mut cfg = IpStackConfig::default();
    cfg.mtu_unchecked(MTU);
    let mut tcp = TcpConfig::default();
    tcp.max_unacked_bytes = 1 << 20;
    tcp.read_buffer_size = 256 * 1024;
    tcp.timeout = Duration::from_secs(2 * 3600);
    tcp.options = Some(vec![TcpOptions::MaximumSegmentSize(MTU - 40)]);
    cfg.with_tcp_config(tcp);
    cfg
}

fn build(p: &Pkt, isn: &HashMap<u16, u32>) -> Vec<u8> {
    let Pkt::Tcp {
        v6,
        port,
        flags,
        options,
        seq,
        ack,
        window,
        payload,
    } = p
    else {
        let Pkt::Raw(raw) = p else { unreachable!() };
        return raw.clone();
    };
    let sport = 1000 + (*port % 4) as u16;
    let b = if *v6 {
        PacketBuilder::ipv6([0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], [0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2], 64)
    } else {
        PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
    };
    let mut t = b.tcp(sport, 80, OUR_ISN.wrapping_add(*seq as u32), *window);
    if flags & 0x01 != 0 {
        t = t.fin();
    }
    if flags & 0x02 != 0 {
        t = t.syn();
    }
    if flags & 0x04 != 0 {
        t = t.rst();
    }
    if flags & 0x08 != 0 {
        t = t.psh();
    }
    if flags & 0x10 != 0 {
        let base = isn.get(&sport).copied().unwrap_or(0);
        t = t.ack(base.wrapping_add(*ack as u32));
    }
    if flags & 0x20 != 0 {
        t = t.urg(0);
    }
    if flags & 0x40 != 0 {
        t = t.ece();
    }
    if flags & 0x80 != 0 {
        t = t.cwr();
    }
    let mut opts = Vec::new();
    if options & 1 != 0 {
        opts.push(TcpOptionElement::MaximumSegmentSize(*window));
    }
    if options & 2 != 0 {
        opts.push(TcpOptionElement::WindowScale(options >> 4));
    }
    if options & 4 != 0 {
        opts.push(TcpOptionElement::SelectiveAcknowledgementPermitted);
    }
    if !opts.is_empty() {
        t = match t.options(&opts) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
    }
    let payload = &payload[..payload.len().min(MTU as usize - 80)];
    let mut out = Vec::new();
    let _ = t.write(&mut out, payload);
    out
}

/// Records the stack's initial sequence number from its SYN-ACKs.
fn learn(pkt: &[u8], isn: &mut HashMap<u16, u32>) {
    let Ok(s) = SlicedPacket::from_ip(pkt) else {
        return;
    };
    if let Some(TransportSlice::Tcp(t)) = s.transport {
        if t.syn() {
            isn.insert(t.destination_port(), t.sequence_number().wrapping_add(1));
        }
    }
}

/// Plays the upper layer: echoes a little of what it reads, until told to
/// stop.
async fn serve(s: IpStackStream, mut stop: watch::Receiver<bool>) {
    let work = async {
        match s {
            IpStackStream::Tcp(mut t) => {
                let mut b = [0u8; 4096];
                while let Ok(n) = t.read(&mut b).await {
                    if n == 0 || t.write_all(&b[..n.min(64)]).await.is_err() {
                        break;
                    }
                }
                let _ = t.shutdown().await;
            }
            IpStackStream::Udp(mut u) => {
                let mut b = [0u8; 2048];
                if let Ok(n) = u.read(&mut b).await {
                    let _ = u.write_all(&b[..n]).await;
                }
            }
            _ => {}
        }
    };
    tokio::select! {
        _ = work => {}
        _ = stop.changed() => {}
    }
}

async fn run(pkts: Vec<Pkt>) {
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let mut stack = IpStack::new(config(), Dev { rx: in_rx, tx: out_tx });
    let (stop_tx, stop) = watch::channel(false);
    let mut stop_accept = stop.clone();
    let acceptor = tokio::spawn(async move {
        let mut streams = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                s = stack.accept() => match s {
                    Ok(s) => {
                        streams.spawn(serve(s, stop.clone()));
                    }
                    Err(_) => break,
                },
                _ = stop_accept.changed() => break,
            }
        }
        while streams.join_next().await.is_some() {}
    });
    let mut isn = HashMap::new();
    for p in pkts.iter().take(64) {
        let pkt = build(p, &isn);
        if pkt.is_empty() {
            continue;
        }
        let _ = in_tx.send(pkt);
        tokio::time::sleep(Duration::from_micros(200)).await;
        while let Ok(out) = out_rx.try_recv() {
            learn(&out, &mut isn);
        }
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
    let _ = stop_tx.send(true);
    let _ = acceptor.await;
    drop(in_tx);
}

fuzz_target!(|pkts: Vec<Pkt>| {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(run(pkts));
    rt.shutdown_timeout(Duration::from_secs(1));
});
