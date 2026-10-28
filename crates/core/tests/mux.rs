//! End-to-end mux tests: a client mux and a server mux joined by an in-memory
//! pipe, with the server connecting to real local sockets.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use sshuttle2_core::frame::{ConnectError, Frame, Kind};
use sshuttle2_core::{Addr, Inbound, Mux, MuxConfig, Open, Role, pump};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

fn small_windows() -> MuxConfig {
    MuxConfig {
        initial_window: 64 * 1024,
        max_window: 1024 * 1024,
        ..MuxConfig::default()
    }
}

/// A minimal server: TCP connects to the given address, UDP is relayed, DNS
/// answers with the query reversed.
async fn serve(mut opens: tokio::sync::mpsc::UnboundedReceiver<Open>) {
    while let Some(open) = opens.recv().await {
        tokio::spawn(async move {
            match open {
                Open::Tcp { chan, addr } => {
                    let Addr::Ip(sa) = addr else {
                        panic!("test uses IPs")
                    };
                    match TcpStream::connect(sa).await {
                        Ok(mut s) => {
                            s.set_nodelay(true).unwrap();
                            chan.tx.connect_ok().await.unwrap();
                            let (r, w) = s.split();
                            pump(chan, r, w).await;
                        }
                        Err(e) => {
                            chan.tx
                                .connect_err(ConnectError::from_io(&e), &e.to_string())
                                .await
                                .unwrap();
                        }
                    }
                }
                Open::Dns { chan, query } => {
                    let mut v = query.to_vec();
                    v.reverse();
                    chan.tx.send_dns(Kind::DnsResp, v.into()).await.unwrap();
                    chan.tx.mark_finished();
                }
                Open::Udp { chan, .. } => {
                    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
                    let (tx, mut rx) = chan.split();
                    let mut buf = vec![0u8; 65536];
                    loop {
                        tokio::select! {
                            m = rx.recv() => match m {
                                Some(Inbound::Udp(Addr::Ip(dst), data)) => {
                                    sock.send_to(&data, dst).await.unwrap();
                                    rx.udp_consumed(data.len());
                                }
                                _ => break,
                            },
                            r = sock.recv_from(&mut buf) => {
                                let (n, src) = r.unwrap();
                                tx.send_udp(&Addr::Ip(src), &buf[..n]);
                            }
                        }
                    }
                }
            }
        });
    }
}

fn pair(cfg: MuxConfig) -> Mux {
    let (a, b) = tokio::io::duplex(64 * 1024);
    let (ar, aw) = tokio::io::split(a);
    let (br, bw) = tokio::io::split(b);
    let (client, _) = Mux::start(ar, aw, BytesMut::new(), Role::Client, cfg.clone());
    let (_server, opens) = Mux::start(br, bw, BytesMut::new(), Role::Server, cfg);
    tokio::spawn(serve(opens));
    client
}

