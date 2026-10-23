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
