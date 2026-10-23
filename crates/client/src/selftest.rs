//! `sshuttle2 tun-selftest` (root): checks, on this machine, that
//! packets written to a tun device reach local sockets. Creates a device,
//! routes a test network into it, sends a UDP probe, and writes replies back
//! in each candidate framing, reporting which arrive.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::routes::{self, Via};
use crate::subnet::Cidr;

const TEST_NET: &str = "198.18.99.0/24";
const PROBE_DST: Ipv4Addr = Ipv4Addr::new(198, 18, 99, 1);

fn wait_readable(fd: &OwnedFd, ms: i32) -> bool {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}

/// Reads packets until an IPv4 one to the probe address with protocol
/// `proto` arrives (up to 2 s). Returns its length including the prefix.
fn read_matching(dev: &OwnedFd, buf: &mut [u8], off: usize, proto: u8) -> Option<usize> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if !wait_readable(dev, 200) {
            continue;
        }
        let n = unsafe { libc::read(dev.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            continue;
        }
        let n = n as usize;
        let pkt = &buf[off..n];
        if pkt.len() >= 20
            && pkt[0] >> 4 == 4
            && pkt[9] == proto
            && pkt[16..20] == PROBE_DST.octets()
        {
            return Some(n);
        }
    }
    None
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn run() -> Result<()> {
    let (dev, ifname) = crate::tundev::create()?;
    println!("created {ifname}");
    let mtu = routes::configure(&ifname, 1500)?;
    println!("configured {ifname}, MTU {mtu}");
    let net: Cidr = TEST_NET.parse()?;
    let guard = routes::TunRoutes::install(
        &ifname,
        &[crate::subnet::Route {
            net,
            exclude: false,
        }],
        &[],
    )?;
    println!("routed {TEST_NET} into {ifname}");
    let _ = Via::Interface(ifname.clone());

    // UDP: send a probe, read it from the device, answer it.
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_millis(700)))?;
    sock.send_to(b"probe", (PROBE_DST, 7))?;
    let off = crate::tundev::HEADER_LEN;
    let mut buf = vec![0u8; 65536];
    // Other traffic (IPv6 router solicitations, mDNS) may come first.
    let Some(n) = read_matching(&dev, &mut buf, off, 17) else {
        println!("FAIL: the UDP probe never appeared on {ifname}");
        drop(guard);
        return Ok(());
    };
    println!("read {n} bytes; first 32: {}", hex(&buf[..n.min(32)]));
    let pkt = &buf[off..n];
    let src = SocketAddr::new(
        Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]).into(),
        u16::from_be_bytes([pkt[20], pkt[21]]),
    );
    println!("probe came from {src}");

    let reply =
        crate::tunmode::build_udp_for_test(SocketAddr::new(PROBE_DST.into(), 7), src, b"reply")
            .context("building the reply")?;
    let variants: Vec<(&str, Vec<u8>)> = if off == 4 {
        vec![
            (
                "AF_INET big-endian [0 0 0 2]",
                [&[0u8, 0, 0, 2][..], &reply].concat(),
            ),
            (
                "AF_INET little-endian [2 0 0 0]",
                [&[2u8, 0, 0, 0][..], &reply].concat(),
            ),
        ]
    } else {
        vec![("no prefix", reply.clone())]
    };
    for (name, framed) in variants {
        let w = unsafe { libc::write(dev.as_raw_fd(), framed.as_ptr().cast(), framed.len()) };
        let werr = if w < 0 {
            format!(" ({})", std::io::Error::last_os_error())
        } else {
            String::new()
        };
        let mut got = [0u8; 64];
        let res = sock.recv_from(&mut got);
        match res {
            Ok((m, from)) => println!(
                "OK:   {name}: write returned {w}{werr}; socket got {:?} from {from}",
                String::from_utf8_lossy(&got[..m])
            ),
            Err(_) => {
                println!("FAIL: {name}: write returned {w}{werr}; nothing reached the socket")
            }
        }
    }

    // TCP: show what this OS's connection request looks like.
    let tcp = std::net::TcpStream::connect_timeout(
        &SocketAddr::new(PROBE_DST.into(), 80),
        Duration::from_millis(10),
    );
    drop(tcp);
    if let Some(n) = read_matching(&dev, &mut buf, off, 6) {
        let pkt = &buf[off..n];
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        let tcp = &pkt[ihl..];
        println!(
            "TCP SYN: flags {:#04x}, header {} bytes, options {}",
            tcp[13],
            (tcp[12] >> 4) * 4,
            hex(&tcp[20..((tcp[12] >> 4) as usize * 4).min(tcp.len())])
        );
    } else {
        println!("no TCP SYN seen");
    }
    // TCP through the userspace stack, as tun mode runs it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    for mtu in [1500u16, 9000] {
        let mtu = routes::configure(&ifname, mtu)?;
        rt.block_on(tcp_echo_test(dev.try_clone()?, mtu));
    }
    drop(guard);
    println!("done; routes removed, {ifname} goes away on exit");
    Ok(())
}

