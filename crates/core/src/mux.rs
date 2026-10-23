//! The stream multiplexer: many TCP connections, UDP flows and DNS queries
//! over one byte stream (the ssh session).
//!
//! Tasks per mux:
//! - **reader** parses frames and hands each to its channel. It never waits on
//!   a consumer: a channel's queue is bounded by the credit its receiver
//!   granted, so the queue can always take what the peer is allowed to send.
//! - **writer** batches queued frames into large writes. Control frames
//!   (WINDOW, PONG, RST, PING) go first; DATA and everything whose order
//!   matters relative to DATA (EOF, CONNECT_OK) share one bounded FIFO.
//! - **pinger** measures round-trip time, which drives window autotuning.
//!
//! Flow control is per channel, so one slow consumer stalls only its own
//! channel. This replaces sshuttle's global "latency control" brake.

use std::collections::HashMap;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, mpsc, watch};
use tracing::{debug, trace, warn};

use crate::addr::Addr;
use crate::frame::{self, ConnectError, DATA_CHUNK, Frame, Kind};

#[derive(Clone, Debug)]
pub struct MuxConfig {
    /// Receive window each new TCP channel starts with.
    pub initial_window: u32,
    /// Ceiling for autotuned windows.
    pub max_window: u32,
    /// Ceiling on the sum of all receive windows, which bounds how much
    /// received data this side may have to buffer.
    pub global_window_cap: u64,
    /// Frames the shared DATA queue holds before senders wait. Kept small so
    /// interactive channels are not stuck behind a long queue.
    pub data_queue: usize,
    /// Bytes of UDP payload a channel may have queued before datagrams drop.
    pub udp_buffer: u32,
    /// Smallest window the brake shrinks a channel to.
    pub min_window: u32,
    /// The brake counts every round trip as at least this long, so delay
    /// jitter on fast paths (Wi-Fi) never brakes a channel.
    pub brake_rtt: Duration,
    pub ping_interval: Duration,
    /// PING interval while this side receives DATA, so the RTT follows the
    /// queue as it builds and drains.
    pub busy_ping_interval: Duration,
}