/// Echo server that also reports how many bytes it saw before EOF.
async fn echo_server() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            s.set_nodelay(true).unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                tokio::io::copy(&mut r, &mut w).await.unwrap();
                w.shutdown().await.unwrap();
            });
        }
    });
    addr
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// Opens a channel, sends `data` through the echo server, returns the echo.
async fn round_trip(mux: &Mux, addr: SocketAddr, data: Vec<u8>) -> Vec<u8> {
    let mut chan = mux.open_tcp(&Addr::Ip(addr)).await.unwrap();
    chan.wait_connected().await.unwrap();
    // A local socket pair stands in for the application's connection.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app = TcpStream::connect(l.local_addr().unwrap());
    let (app, accepted) = tokio::join!(app, l.accept());
    let mut app = app.unwrap();
    let (mut local, _) = accepted.unwrap();
    let pumping = tokio::spawn(async move {
        let (r, w) = local.split();
        pump(chan, r, w).await
    });
    let (mut ar, mut aw) = app.split();
    let send = async {
        aw.write_all(&data).await.unwrap();
        aw.shutdown().await.unwrap();
    };
    let mut got = Vec::new();
    let recv = ar.read_to_end(&mut got);
    let (_, n) = tokio::join!(send, recv);
    n.unwrap();
    assert!(pumping.await.unwrap().is_clean());
    got
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_transfer_survives_small_windows() {
    let mux = pair(small_windows());
    let echo = echo_server().await;
    let data = pattern(32 * 1024 * 1024 + 123, 7);
    let got = round_trip(&mux, echo, data.clone()).await;
    assert_eq!(got.len(), data.len());
    assert!(got == data, "echoed bytes differ");
    assert!(
        mux.stats()
            .bytes_up
            .load(std::sync::atomic::Ordering::Relaxed)
            >= data.len() as u64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_channels_at_once() {
    let mux = pair(small_windows());
    let echo = echo_server().await;
    let mut tasks = Vec::new();
    for i in 0..64u8 {
        let mux = mux.clone();
        tasks.push(tokio::spawn(async move {
            let data = pattern(256 * 1024 + i as usize, i);
            let got = round_trip(&mux, echo, data.clone()).await;
            assert!(got == data, "channel {i} corrupted");
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    // Every channel unregistered itself.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(mux.open_channels(), 0);
}

#[tokio::test]
async fn refused_connect_reports_reason() {
    let mux = pair(MuxConfig::default());
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    let mut chan = mux.open_tcp(&Addr::Ip(closed)).await.unwrap();
    let err = chan.wait_connected().await.unwrap_err();
    assert_eq!(err.0, ConnectError::Refused);
}

#[tokio::test]
async fn half_close_keeps_reverse_direction_open() {
    let mux = pair(MuxConfig::default());
    // Server reads everything until EOF, then answers with the byte count.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.write_all(format!("got {}", all.len()).as_bytes())
            .await
            .unwrap();
    });
    let got = round_trip(&mux, addr, vec![1u8; 100_000]).await;
    assert_eq!(got, b"got 100000");
}

#[tokio::test]
async fn dns_round_trip() {
    let mux = pair(MuxConfig::default());
    let resp = mux
        .dns(Bytes::from_static(b"abc"), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(&resp[..], b"cba");
}

#[tokio::test]
async fn udp_round_trip() {
    let mux = pair(MuxConfig::default());
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        loop {
            let (n, from) = echo.recv_from(&mut buf).await.unwrap();
            echo.send_to(&buf[..n], from).await.unwrap();
        }
    });
    let chan = mux.open_udp(false).await.unwrap();
    let (tx, mut rx) = chan.split();
    // macOS caps UDP datagrams at 9216 bytes by default (net.inet.udp.maxdgram).
    let big = pattern(9000, 3);
    assert!(tx.send_udp(&Addr::Ip(echo_addr), &big));
    match tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        Some(Inbound::Udp(Addr::Ip(src), data)) => {
            assert_eq!(src, echo_addr);
            assert!(data[..] == big[..]);
        }
        other => panic!("unexpected {other:?}"),
    }
    tx.close_udp();
}

#[tokio::test]
async fn overrun_resets_channel() {
    // A raw peer that ignores flow control.
    let (a, b) = tokio::io::duplex(1024 * 1024);
    let (ar, aw) = tokio::io::split(a);
    let (_br, mut bw) = tokio::io::split(b);
    let cfg = MuxConfig {
        initial_window: 1000,
        ..MuxConfig::default()
    };
    let (client, _) = Mux::start(ar, aw, BytesMut::new(), Role::Client, cfg);
    let mut chan = client.open_tcp(&Addr::Domain("x".into(), 1)).await.unwrap();
    let id = chan.id();
    let mut out = BytesMut::new();
    Frame::empty(Kind::ConnectOk, id).encode(&mut out);
    Frame::new(Kind::Data, id, vec![0u8; 800]).encode(&mut out);
    Frame::new(Kind::Data, id, vec![0u8; 800]).encode(&mut out);
    bw.write_all(&out).await.unwrap();
    assert!(matches!(chan.rx.recv().await, Some(Inbound::ConnectOk)));
    assert!(matches!(chan.rx.recv().await, Some(Inbound::Data(_))));
    assert!(matches!(chan.rx.recv().await, Some(Inbound::Rst)));
}

#[tokio::test]
async fn pings_from_a_peer_that_never_reads_queue_few_pongs() {
    // A tiny pipe: our writer blocks after a few frames.
    let (a, b) = tokio::io::duplex(64);
    let (ar, aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);
    let (_client, _) = Mux::start(ar, aw, BytesMut::new(), Role::Client, MuxConfig::default());
    let mut out = BytesMut::new();
    for i in 0..2000u64 {
        Frame::ping(Kind::Ping, i).encode(&mut out);
    }
    // A PING without an 8-byte stamp gets no answer.
    Frame::new(Kind::Ping, 0, vec![0u8; 60_000]).encode(&mut out);
    bw.write_all(&out).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut got = BytesMut::new();
    let mut pongs = 0;
    while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(200), br.read_buf(&mut got)).await
    {
        if n == 0 {
            break;
        }
        while let Some(f) = sshuttle2_core::frame::parse(&mut got).unwrap() {
            if f.kind == Kind::Pong {
                assert_eq!(f.payload.len(), 8);
                pongs += 1;
            }
        }
    }
    assert!(pongs > 0 && pongs < 100, "{pongs} PONGs for 2000 PINGs");
}

#[tokio::test]
async fn repeated_channel_messages_are_dropped() {
    let (a, b) = tokio::io::duplex(1024 * 1024);
    let (ar, aw) = tokio::io::split(a);
    let (_br, mut bw) = tokio::io::split(b);
    let (client, _) = Mux::start(ar, aw, BytesMut::new(), Role::Client, MuxConfig::default());
    let mut chan = client.open_tcp(&Addr::Domain("x".into(), 1)).await.unwrap();
    let id = chan.id();
    let mut out = BytesMut::new();
    for _ in 0..1000 {
        Frame::empty(Kind::ConnectOk, id).encode(&mut out);
        Frame::new(Kind::DnsResp, id, vec![0u8; 1000]).encode(&mut out);
        Frame::empty(Kind::Eof, id).encode(&mut out);
    }
    bw.write_all(&out).await.unwrap();
    assert!(matches!(chan.rx.recv().await, Some(Inbound::ConnectOk)));
    assert!(matches!(chan.rx.recv().await, Some(Inbound::Eof)));
    let more = tokio::time::timeout(Duration::from_millis(100), chan.rx.recv()).await;
    assert!(more.is_err(), "unexpected {more:?}");
}

#[tokio::test]
async fn peer_eof_closes_mux() {
    let (a, b) = tokio::io::duplex(1024);
    let (ar, aw) = tokio::io::split(a);
    let (client, _) = Mux::start(ar, aw, BytesMut::new(), Role::Client, MuxConfig::default());
    drop(b);
    let reason = tokio::time::timeout(Duration::from_secs(5), client.closed())
        .await
        .unwrap();
    assert!(!reason.is_empty());
    assert!(client.open_tcp(&Addr::Domain("x".into(), 1)).await.is_err());
}

/// One direction of an emulated network path: `delay` one way and at most
/// `rate` bytes per second, behind a deep buffer (like ssh's and the
/// kernel's, which queue whatever the windows let in). Data arrives in
/// chunks of up to 32 KiB, as ssh delivers whole packets.
async fn shaped<R, W>(mut rd: R, mut wr: W, rate: f64, delay: Duration)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::time::{Instant, sleep, sleep_until};
    let buffer = Duration::from_secs(10);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    tokio::spawn(async move {
        while let Some((at, chunk)) = rx.recv().await {
            sleep_until(at).await;
            if wr.write_all(&chunk).await.is_err() {
                return;
            }
        }
    });
    let mut wire_free = Instant::now();
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = match rd.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let now = Instant::now();
        wire_free = wire_free.max(now) + Duration::from_secs_f64(n as f64 / rate);
        if tx.send((wire_free + delay, buf[..n].to_vec())).is_err() {
            return;
        }
        if wire_free > now + buffer {
            sleep(wire_free - now - buffer).await;
        }
    }
}

