//! Transparent mode: the firewall redirects outgoing connections to local
//! listeners here; each is sent through the tunnel to where it was going.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use sshuttle2_core::fdio::set_linger_zero;
use sshuttle2_core::{Addr, Mux, pump};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tracing::{debug, warn};

use crate::closer::close_later;
use crate::helper::Helper;
use crate::sysnet::is_own_listener;

const DNS_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Listeners {
    pub tcp: Vec<TcpListener>,
    pub dns: Vec<Arc<UdpSocket>>,
    pub tcp_port: u16,
    pub dns_port: u16,
}

/// Binds the TCP redirector on 127.0.0.1 and ::1, and the DNS listener when
/// asked, searching ports downward from 12300 as sshuttle does. `fixed_port`
/// skips the search for the TCP port.
pub async fn bind(fixed_port: Option<u16>, dns: bool) -> Result<Listeners> {
    let ports: Vec<u16> = match fixed_port {
        Some(p) => vec![p],
        None => (9000..=12300).rev().collect(),
    };
    let mut tcp = None;
    for &p in &ports {
        match bind_tcp(p).await {
            Ok(l) => {
                tcp = Some((p, l));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e).context("binding the TCP redirector"),
        }
    }
    let (tcp_port, tcp) = tcp.context("no free port for the TCP redirector")?;
    let (dns_port, dns) = if dns {
        let mut found = None;
        for p in (9000..=12300).rev().filter(|p| *p != tcp_port) {
            match bind_udp(p).await {
                Ok(s) => {
                    found = Some((p, s));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(e) => return Err(e).context("binding the DNS listener"),
            }
        }
        found.context("no free port for the DNS listener")?
    } else {
        (0, Vec::new())
    };
    Ok(Listeners {
        tcp,
        dns,
        tcp_port,
        dns_port,
    })
}

async fn bind_tcp(port: u16) -> std::io::Result<Vec<TcpListener>> {
    let mut v = vec![TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?];
    match TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
        Ok(l) => v.push(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Err(e),
        Err(e) => debug!("no IPv6 redirector: {e}"),
    }
    Ok(v)
}

async fn bind_udp(port: u16) -> std::io::Result<Vec<Arc<UdpSocket>>> {
    let mut v = vec![Arc::new(
        UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).await?,
    )];
    match UdpSocket::bind((Ipv6Addr::LOCALHOST, port)).await {
        Ok(s) => v.push(Arc::new(s)),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return Err(e),
        Err(e) => debug!("no IPv6 DNS listener: {e}"),
    }
    Ok(v)
}

/// The listen addresses, which must be excluded from redirection.
pub fn listen_ips() -> [IpAddr; 2] {
    [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
}

/// Accepts redirected connections and DNS queries until an error.
pub async fn serve(l: Listeners, mux: Mux, helper: Arc<Helper>) -> Result<()> {
    let own = [l.tcp_port, l.dns_port];
    let mut tasks = tokio::task::JoinSet::new();
    for listener in l.tcp {
        tasks.spawn(accept_loop(listener, mux.clone(), helper.clone(), own));
    }
    for sock in l.dns {
        tasks.spawn(dns_loop(sock, mux.clone()));
    }
    match tasks.join_next().await {
        Some(Ok(r)) => r,
        Some(Err(e)) => bail!("listener task failed: {e}"),
        None => Ok(()),
    }
}

async fn accept_loop(l: TcpListener, mux: Mux, helper: Arc<Helper>, own: [u16; 2]) -> Result<()> {
    loop {
        let (sock, peer) = match l.accept().await {
            Ok(x) => x,
            Err(e) if matches!(e.raw_os_error(), Some(libc::EMFILE | libc::ENFILE)) => {
                // Out of file descriptors: the connection stays queued. Back
                // off rather than spin.
                warn!("too many open files; pausing accepts");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            Err(e) => return Err(e).context("accept"),
        };
        let mux = mux.clone();
        let helper = helper.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, peer, mux, helper, own).await {
                debug!("{peer}: {e:#}");
            }
        });
    }
}