impl Default for MuxConfig {
    fn default() -> Self {
        MuxConfig {
            initial_window: 256 * 1024,
            max_window: 16 * 1024 * 1024,
            global_window_cap: 256 * 1024 * 1024,
            data_queue: 16,
            udp_buffer: 1024 * 1024,
            min_window: 8 * 1024,
            brake_rtt: Duration::from_millis(100),
            ping_interval: Duration::from_secs(1),
            busy_ping_interval: Duration::from_millis(250),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Opens channels (allocates ids).
    Client,
    /// Accepts channels opened by the client.
    Server,
}

/// What a channel's owner receives from the peer.
#[derive(Debug)]
pub enum Inbound {
    Data(Bytes),
    Eof,
    Rst,
    ConnectOk,
    ConnectErr(ConnectError, String),
    /// UDP datagram: address (destination outbound, source inbound) + payload.
    Udp(Addr, Bytes),
    UdpClose,
    Dns(Bytes),
}

/// A channel the peer opened (server side).
pub enum Open {
    Tcp { chan: Channel, addr: Addr },
    Udp { chan: Channel, ipv6: bool },
    Dns { chan: Channel, query: Bytes },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChanKind {
    Tcp,
    Udp,
    Dns,
}

/// Bytes this side may still send on a channel.
struct Credit {
    avail: AtomicU64,
    closed: AtomicBool,
    notify: Notify,
}

impl Credit {
    fn new(n: u64) -> Self {
        Credit {
            avail: AtomicU64::new(n),
            closed: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn add(&self, n: u64) {
        self.avail.fetch_add(n, AcqRel);
        self.notify.notify_one();
    }

    fn close(&self) {
        self.closed.store(true, Release);
        self.notify.notify_one();
    }

    /// Waits until some credit is available. `None` once the channel is reset
    /// or the mux is gone. Only one task (the channel's sender) waits.
    async fn wait(&self) -> Option<u64> {
        loop {
            if self.closed.load(Acquire) {
                return None;
            }
            let n = self.avail.load(Acquire);
            if n > 0 {
                return Some(n);
            }
            self.notify.notified().await;
        }
    }

    fn consume(&self, n: u64) {
        self.avail.fetch_sub(n, AcqRel);
    }
}

struct ChanEntry {
    tx: mpsc::UnboundedSender<Inbound>,
    credit: Arc<Credit>,
    /// Bytes the peer may still send us before overrunning what we granted.
    recv_allow: Arc<AtomicI64>,
    kind: ChanKind,
}

#[derive(Default)]
pub struct Stats {
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    pub channels_opened: AtomicU64,
}

struct Inner {
    role: Role,
    cfg: MuxConfig,
    ctrl_tx: mpsc::UnboundedSender<Frame>,
    data_tx: mpsc::Sender<Frame>,
    channels: Mutex<HashMap<u32, ChanEntry>>,
    next_id: AtomicU32,
    start: Instant,
    rtt: Mutex<RttFilter>,
    total_window: AtomicU64,
    closed: watch::Sender<Option<String>>,
    stats: Stats,
}

impl Inner {
    fn is_closed(&self) -> bool {
        self.closed.borrow().is_some()
    }

    fn rtt(&self) -> Duration {
        self.rtt.lock().unwrap().smoothed()
    }

    /// The smallest round trip seen, and the standing one (the smallest of
    /// the last second's samples: a queue that stays, not jitter).
    fn rtt_view(&self) -> (Duration, Duration) {
        let now = self.start.elapsed().as_micros() as u64;
        let rtt = self.rtt.lock().unwrap();
        (rtt.min(), rtt.standing(now))
    }

    fn send_ctrl(&self, f: Frame) {
        let _ = self.ctrl_tx.send(f);
    }

    /// Marks the mux dead and wakes every channel so its owner notices.
    fn shutdown(&self, reason: String) {
        let first = self.closed.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason.clone());
                true
            } else {
                false
            }
        });
        if !first {
            return;
        }
        debug!("mux closed: {reason}");
        let channels = self.channels.lock().unwrap();
        for e in channels.values() {
            let _ = e.tx.send(Inbound::Rst);
            e.credit.close();
        }
    }

    /// Registers a channel. `id` is chosen by the client.
    fn register(self: &Arc<Self>, id: u32, kind: ChanKind) -> Result<Channel> {
        if self.is_closed() {
            bail!("tunnel is closed");
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let (send_credit, recv_window) = match kind {
            ChanKind::Tcp => (self.cfg.initial_window, self.cfg.initial_window),
            ChanKind::Udp => (0, self.cfg.udp_buffer),
            ChanKind::Dns => (0, frame::MAX_PAYLOAD as u32),
        };
        let credit = Arc::new(Credit::new(send_credit as u64));
        let recv_allow = Arc::new(AtomicI64::new(recv_window as i64));
        {
            let mut channels = self.channels.lock().unwrap();
            if channels.contains_key(&id) {
                bail!("channel {id} already open");
            }
            channels.insert(
                id,
                ChanEntry {
                    tx,
                    credit: credit.clone(),
                    recv_allow: recv_allow.clone(),
                    kind,
                },
            );
        }
        if kind == ChanKind::Tcp {
            self.total_window.fetch_add(recv_window as u64, Relaxed);
        }
        self.stats.channels_opened.fetch_add(1, Relaxed);
        let shared = Arc::new(ChanShared {
            id,
            inner: self.clone(),
            credit,
            recv_allow,
            kind,
            finished: AtomicBool::new(false),
            window: AtomicU64::new(if kind == ChanKind::Tcp {
                recv_window as u64
            } else {
                0
            }),
        });
        Ok(Channel {
            tx: ChanTx {
                shared: shared.clone(),
            },
            rx: ChanRx {
                shared,
                rx,
                window: recv_window,
                consumed: 0,
                last_grant: Instant::now(),
                paced_bytes: 0.0,
                paced_secs: 0.0,
            },
        })
    }

    fn dispatch(self: &Arc<Self>, f: Frame, opens: &mpsc::UnboundedSender<Open>) -> Result<()> {
        trace!(kind = ?f.kind, chan = f.chan, len = f.payload.len(), "<");
        match f.kind {
            Kind::Ping => self.send_ctrl(Frame::new(Kind::Pong, 0, f.payload)),
            Kind::Pong => {
                let sent = frame::parse_u64(&f.payload)?;
                let now = self.start.elapsed().as_micros() as u64;
                let sample = now.saturating_sub(sent).max(1);
                let mut rtt = self.rtt.lock().unwrap();
                rtt.answered(sent);
                rtt.add(now, sample);
            }
            Kind::Exit => bail!("peer said goodbye"),
            Kind::Hello => bail!("unexpected HELLO after handshake"),
            Kind::TcpConnect | Kind::UdpOpen | Kind::DnsReq => {
                if self.role != Role::Server {
                    bail!("peer tried to open channel {}", f.chan);
                }
                let mut payload = f.payload;
                let (kind, mk): (ChanKind, Box<dyn FnOnce(Channel) -> Open>) = match f.kind {
                    Kind::TcpConnect => {
                        let addr = Addr::decode(&mut payload)?;
                        (
                            ChanKind::Tcp,
                            Box::new(move |chan| Open::Tcp { chan, addr }),
                        )
                    }
                    Kind::UdpOpen => {
                        let ipv6 = payload.first() == Some(&6);
                        (
                            ChanKind::Udp,
                            Box::new(move |chan| Open::Udp { chan, ipv6 }),
                        )
                    }
                    _ => (
                        ChanKind::Dns,
                        Box::new(move |chan| Open::Dns {
                            chan,
                            query: payload,
                        }),
                    ),
                };
                let chan = self.register(f.chan, kind)?;
                let _ = opens.send(mk(chan));
            }
            Kind::Window => {
                let n = frame::parse_u32(&f.payload)?;
                if let Some(e) = self.channels.lock().unwrap().get(&f.chan) {
                    e.credit.add(n as u64);
                }
            }
            Kind::Data => {
                let len = f.payload.len() as i64;
                self.stats.bytes_down.fetch_add(len as u64, Relaxed);
                let channels = self.channels.lock().unwrap();
                let Some(e) = channels.get(&f.chan) else {
                    return Ok(());
                };
                if e.recv_allow.fetch_sub(len, AcqRel) < len {
                    // The peer sent more than we granted. Its accounting is
                    // broken for this channel; reset it rather than buffer
                    // without limit.
                    warn!(
                        chan = f.chan,
                        "peer overran its flow-control window; resetting"
                    );
                    let _ = e.tx.send(Inbound::Rst);
                    e.credit.close();
                    self.send_ctrl(Frame::empty(Kind::Rst, f.chan));
                } else {
                    let _ = e.tx.send(Inbound::Data(f.payload));
                }
            }
            Kind::UdpData => {
                let len = f.payload.len() as i64;
                let channels = self.channels.lock().unwrap();
                let Some(e) = channels.get(&f.chan) else {
                    return Ok(());
                };
                if e.kind != ChanKind::Udp {
                    return Ok(());
                }
                if e.recv_allow.fetch_sub(len, AcqRel) < len {
                    // Consumer is behind: drop the datagram, as a router would.
                    e.recv_allow.fetch_add(len, AcqRel);
                    trace!(chan = f.chan, "UDP queue full; dropping datagram");
                    return Ok(());
                }
                let mut payload = f.payload;
                match Addr::decode(&mut payload) {
                    Ok(addr) => {
                        let _ = e.tx.send(Inbound::Udp(addr, payload));
                    }
                    Err(err) => {
                        e.recv_allow.fetch_add(len, AcqRel);
                        debug!(chan = f.chan, "bad UDP frame: {err}");
                    }
                }
            }
            Kind::Eof | Kind::Rst | Kind::ConnectOk | Kind::ConnectErr | Kind::UdpClose => {
                let channels = self.channels.lock().unwrap();
                let Some(e) = channels.get(&f.chan) else {
                    return Ok(());
                };
                let msg = match f.kind {
                    Kind::Eof => Inbound::Eof,
                    Kind::ConnectOk => Inbound::ConnectOk,
                    Kind::UdpClose => Inbound::UdpClose,
                    Kind::ConnectErr => {
                        let (code, msg) = frame::parse_connect_err(&f.payload);
                        e.credit.close();
                        Inbound::ConnectErr(code, msg)
                    }
                    _ => {
                        e.credit.close();
                        Inbound::Rst
                    }
                };
                let _ = e.tx.send(msg);
            }
            Kind::DnsResp => {
                let channels = self.channels.lock().unwrap();
                if let Some(e) = channels.get(&f.chan) {
                    let _ = e.tx.send(Inbound::Dns(f.payload));
                }
            }
        }
        Ok(())
    }
}

/// Round-trip time from PING/PONG. The minimum is kept over two 30-second
/// epochs, so it follows route changes but ignores queueing delay.
#[derive(Default)]
struct RttFilter {
    smoothed_us: u64,
    min_cur_us: u64,
    min_prev_us: u64,
    epoch_start_us: u64,
    /// The last few samples as (arrival, rtt), newest at `next - 1`.
    recent: [(u64, u64); 8],
    next: usize,
    /// Stamps of PINGs not yet answered, oldest first.
    waiting: std::collections::VecDeque<u64>,
}

impl RttFilter {
    const EPOCH_US: u64 = 30_000_000;
    const STANDING_US: u64 = 1_000_000;
    const DEFAULT: Duration = Duration::from_millis(100);

    fn add(&mut self, now_us: u64, sample_us: u64) {
        self.recent[self.next % self.recent.len()] = (now_us, sample_us);
        self.next += 1;
        self.smoothed_us = if self.smoothed_us == 0 {
            sample_us
        } else {
            (self.smoothed_us * 7 + sample_us) / 8
        };
        if self.min_cur_us == 0 || now_us - self.epoch_start_us > Self::EPOCH_US {
            self.min_prev_us = self.min_cur_us;
            self.min_cur_us = sample_us;
            self.epoch_start_us = now_us;
        } else {
            self.min_cur_us = self.min_cur_us.min(sample_us);
        }
    }

    fn smoothed(&self) -> Duration {
        match self.smoothed_us {
            0 => Self::DEFAULT,
            us => Duration::from_micros(us),
        }
    }

    fn min(&self) -> Duration {
        let us = match (self.min_cur_us, self.min_prev_us) {
            (0, 0) => return Self::DEFAULT,
            (a, 0) | (0, a) => a,
            (a, b) => a.min(b),
        };
        Duration::from_micros(us)
    }

    /// The smallest sample of the last second, or the latest sample if
    /// none is that recent. A queue that persists raises every sample;
    /// jitter raises only some.
    ///
    /// Behind a queue of seconds no answer arrives at all, so a PING
    /// unanswered for over a second also counts: the round trip is at least
    /// that long.
    fn standing(&self, now_us: u64) -> Duration {
        let n = self.next.min(self.recent.len());
        let mut us = if n == 0 {
            Self::DEFAULT.as_micros() as u64
        } else {
            let latest = self.recent[(self.next - 1) % self.recent.len()].1;
            self.recent[..n]
                .iter()
                .filter(|(at, _)| now_us.saturating_sub(*at) <= Self::STANDING_US)
                .map(|(_, rtt)| *rtt)
                .min()
                .unwrap_or(latest)
        };
        let waited = self
            .waiting
            .front()
            .map_or(0, |&t| now_us.saturating_sub(t));
        if waited > Self::STANDING_US {
            us = us.max(waited);
        }
        Duration::from_micros(us)
    }

    fn sent(&mut self, stamp_us: u64) {
        if self.waiting.len() < 64 {
            self.waiting.push_back(stamp_us);
        }
    }

    fn answered(&mut self, stamp_us: u64) {
        while self.waiting.front().is_some_and(|&t| t <= stamp_us) {
            self.waiting.pop_front();
        }
    }
}

/// State both halves of a channel share. Dropping the last reference
/// unregisters the channel, and resets it at the peer unless it finished.
struct ChanShared {
    id: u32,
    inner: Arc<Inner>,
    credit: Arc<Credit>,
    recv_allow: Arc<AtomicI64>,
    kind: ChanKind,
    /// Set once the peer needs no RST: clean close, or the peer reset first.
    finished: AtomicBool,
    /// Current receive window (TCP), for the global cap.
    window: AtomicU64,
}

impl Drop for ChanShared {
    fn drop(&mut self) {
        self.inner.channels.lock().unwrap().remove(&self.id);
        self.inner
            .total_window
            .fetch_sub(self.window.load(Relaxed), Relaxed);
        if !self.finished.load(Acquire) && !self.inner.is_closed() {
            let kind = if self.kind == ChanKind::Udp {
                Kind::UdpClose
            } else {
                Kind::Rst
            };
            self.inner.send_ctrl(Frame::empty(kind, self.id));
        }
    }
}

/// A multiplexed channel. Split it to drive both directions concurrently.
pub struct Channel {
    pub tx: ChanTx,
    pub rx: ChanRx,
}

impl Channel {
    pub fn id(&self) -> u32 {
        self.tx.shared.id
    }

    pub fn split(self) -> (ChanTx, ChanRx) {
        (self.tx, self.rx)
    }

    /// For TCP channels opened by the client: waits for the server's verdict
    /// on the connect. Used where the local side needs an answer before data
    /// flows (SOCKS); transparent modes skip it and send optimistically.
    pub async fn wait_connected(&mut self) -> Result<(), (ConnectError, String)> {
        match self.rx.recv().await {
            Some(Inbound::ConnectOk) => Ok(()),
            Some(Inbound::ConnectErr(code, msg)) => {
                self.tx.mark_finished();
                Err((code, msg))
            }
            Some(other) => Err((ConnectError::General, format!("unexpected {other:?}"))),
            None => Err((ConnectError::General, "tunnel closed".into())),
        }
    }
}

/// Sending half.
#[derive(Clone)]
pub struct ChanTx {
    shared: Arc<ChanShared>,
}

impl ChanTx {
    pub fn id(&self) -> u32 {
        self.shared.id
    }

    /// Queues a frame behind this channel's earlier DATA.
    pub async fn send(&self, f: Frame) -> Result<()> {
        self.shared
            .inner
            .data_tx
            .send(f)
            .await
            .map_err(|_| anyhow!("tunnel is closed"))
    }

    pub async fn send_data(&self, b: Bytes) -> Result<()> {
        self.send(Frame::new(Kind::Data, self.shared.id, b)).await
    }

    pub async fn send_eof(&self) -> Result<()> {
        self.send(Frame::empty(Kind::Eof, self.shared.id)).await
    }

    pub async fn connect_ok(&self) -> Result<()> {
        self.send(Frame::empty(Kind::ConnectOk, self.shared.id))
            .await
    }

    pub async fn connect_err(&self, code: ConnectError, msg: &str) -> Result<()> {
        self.mark_finished();
        self.send(Frame::connect_err(self.shared.id, code, msg))
            .await
    }

    pub async fn send_dns(&self, kind: Kind, msg: Bytes) -> Result<()> {
        self.send(Frame::new(kind, self.shared.id, msg)).await
    }

    /// Sends a UDP datagram, or drops it if the tunnel is backed up.
    pub fn send_udp(&self, addr: &Addr, payload: &[u8]) -> bool {
        let f = Frame::udp_data(self.shared.id, addr, payload);
        self.shared.inner.data_tx.try_send(f).is_ok()
    }

    pub fn close_udp(&self) {
        if !self.shared.finished.swap(true, AcqRel) {
            self.shared
                .inner
                .send_ctrl(Frame::empty(Kind::UdpClose, self.shared.id));
        }
    }

    pub async fn wait_credit(&self) -> Option<u64> {
        self.shared.credit.wait().await
    }

    pub fn consume_credit(&self, n: usize) {
        self.shared.credit.consume(n as u64);
    }

    /// Tells the peer this channel is dead (local error).
    pub fn reset(&self) {
        if !self.shared.finished.swap(true, AcqRel) {
            self.shared
                .inner
                .send_ctrl(Frame::empty(Kind::Rst, self.shared.id));
        }
    }

    /// The channel ended cleanly, or the peer already knows it is gone.
    pub fn mark_finished(&self) {
        self.shared.finished.store(true, Release);
    }
}

/// Receiving half.
pub struct ChanRx {
    shared: Arc<ChanShared>,
    rx: mpsc::UnboundedReceiver<Inbound>,
    window: u32,
    consumed: u32,
    last_grant: Instant,
    /// Bytes delivered and the time they took, decaying by a quarter per
    /// grant: about the last window's worth, which smooths out bursts.
    paced_bytes: f64,
    paced_secs: f64,
}

impl ChanRx {
    pub async fn recv(&mut self) -> Option<Inbound> {
        self.rx.recv().await
    }

    /// Reports `n` bytes of DATA delivered to the local socket, and grants the
    /// peer more credit each time a quarter of the window has been used.
    ///
    /// The window tracks about twice this channel's bandwidth-delay product,
    /// using the smallest round trip seen (queueing inflates the average):
    /// - While the window is what limits the channel, a quarter of it drains
    ///   in about a third of a round trip, so it doubles.
    /// - Once the link is the limit, a quarter takes longer; growth stops at
    ///   about 2x BDP, where a quarter takes half the minimum round trip.
    /// - A window far bigger than the rate needs (a quarter takes over two
    ///   round trips) shrinks.
    ///
    /// Data a window allows beyond the BDP only waits in ssh's and the
    /// kernel's buffers, delaying every other channel; keeping windows near
    /// 2x BDP keeps pages responsive during big downloads.
    ///
    /// Those rules alone never take a window below the initial one, so when
    /// the tunnel is far slower than the windows (a slow or distant path),
    /// N busy channels keep N initial windows queued: seconds of delay for
    /// every channel. The brake handles that: when a channel's window takes
    /// more than four round trips to drain and the tunnel's standing RTT is
    /// over two, it withholds grants until the window drains in about 2.5
    /// round trips, down to `min_window`. Round trips count as at least
    /// `brake_rtt` here, so Wi-Fi jitter on a fast path never brakes. While
    /// that standing queue lasts, no window grows. A braked window grows
    /// back to the initial one by half at a time while it drains in under
    /// two round trips. Drain times here use about the last window's pace,
    /// since a single quarter can arrive in one burst.
    pub fn consumed(&mut self, n: usize) {
        self.consumed += n as u32;
        if self.consumed == 0 || self.consumed < self.window / 4 {
            return;
        }
        let shared = self.shared.clone();
        let inner = &shared.inner;
        let cfg = &inner.cfg;
        let mut grant = self.consumed;
        let now = Instant::now();
        let interval = now.duration_since(self.last_grant);
        let (min_rtt, standing) = inner.rtt_view();
        // Time to drain the whole window at the pace of about the last
        // window: a single quarter can arrive in one burst.
        self.paced_bytes = self.paced_bytes * 0.75 + self.consumed as f64;
        self.paced_secs = self.paced_secs * 0.75 + interval.as_secs_f64();
        let drain = Duration::from_secs_f64(
            (self.window as f64 * self.paced_secs / self.paced_bytes).min(1e6),
        );
        let slow = min_rtt.max(cfg.brake_rtt);
        // Data arrives in bursts, so one quick quarter proves little; while
        // the tunnel holds a standing queue, no window grows.
        let queued = standing > slow * 2;
        if queued && drain > slow * 4 {
            let target =
                (self.window as f64 * (slow.as_secs_f64() * 2.5 / drain.as_secs_f64())) as u32;
            let target = target.max(cfg.min_window).min(self.window);
            let shrink = grant.min(self.window - target);
            self.shrink(shrink);
            grant -= shrink;
            trace!(
                chan = self.shared.id,
                window = self.window,
                ?drain,
                ?standing,
                "braked"
            );
        } else if !queued && interval < min_rtt / 2 && self.window < cfg.max_window {
            grant += self.grow(self.window.min(cfg.max_window - self.window));
        } else if !queued && self.window < cfg.initial_window && drain < slow * 2 {
            grant += self.grow((self.window / 2).min(cfg.initial_window - self.window));
        } else if interval > min_rtt * 2 && self.window > cfg.initial_window {
            // `grant` is at least window/4, so this cannot underflow.
            let shrink = (self.window / 4).min(self.window - cfg.initial_window);
            self.shrink(shrink);
            grant -= shrink;
            trace!(chan = self.shared.id, window = self.window, "window shrunk");
        }
        self.last_grant = now;
        self.consumed = 0;
        if grant == 0 {
            return;
        }
        // Raise the allowance before telling the peer, so the reader can never
        // see data the peer was entitled to send as an overrun.
        self.shared.recv_allow.fetch_add(grant as i64, AcqRel);
        inner.send_ctrl(Frame::window(self.shared.id, grant));
    }

    /// Grows the window by `by` unless that would pass the global cap.
    /// Returns how much it grew.
    fn grow(&mut self, by: u32) -> u32 {
        let inner = &self.shared.inner;
        let total = inner.total_window.load(Relaxed);
        if total + by as u64 > inner.cfg.global_window_cap {
            return 0;
        }
        inner.total_window.fetch_add(by as u64, Relaxed);
        self.shared.window.fetch_add(by as u64, Relaxed);
        self.window += by;
        trace!(chan = self.shared.id, window = self.window, "window grown");
        by
    }

    fn shrink(&mut self, by: u32) {
        let inner = &self.shared.inner;
        inner.total_window.fetch_sub(by as u64, Relaxed);
        self.shared.window.fetch_sub(by as u64, Relaxed);
        self.window -= by;
    }

    /// Reports a UDP datagram of `n` bytes handled, freeing queue space.
    pub fn udp_consumed(&self, n: usize) {
        self.shared.recv_allow.fetch_add(n as i64, AcqRel);
    }
}

/// How a pumped TCP channel ended.
#[derive(Debug)]
pub enum PumpEnd {
    /// Both sides sent EOF and every byte was delivered.
    Clean,
    /// The peer reset the channel (or its connect failed).
    PeerReset,
    /// The local socket failed; the peer has been sent RST.
    LocalError(std::io::Error),
    /// The tunnel itself went away.
    TunnelClosed,
}

impl PumpEnd {
    pub fn is_clean(&self) -> bool {
        matches!(self, PumpEnd::Clean)
    }
}

enum Stop {
    Peer,
    Local(std::io::Error),
    Tunnel,
}

/// Copies bytes both ways between a TCP channel and a local stream until both
/// directions have finished. Half-close is honoured: an EOF from one side
/// shuts down writing on the other while the reverse direction keeps going.
pub async fn pump<R, W>(chan: Channel, mut rd: R, mut wr: W) -> PumpEnd
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let Channel { tx, mut rx } = chan;
    let up = async {
        let mut buf = BytesMut::new();
        loop {
            let Some(avail) = tx.wait_credit().await else {
                return Err(Stop::Peer);
            };
            let max = (avail as usize).min(DATA_CHUNK);
            buf.reserve(max);
            let n = rd
                .read_buf(&mut (&mut buf).limit(max))
                .await
                .map_err(Stop::Local)?;
            if n == 0 {
                tx.send_eof().await.map_err(|_| Stop::Tunnel)?;
                return Ok(());
            }
            tx.consume_credit(n);
            tx.send_data(buf.split().freeze())
                .await
                .map_err(|_| Stop::Tunnel)?;
        }
    };
    let down = async {
        loop {
            match rx.recv().await {
                Some(Inbound::Data(b)) => {
                    wr.write_all(&b).await.map_err(Stop::Local)?;
                    rx.consumed(b.len());
                }
                Some(Inbound::Eof) => {
                    let _ = wr.shutdown().await;
                    return Ok(());
                }
                Some(Inbound::ConnectOk) => {}
                Some(Inbound::Rst | Inbound::ConnectErr(..)) => return Err(Stop::Peer),
                Some(_) => {}
                None => return Err(Stop::Tunnel),
            }
        }
    };
    let end = match tokio::try_join!(up, down) {
        Ok(_) => {
            tx.mark_finished();
            PumpEnd::Clean
        }
        Err(Stop::Peer) => {
            tx.mark_finished();
            PumpEnd::PeerReset
        }
        Err(Stop::Local(e)) => {
            tx.reset();
            PumpEnd::LocalError(e)
        }
        Err(Stop::Tunnel) => PumpEnd::TunnelClosed,
    };
    if !end.is_clean() && tx.shared.inner.is_closed() {
        return PumpEnd::TunnelClosed;
    }
    end
}

/// Handle to a running mux. Cheap to clone.
#[derive(Clone)]
pub struct Mux {
    inner: Arc<Inner>,
}

impl Mux {
    /// Starts the mux tasks on the current tokio runtime. `prefix` holds bytes
    /// already read from `rd` (e.g. while looking for the startup banner).
    /// Returns the channels the peer opens (only a server gets any).
    pub fn start<R, W>(
        rd: R,
        wr: W,
        prefix: BytesMut,
        role: Role,
        cfg: MuxConfig,
    ) -> (Mux, mpsc::UnboundedReceiver<Open>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        let (data_tx, data_rx) = mpsc::channel(cfg.data_queue);
        let (open_tx, open_rx) = mpsc::unbounded_channel();
        let (closed, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            role,
            cfg,
            ctrl_tx,
            data_tx,
            channels: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            start: Instant::now(),
            rtt: Mutex::new(RttFilter::default()),
            total_window: AtomicU64::new(0),
            closed,
            stats: Stats::default(),
        });
        tokio::spawn(reader(inner.clone(), rd, prefix, open_tx));
        tokio::spawn(writer(inner.clone(), wr, ctrl_rx, data_rx));
        tokio::spawn(pinger(Arc::downgrade(&inner)));
        (Mux { inner }, open_rx)
    }

