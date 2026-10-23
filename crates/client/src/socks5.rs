//! A SOCKS5 proxy (RFC 1928) whose connections go through the tunnel.
//! Needs no root: apps opt in by pointing at it. CONNECT and UDP ASSOCIATE
//! are supported; names in requests are resolved on the server.

use std::net::SocketAddr;

use anyhow::{Result, bail, ensure};
use bytes::{Bytes, BytesMut};
use sshuttle2_core::addr::{ATYP_DOMAIN, ATYP_V4, ATYP_V6};
use sshuttle2_core::fdio::set_linger_zero;
use sshuttle2_core::mux::{ChanRx, ChanTx};
use sshuttle2_core::{Addr, Inbound, Mux, pump};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tracing::debug;

use crate::closer::close_later;

const VER: u8 = 5;
const CMD_CONNECT: u8 = 1;
const CMD_UDP_ASSOCIATE: u8 = 3;
const REP_OK: u8 = 0;
const REP_CMD_UNSUPPORTED: u8 = 7;

pub async fn serve(listener: TcpListener, mux: Mux) -> Result<()> {
    loop {
        let (sock, peer) = listener.accept().await?;
        let mux = mux.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, mux).await {
                debug!("socks client {peer}: {e:#}");
            }
        });
    }
}

async fn handle(mut s: TcpStream, mux: Mux) -> Result<()> {
    s.set_nodelay(true)?;
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr).await?;
    ensure!(hdr[0] == VER, "not a SOCKS5 client (version {})", hdr[0]);
    let mut methods = vec![0u8; hdr[1] as usize];
    s.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        s.write_all(&[VER, 0xff]).await?;
        bail!("client offers no unauthenticated method");
    }
    s.write_all(&[VER, 0]).await?;

    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    ensure!(req[0] == VER, "bad request version {}", req[0]);
    let addr = read_addr(&mut s, req[3]).await?;
    match req[1] {
        CMD_CONNECT => connect(s, mux, addr).await,
        CMD_UDP_ASSOCIATE => udp_associate(s, mux).await,
        other => {
            reply(&mut s, REP_CMD_UNSUPPORTED, unspecified()).await?;
            bail!("unsupported command {other}");
        }
    }
}

async fn read_addr(s: &mut TcpStream, atyp: u8) -> Result<Addr> {
    let len = match atyp {
        ATYP_V4 => 4 + 2,
        ATYP_V6 => 16 + 2,
        ATYP_DOMAIN => s.read_u8().await? as usize + 2,
        other => bail!("unknown address type {other}"),
    };
    let mut raw = BytesMut::with_capacity(len + 2);
    raw.extend_from_slice(&[atyp]);
    if atyp == ATYP_DOMAIN {
        raw.extend_from_slice(&[(len - 2) as u8]);
    }
    let start = raw.len();
    raw.resize(start + len, 0);
    s.read_exact(&mut raw[start..]).await?;
    Addr::decode(&mut raw.freeze())
}

fn unspecified() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 0))
}

async fn reply(s: &mut TcpStream, code: u8, bound: SocketAddr) -> Result<()> {
    let mut out = BytesMut::from(&[VER, code, 0][..]);
    Addr::Ip(bound).encode(&mut out);
    s.write_all(&out).await?;
    Ok(())
}

async fn connect(mut s: TcpStream, mux: Mux, addr: Addr) -> Result<()> {
    let mut chan = mux.open_tcp(&addr).await?;
    if let Err((code, msg)) = chan.wait_connected().await {
        reply(&mut s, code as u8, unspecified()).await?;
        bail!("connect {addr}: {msg}");
    }
    reply(&mut s, REP_OK, unspecified()).await?;
    let (r, w) = s.split();
    let end = pump(chan, r, w).await;
    debug!("{addr}: {end:?}");
    if !end.is_clean() {
        set_linger_zero(&s);
    }
    close_later(s);
    Ok(())
}