/// A client and a server mux joined by an emulated path.
fn slow_pair(cfg: MuxConfig, rate: f64, delay: Duration) -> Mux {
    let (c, c_link) = tokio::io::duplex(64 * 1024);
    let (s, s_link) = tokio::io::duplex(64 * 1024);
    let (cr, cw) = tokio::io::split(c);
    let (sr, sw) = tokio::io::split(s);
    let (c_link_r, c_link_w) = tokio::io::split(c_link);
    let (s_link_r, s_link_w) = tokio::io::split(s_link);
    tokio::spawn(shaped(c_link_r, s_link_w, rate, delay));
    tokio::spawn(shaped(s_link_r, c_link_w, rate, delay));
    let (client, _) = Mux::start(cr, cw, BytesMut::new(), Role::Client, cfg.clone());
    let (_server, opens) = Mux::start(sr, sw, BytesMut::new(), Role::Server, cfg);
    tokio::spawn(serve(opens));
    client
}

/// A server that sends data to every connection until it closes.
async fn source_server() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let chunk = vec![7u8; 16 * 1024];
                while s.write_all(&chunk).await.is_ok() {}
            });
        }
    });
    addr
}

/// Time for one byte to go through the echo server and back.
async fn echo_time(mux: &Mux, echo: SocketAddr, samples: usize) -> Vec<Duration> {
    let mut chan = mux.open_tcp(&Addr::Ip(echo)).await.unwrap();
    chan.wait_connected().await.unwrap();
    let mut times = Vec::new();
    for _ in 0..samples {
        let t = std::time::Instant::now();
        chan.tx.send_data(Bytes::from_static(b"x")).await.unwrap();
        chan.tx.consume_credit(1);
        loop {
            match chan.rx.recv().await {
                Some(Inbound::Data(b)) => {
                    chan.rx.consumed(b.len());
                    break;
                }
                Some(Inbound::ConnectOk) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        times.push(t.elapsed());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    times
}

/// Four downloads through a path far slower than their windows: the queue
/// they leave in the tunnel must stay short, or every other channel (and
/// every new connection) waits behind it. Without the brake each channel
/// keeps its initial window queued: 4 x 64 KiB at 1 MB/s is about 260 ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_path_keeps_queue_short() {
    let cfg = MuxConfig {
        initial_window: 64 * 1024,
        max_window: 1024 * 1024,
        brake_rtt: Duration::from_millis(20),
        ping_interval: Duration::from_millis(100),
        busy_ping_interval: Duration::from_millis(50),
        ..MuxConfig::default()
    };
    let rate = 1_000_000.0;
    let mux = slow_pair(cfg, rate, Duration::from_millis(10));
    let source = source_server().await;
    let echo = echo_server().await;
    let idle = echo_time(&mux, echo, 3).await;
    let received = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    for _ in 0..4 {
        let mut chan = mux.open_tcp(&Addr::Ip(source)).await.unwrap();
        let received = received.clone();
        tokio::spawn(async move {
            while let Some(m) = chan.rx.recv().await {
                if let Inbound::Data(b) = m {
                    received.fetch_add(b.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    chan.rx.consumed(b.len());
                }
            }
        });
    }
    // The initial windows drain first.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = received.load(std::sync::atomic::Ordering::Relaxed);
    let t = std::time::Instant::now();
    let mut loaded = echo_time(&mux, echo, 10).await;
    let rate_seen = (received.load(std::sync::atomic::Ordering::Relaxed) - before) as f64
        / t.elapsed().as_secs_f64();
    loaded.sort();
    let median = loaded[loaded.len() / 2];
    assert!(
        median < Duration::from_millis(150),
        "echo under load took {median:?} (idle {:?}, all {loaded:?})",
        idle[idle.len() - 1]
    );
    assert!(
        rate_seen > rate * 0.8,
        "the brake cost throughput: {rate_seen:.0} B/s of {rate:.0}"
    );
}