    fn alloc(&self, kind: ChanKind) -> Result<Channel> {
        let id = self.inner.next_id.fetch_add(1, Relaxed);
        if id == u32::MAX {
            bail!("channel ids exhausted");
        }
        self.inner.register(id, kind)
    }

    /// Opens a TCP channel to `addr`. Returns at once: data may be sent before
    /// the server has connected (it buffers it within the window).
    pub async fn open_tcp(&self, addr: &Addr) -> Result<Channel> {
        let chan = self.alloc(ChanKind::Tcp)?;
        chan.tx.send(Frame::tcp_connect(chan.id(), addr)).await?;
        Ok(chan)
    }

    pub async fn open_udp(&self, ipv6: bool) -> Result<Channel> {
        let chan = self.alloc(ChanKind::Udp)?;
        chan.tx.send(Frame::udp_open(chan.id(), ipv6)).await?;
        Ok(chan)
    }

    /// Resolves one DNS query (a raw DNS message) through the server.
    pub async fn dns(&self, query: Bytes, timeout: Duration) -> Result<Bytes> {
        let mut chan = self.alloc(ChanKind::Dns)?;
        chan.tx.send_dns(Kind::DnsReq, query).await?;
        match tokio::time::timeout(timeout, chan.rx.recv()).await {
            Ok(Some(Inbound::Dns(resp))) => {
                chan.tx.mark_finished();
                Ok(resp)
            }
            Ok(_) => bail!("tunnel closed during DNS query"),
            Err(_) => bail!("DNS query timed out"),
        }
    }