/// One tunnel UDP channel per address family, opened when first needed.
struct UdpChans {
    v4: Option<(ChanTx, ChanRx)>,
    v6: Option<(ChanTx, ChanRx)>,
}

async fn recv_from(slot: &mut Option<(ChanTx, ChanRx)>) -> Option<Inbound> {
    match slot {
        Some((_, rx)) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn udp_associate(mut ctl: TcpStream, mux: Mux) -> Result<()> {
    let local_ip = ctl.local_addr()?.ip();
    let udp = UdpSocket::bind((local_ip, 0)).await?;
    reply(&mut ctl, REP_OK, udp.local_addr()?).await?;

    let mut client: Option<SocketAddr> = None;
    let mut chans = UdpChans { v4: None, v6: None };
    let mut buf = vec![0u8; 65_536];
    let mut ctl_buf = [0u8; 64];
    loop {
        tokio::select! {
            // The association lasts as long as the control connection.
            r = ctl.read(&mut ctl_buf) => {
                if matches!(r, Ok(0) | Err(_)) {
                    break;
                }
            }
            r = udp.recv_from(&mut buf) => {
                let (n, from) = r?;
                if *client.get_or_insert(from) != from {
                    continue;
                }
                let Some((dst, payload)) = parse_udp_request(&buf[..n]) else { continue };
                let ipv6 = matches!(dst, Addr::Ip(SocketAddr::V6(_)));
                let slot = if ipv6 { &mut chans.v6 } else { &mut chans.v4 };
                if slot.is_none() {
                    *slot = Some(mux.open_udp(ipv6).await?.split());
                }
                let (tx, _) = slot.as_ref().unwrap();
                tx.send_udp(&dst, payload);
            }
            m = recv_from(&mut chans.v4) => forward_reply(&udp, client, &mut chans.v4, m).await,
            m = recv_from(&mut chans.v6) => forward_reply(&udp, client, &mut chans.v6, m).await,
        }
    }
    for (tx, _) in [chans.v4.take(), chans.v6.take()].into_iter().flatten() {
        tx.close_udp();
    }
    Ok(())
}

async fn forward_reply(
    udp: &UdpSocket,
    client: Option<SocketAddr>,
    slot: &mut Option<(ChanTx, ChanRx)>,
    m: Option<Inbound>,
) {
    match m {
        Some(Inbound::Udp(src, data)) => {
            if let Some((_, rx)) = slot {
                rx.udp_consumed(data.len());
            }
            if let Some(client) = client {
                let mut out = BytesMut::from(&[0u8, 0, 0][..]);
                src.encode(&mut out);
                out.extend_from_slice(&data);
                let _ = udp.send_to(&out, client).await;
            }
        }
        // The server dropped this flow (idle timeout): reopen on next use.
        Some(Inbound::UdpClose | Inbound::Rst) | None => {
            if let Some((tx, _)) = slot.take() {
                tx.mark_finished();
            }
        }
        Some(_) => {}
    }
}

/// `[RSV RSV FRAG ATYP ADDR PORT DATA]`. Fragmented datagrams are dropped,
/// as most SOCKS servers do.
fn parse_udp_request(pkt: &[u8]) -> Option<(Addr, &[u8])> {
    if pkt.len() < 4 || pkt[2] != 0 {
        return None;
    }
    let mut rest = Bytes::copy_from_slice(&pkt[3..]);
    let before = rest.len();
    let addr = Addr::decode(&mut rest).ok()?;
    let used = before - rest.len();
    Some((addr, &pkt[3 + used..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_udp_header() {
        let mut pkt = vec![0, 0, 0];
        let mut a = BytesMut::new();
        Addr::Domain("example.com".into(), 53).encode(&mut a);
        pkt.extend_from_slice(&a);
        pkt.extend_from_slice(b"payload");
        let (addr, data) = parse_udp_request(&pkt).unwrap();
        assert_eq!(addr, Addr::Domain("example.com".into(), 53));
        assert_eq!(data, b"payload");
        pkt[2] = 1;
        assert!(parse_udp_request(&pkt).is_none());
    }
}