async fn handle(
    mut sock: TcpStream,
    peer: SocketAddr,
    mux: Mux,
    helper: Arc<Helper>,
    own: [u16; 2],
) -> Result<()> {
    let dst = original_dst(&sock, peer, &helper)
        .await
        .context("finding the original destination")?;
    if is_own_listener(dst, &own) {
        debug!("{peer} connected to our own listener; dropping");
        return Ok(());
    }
    let _ = sock.set_nodelay(true);
    debug!("{peer} -> {dst}");
    // Optimistic: the app already thinks it is connected, so data flows
    // before the server has finished connecting. A failed connect resets.
    let chan = mux.open_tcp(&Addr::Ip(dst)).await?;
    let (r, w) = sock.split();
    let end = pump(chan, r, w).await;
    debug!("{peer} -> {dst}: {end:?}");
    if !end.is_clean() {
        set_linger_zero(&sock);
    }
    close_later(sock);
    Ok(())
}

/// Where a redirected connection was going. macOS: only root can ask pf,
/// so the helper does.
#[cfg(not(target_os = "linux"))]
async fn original_dst(sock: &TcpStream, peer: SocketAddr, helper: &Helper) -> Result<SocketAddr> {
    helper.natlook(peer, sock.local_addr()?).await
}

/// Where a redirected connection was going. Linux: conntrack keeps it for
/// nft's REDIRECT, and any process may ask about its own socket, so no round
/// trip to the helper.
#[cfg(target_os = "linux")]
async fn original_dst(sock: &TcpStream, _peer: SocketAddr, _helper: &Helper) -> Result<SocketAddr> {
    Ok(so_original_dst(sock, sock.local_addr()?.is_ipv6())?)
}

/// `getsockopt(SO_ORIGINAL_DST)`, or `IP6T_SO_ORIGINAL_DST` for IPv6 (the
/// same number at a different level).
#[cfg(target_os = "linux")]
fn so_original_dst(sock: &impl std::os::fd::AsRawFd, v6: bool) -> std::io::Result<SocketAddr> {
    let (level, name) = if v6 {
        (libc::SOL_IPV6, libc::IP6T_SO_ORIGINAL_DST)
    } else {
        (libc::SOL_IP, libc::SO_ORIGINAL_DST)
    };
    // SAFETY: sockaddr_storage is plain data, large enough for either
    // family, and the kernel writes at most `len` bytes of it.
    unsafe {
        let mut ss: libc::sockaddr_storage = std::mem::zeroed();
        let mut len = std::mem::size_of_val(&ss) as libc::socklen_t;
        if libc::getsockopt(
            sock.as_raw_fd(),
            level,
            name,
            (&raw mut ss).cast(),
            &mut len,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        match ss.ss_family as libc::c_int {
            libc::AF_INET => {
                let sin = &*(&raw const ss).cast::<libc::sockaddr_in>();
                Ok(SocketAddr::new(
                    Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).into(),
                    u16::from_be(sin.sin_port),
                ))
            }
            libc::AF_INET6 => {
                let sin6 = &*(&raw const ss).cast::<libc::sockaddr_in6>();
                Ok(SocketAddr::new(
                    Ipv6Addr::from(sin6.sin6_addr.s6_addr).into(),
                    u16::from_be(sin6.sin6_port),
                ))
            }
            f => Err(std::io::Error::other(format!(
                "original destination has address family {f}"
            ))),
        }
    }
}

async fn dns_loop(sock: Arc<UdpSocket>, mux: Mux) -> Result<()> {
    let mut buf = vec![0u8; 65_536];
    loop {
        let (n, from) = sock.recv_from(&mut buf).await?;
        let query = Bytes::copy_from_slice(&buf[..n]);
        let sock = sock.clone();
        let mux = mux.clone();
        tokio::spawn(async move {
            match mux.dns(query, DNS_TIMEOUT).await {
                // The reply goes back from our listener; pf or conntrack
                // rewrites its source to the nameserver the app asked.
                Ok(resp) => {
                    let _ = sock.send_to(&resp, from).await;
                }
                Err(e) => debug!("DNS for {from}: {e:#}"),
            }
        });
    }
}