/// Runs ipstack on the device with an echo handler, connects to it from
/// this process, and prints the first packets each way.
async fn tcp_echo_test(dev: OwnedFd, mtu: u16) -> bool {
    use sshuttle2_core::fdio::{FdReader, FdWriter};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let off = crate::tundev::HEADER_LEN;
    let mut rd = match FdReader::new(dev.try_clone().unwrap()) {
        Ok(r) => r,
        Err(e) => {
            println!("FAIL: tun reader: {e}");
            return false;
        }
    };
    let mut wr = FdWriter::new(dev).unwrap();
    let (to_stack_tx, to_stack_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    let (from_stack_tx, mut from_stack_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    let shown = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let show = {
        let shown = shown.clone();
        move |dir: &str, pkt: &[u8]| {
            if let Some(d) = crate::tunmode::describe(pkt)
                && shown.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 16
            {
                println!("  {dir} {d}");
            }
        }
    };
    let show_out = show.clone();
    let writer = tokio::spawn(async move {
        while let Some(pkt) = from_stack_rx.recv().await {
            show_out("stack->app", &pkt);
            let mut framed = Vec::with_capacity(pkt.len() + 4);
            if off == 4 {
                framed.extend_from_slice(&crate::tundev::header_for(pkt[0]));
            }
            framed.extend_from_slice(&pkt);
            let _ = wr.write(&framed).await;
        }
    });
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; 70_000];
        loop {
            let Ok(n) = rd.read(&mut buf).await else {
                return;
            };
            if n <= off {
                continue;
            }
            let pkt = &buf[off..n];
            if pkt[0] >> 4 == 4 && pkt[9] == 6 {
                show("app->stack", pkt);
                let _ = to_stack_tx.try_send(pkt.to_vec());
            }
        }
    });
    let stack = crate::tunmode::test_stack(to_stack_rx, from_stack_tx, mtu);
    let echo = tokio::spawn(async move {
        let mut stack = stack;
        while let Ok(s) = stack.accept().await {
            if let ipstack::IpStackStream::Tcp(s) = s {
                tokio::spawn(async move {
                    let (mut r, mut w) = tokio::io::split(s);
                    // As tun mode's pump does: echo until the app's FIN, then
                    // shut down, which sends ours. Dropping the stream without
                    // that resets it.
                    if tokio::io::copy(&mut r, &mut w).await.is_ok() {
                        let _ = w.shutdown().await;
                    }
                });
            }
        }
    });

    let target = SocketAddr::new(Ipv4Addr::new(198, 18, 99, 3).into(), 80);
    let data: Vec<u8> = (0..2_000_000u32).map(|i| (i * 7 + i / 255) as u8).collect();
    let started = std::time::Instant::now();
    let res = tokio::time::timeout(Duration::from_secs(10), async {
        let s = tokio::net::TcpStream::connect(target)
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let (mut r, mut w) = s.into_split();
        let send = async {
            w.write_all(&data)
                .await
                .map_err(|e| format!("write: {e}"))?;
            w.shutdown().await.map_err(|e| format!("shutdown: {e}"))
        };
        let mut got = Vec::new();
        let recv = async {
            r.read_to_end(&mut got)
                .await
                .map_err(|e| format!("read: {e}"))
        };
        let (a, b) = tokio::join!(send, recv);
        a?;
        b?;
        Ok::<_, String>(got)
    })
    .await;
    writer.abort();
    reader.abort();
    echo.abort();
    let secs = started.elapsed().as_secs_f64();
    match res {
        Ok(Ok(got)) if got == data => {
            println!(
                "OK:   MTU {mtu}: 2 MB TCP echo intact ({:.0} Mbit/s)",
                2.0 * 8.0 * 2.0 / secs
            );
            true
        }
        Ok(Ok(got)) => {
            println!(
                "FAIL: MTU {mtu}: echo returned {} of {} bytes, or different bytes",
                got.len(),
                data.len()
            );
            false
        }
        Ok(Err(e)) => {
            println!("FAIL: MTU {mtu}: TCP {e}");
            false
        }
        Err(_) => {
            println!("FAIL: MTU {mtu}: TCP timed out (connect or echo)");
            false
        }
    }
}
