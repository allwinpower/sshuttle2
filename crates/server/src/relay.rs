//! Outbound connections the client asked for.

use std::net::SocketAddr;
use std::time::Duration;

use sshuttle2_core::fdio::set_linger_zero;
use sshuttle2_core::frame::ConnectError;
use sshuttle2_core::{Addr, Channel, Inbound, pump};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{Instant, sleep_until, timeout};
use tracing::debug;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const UDP_IDLE: Duration = Duration::from_secs(60);

pub async fn tcp(chan: Channel, addr: Addr) {
    let stream = match timeout(CONNECT_TIMEOUT, connect(&addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            debug!("connect {addr}: {e}");
            let _ = chan
                .tx
                .connect_err(ConnectError::from_io(&e), &e.to_string())
                .await;
            return;
        }
        Err(_) => {
            debug!("connect {addr}: timed out");
            let _ = chan
                .tx
                .connect_err(ConnectError::TimedOut, "connect timed out")
                .await;
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    if chan.tx.connect_ok().await.is_err() {
        return;
    }
    let mut stream = stream;
    let (r, w) = stream.split();
    let end = pump(chan, r, w).await;
    debug!("{addr}: {end:?}");
    if !end.is_clean() {
        set_linger_zero(&stream);
    }
}

async fn connect(addr: &Addr) -> std::io::Result<TcpStream> {
    match addr {
        Addr::Ip(sa) => TcpStream::connect(sa).await,
        Addr::Domain(name, port) => {
            let mut last = None;
            for sa in tokio::net::lookup_host((name.as_str(), *port)).await? {
                match TcpStream::connect(sa).await {
                    Ok(s) => return Ok(s),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(|| std::io::Error::other(format!("{name}: no addresses"))))
        }
    }
}

async fn resolve_udp(addr: &Addr, ipv6: bool) -> Option<SocketAddr> {
    match addr {
        Addr::Ip(sa) => Some(*sa),
        Addr::Domain(name, port) => tokio::net::lookup_host((name.as_str(), *port))
            .await
            .ok()?
            .find(|sa| sa.is_ipv6() == ipv6),
    }
}

/// One UDP flow from one client source port. A single unbound socket serves
/// every destination that source talks to (endpoint-independent mapping), so
/// STUN and peer-to-peer traffic work.
pub async fn udp(chan: Channel, ipv6: bool) {
    let bind = if ipv6 { "[::]:0" } else { "0.0.0.0:0" };
    let sock = match UdpSocket::bind(bind).await {
        Ok(s) => s,
        Err(e) => {
            debug!("UDP bind: {e}");
            return; // dropping the channel tells the client
        }
    };
    let (tx, mut rx) = chan.split();
    let mut buf = vec![0u8; 65_536];
    let mut deadline = Instant::now() + UDP_IDLE;
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Some(Inbound::Udp(addr, data)) => {
                    if let Some(dst) = resolve_udp(&addr, ipv6).await
                        && let Err(e) = sock.send_to(&data, dst).await
                    {
                        debug!("UDP send to {dst}: {e}");
                    }
                    rx.udp_consumed(data.len());
                    deadline = Instant::now() + UDP_IDLE;
                }
                Some(Inbound::UdpClose | Inbound::Rst) | None => {
                    tx.mark_finished();
                    return;
                }
                Some(_) => {}
            },
            r = sock.recv_from(&mut buf) => match r {
                Ok((n, src)) => {
                    tx.send_udp(&Addr::Ip(src), &buf[..n]);
                    deadline = Instant::now() + UDP_IDLE;
                }
                Err(e) => debug!("UDP recv: {e}"),
            },
            _ = sleep_until(deadline) => {
                tx.close_udp();
                return;
            }
        }
    }
}

/// Whether this host can reach the IPv6 internet. Connecting a UDP socket
/// sends nothing; it only asks the kernel for a route.
pub fn has_ipv6_route() -> bool {
    std::net::UdpSocket::bind("[::]:0")
        .and_then(|s| s.connect("[2001:4860:4860::8888]:53"))
        .is_ok()
}
