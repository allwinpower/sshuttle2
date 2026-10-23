//! DNS queries forwarded from the client, answered by this host's resolvers.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use sshuttle2_core::Channel;
use sshuttle2_core::frame::Kind;
use tokio::net::UdpSocket;
use tokio::time::timeout;
use tracing::debug;

const TRIES: usize = 3;
const TRY_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Resolver {
    servers: Vec<SocketAddr>,
}

impl Resolver {
    /// `override_ns` is the client's `--to-ns` (`IP` or `IP:PORT`); otherwise
    /// the nameservers in /etc/resolv.conf.
    pub fn new(override_ns: Option<&str>) -> Result<Self> {
        let servers = match override_ns {
            Some(ns) => vec![parse_ns(ns).with_context(|| format!("bad nameserver {ns:?}"))?],
            None => {
                let text = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
                let mut v = resolv_conf_servers(&text);
                if v.is_empty() {
                    v.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 53));
                }
                v
            }
        };
        Ok(Resolver { servers })
    }

    pub fn describe(&self) -> String {
        self.servers
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Sends the query, retrying across nameservers. Returns the first reply
    /// whose transaction id matches.
    async fn resolve(&self, query: &[u8]) -> Option<Bytes> {
        if query.len() < 12 {
            return None;
        }
        for attempt in 0..TRIES {
            let ns = self.servers[attempt % self.servers.len()];
            match timeout(TRY_TIMEOUT, ask(ns, query)).await {
                Ok(Ok(resp)) => return Some(resp),
                Ok(Err(e)) => debug!("DNS via {ns}: {e}"),
                Err(_) => debug!("DNS via {ns}: timed out"),
            }
        }
        None
    }
}

async fn ask(ns: SocketAddr, query: &[u8]) -> std::io::Result<Bytes> {
    let bind = if ns.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let sock = UdpSocket::bind(bind).await?;
    sock.connect(ns).await?;
    sock.send(query).await?;
    let mut buf = vec![0u8; 65_536];
    loop {
        let n = sock.recv(&mut buf).await?;
        if n >= 2 && buf[..2] == query[..2] {
            buf.truncate(n);
            return Ok(buf.into());
        }
    }
}

pub async fn answer(resolver: Arc<Resolver>, chan: Channel, query: Bytes) {
    if let Some(resp) = resolver.resolve(&query).await
        && chan.tx.send_dns(Kind::DnsResp, resp).await.is_ok()
    {
        chan.tx.mark_finished();
    }
    // On failure the channel drops unfinished, which resets it at the client.
}

fn parse_ns(s: &str) -> Option<SocketAddr> {
    s.parse::<SocketAddr>()
        .ok()
        .or_else(|| s.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, 53)))
}

fn resolv_conf_servers(text: &str) -> Vec<SocketAddr> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|rest| rest.split_whitespace().next())
        // Drop an IPv6 zone suffix (fe80::1%eth0): SocketAddr cannot carry it.
        .filter_map(|ip| ip.split('%').next()?.parse::<IpAddr>().ok())
        .map(|ip| SocketAddr::new(ip, 53))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_resolv_conf() {
        let text = "# comment\nnameserver 10.0.0.2\nsearch example.com\nnameserver  2001:db8::53\nnameserver fe80::1%eth0\nnameserverbogus\n";
        let v = resolv_conf_servers(text);
        assert_eq!(
            v,
            vec![
                "10.0.0.2:53".parse().unwrap(),
                "[2001:db8::53]:53".parse().unwrap(),
                "[fe80::1]:53".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn parses_override() {
        assert_eq!(parse_ns("1.1.1.1"), Some("1.1.1.1:53".parse().unwrap()));
        assert_eq!(
            parse_ns("1.1.1.1:5353"),
            Some("1.1.1.1:5353".parse().unwrap())
        );
        assert_eq!(parse_ns("nope"), None);
    }
}