    /// Waits until the mux has shut down and returns why.
    pub async fn closed(&self) -> String {
        let mut rx = self.inner.closed.subscribe();
        loop {
            if let Some(reason) = rx.borrow_and_update().clone() {
                return reason;
            }
            if rx.changed().await.is_err() {
                return "mux dropped".into();
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Asks the peer to shut down and closes this side.
    pub fn close(&self) {
        self.inner.send_ctrl(Frame::empty(Kind::Exit, 0));
        self.inner.shutdown("closed locally".into());
    }

    pub fn rtt(&self) -> Duration {
        self.inner.rtt()
    }

    pub fn stats(&self) -> &Stats {
        &self.inner.stats
    }

    pub fn open_channels(&self) -> usize {
        self.inner.channels.lock().unwrap().len()
    }
}

async fn reader<R>(
    inner: Arc<Inner>,
    mut rd: R,
    mut buf: BytesMut,
    opens: mpsc::UnboundedSender<Open>,
) where
    R: AsyncRead + Unpin,
{
    const READ_SIZE: usize = 256 * 1024;
    let mut closed = inner.closed.subscribe();
    let reason = loop {
        loop {
            match frame::parse(&mut buf) {
                Ok(Some(f)) => {
                    if let Err(e) = inner.dispatch(f, &opens) {
                        inner.shutdown(format!("{e:#}"));
                        return;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    inner.shutdown(format!("protocol error: {e:#}"));
                    return;
                }
            }
        }
        if buf.capacity() - buf.len() < READ_SIZE / 4 {
            buf.reserve(READ_SIZE);
        }
        tokio::select! {
            r = rd.read_buf(&mut buf) => match r {
                Ok(0) => break "peer closed the connection".to_string(),
                Ok(_) => {}
                Err(e) => break format!("read failed: {e}"),
            },
            _ = closed.changed() => return,
        }
    };
    inner.shutdown(reason);
}

async fn writer<W>(
    inner: Arc<Inner>,
    mut wr: W,
    mut ctrl_rx: mpsc::UnboundedReceiver<Frame>,
    mut data_rx: mpsc::Receiver<Frame>,
) where
    W: AsyncWrite + Unpin,
{
    const BATCH: usize = 256 * 1024;
    let mut closed = inner.closed.subscribe();
    let mut batch = BytesMut::with_capacity(BATCH + frame::MAX_PAYLOAD);
    let encode = |f: Frame, batch: &mut BytesMut| {
        trace!(kind = ?f.kind, chan = f.chan, len = f.payload.len(), ">");
        if f.kind == Kind::Data {
            inner
                .stats
                .bytes_up
                .fetch_add(f.payload.len() as u64, Relaxed);
        }
        let exit = f.kind == Kind::Exit;
        f.encode(batch);
        exit
    };
    loop {
        let first = tokio::select! {
            biased;
            Some(f) = ctrl_rx.recv() => f,
            Some(f) = data_rx.recv() => f,
            _ = closed.changed() => {
                // Deliver a final EXIT if one is queued, then stop.
                while let Ok(f) = ctrl_rx.try_recv() {
                    if encode(f, &mut batch) {
                        let _ = wr.write_all(&batch).await;
                        let _ = wr.flush().await;
                        break;
                    }
                }
                return;
            }
        };
        let mut exit = encode(first, &mut batch);
        while batch.len() < BATCH && !exit {
            if let Ok(f) = ctrl_rx.try_recv() {
                exit = encode(f, &mut batch);
            } else if let Ok(f) = data_rx.try_recv() {
                exit = encode(f, &mut batch);
            } else {
                break;
            }
        }
        let res = async {
            wr.write_all(&batch).await?;
            wr.flush().await
        }
        .await;
        batch.clear();
        if let Err(e) = res {
            inner.shutdown(format!("write failed: {e}"));
            return;
        }
        if exit {
            return;
        }
    }
}

async fn pinger(inner: std::sync::Weak<Inner>) {
    // First probe right away: windows autotune from the measured RTT.
    let mut received = 0;
    loop {
        let wait = {
            let Some(inner) = inner.upgrade() else { return };
            if inner.is_closed() {
                return;
            }
            let stamp = inner.start.elapsed().as_micros() as u64;
            inner.rtt.lock().unwrap().sent(stamp);
            inner.send_ctrl(Frame::ping(Kind::Ping, stamp));
            // Probe more often while DATA arrives: the brake needs to see
            // a queue build and drain.
            let now = inner.stats.bytes_down.load(Relaxed);
            let busy = now != received;
            received = now;
            if busy {
                inner.cfg.busy_ping_interval
            } else {
                inner.cfg.ping_interval
            }
        };
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    #[test]
    fn standing_rtt_ignores_jitter_but_not_a_queue() {
        let mut f = RttFilter::default();
        assert_eq!(f.standing(0), RttFilter::DEFAULT);
        // Jitter: one low sample in the last second keeps it low.
        for (i, rtt) in [20, 90, 20, 150, 95].iter().enumerate() {
            f.add(i as u64 * 250 * MS, rtt * MS);
        }
        assert_eq!(f.standing(1000 * MS), Duration::from_millis(20));
        // A queue that stays raises every sample.
        for i in 5..10 {
            f.add(i * 250 * MS, 900 * MS);
        }
        assert_eq!(f.standing(2250 * MS), Duration::from_millis(900));
        assert_eq!(f.min(), Duration::from_millis(20));
        // Old samples age out; the latest stands in when nothing is recent.
        assert_eq!(f.standing(60_000 * MS), Duration::from_millis(900));
    }

    #[test]
    fn unanswered_ping_counts_once_over_a_second() {
        let mut f = RttFilter::default();
        f.sent(0);
        f.answered(0);
        f.add(100 * MS, 100 * MS);
        f.sent(200 * MS);
        f.sent(450 * MS);
        // Waiting 800 ms could be jitter.
        assert_eq!(f.standing(1000 * MS), Duration::from_millis(100));
        // Waiting 3 s is a queue, though no sample says so yet.
        assert_eq!(f.standing(3200 * MS), Duration::from_millis(3000));
        // Its answer clears it; the next one waits less.
        f.answered(200 * MS);
        f.add(3300 * MS, 3100 * MS);
        assert_eq!(f.standing(3300 * MS), Duration::from_millis(3100));
        f.answered(450 * MS);
        assert!(f.waiting.is_empty());
    }
}
