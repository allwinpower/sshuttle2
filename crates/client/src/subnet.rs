//! Networks to route through (or keep out of) the tunnel.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::str::FromStr;

use anyhow::{Context, Result, bail};

/// An IP network in CIDR form, host bits cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cidr {
    pub ip: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn new(ip: IpAddr, prefix: u8) -> Result<Cidr> {
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            bail!("prefix /{prefix} too long for {ip}");
        }
        Ok(Cidr {
            ip: mask(ip, prefix),
            prefix,
        })
    }

    pub fn host(ip: IpAddr) -> Cidr {
        Cidr {
            ip,
            prefix: if ip.is_ipv4() { 32 } else { 128 },
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn is_ipv6(&self) -> bool {
        self.ip.is_ipv6()
    }
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let m = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4(Ipv4Addr::from(bits & m))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let m = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6(Ipv6Addr::from(bits & m))
        }
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix)
    }
}

/// Parses sshuttle-style networks: `10.0.0.0/8`, the short forms `0/0` and
/// `10/8`, a bare address, or `::/0`.
impl FromStr for Cidr {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Cidr> {
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (
                a,
                Some(
                    p.parse::<u8>()
                        .with_context(|| format!("bad prefix in {s:?}"))?,
                ),
            ),
            None => (s, None),
        };
        if let Ok(ip) = addr.parse::<IpAddr>() {
            let prefix = prefix.unwrap_or(if ip.is_ipv4() { 32 } else { 128 });
            return Cidr::new(ip, prefix);
        }
        // IPv4 with missing trailing octets, as sshuttle accepts: "10/8", "0/0".
        let parts: Vec<&str> = addr.split('.').collect();
        if !parts.is_empty() && parts.len() < 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
            let mut octets = [0u8; 4];
            for (i, p) in parts.iter().enumerate() {
                octets[i] = p.parse().unwrap();
            }
            let prefix = prefix.unwrap_or(8 * parts.len() as u8);
            return Cidr::new(IpAddr::V4(Ipv4Addr::from(octets)), prefix);
        }
        bail!("not a network: {s:?}")
    }
}

/// A network with whether it is excluded from the tunnel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub net: Cidr,
    pub exclude: bool,
}

/// Parses a network, or resolves a hostname to all its addresses (/32 or
/// /128 each).
pub fn parse_or_resolve(s: &str) -> Result<Vec<Cidr>> {
    if let Ok(c) = s.parse::<Cidr>() {
        return Ok(vec![c]);
    }
    let (host, prefix) = match s.split_once('/') {
        Some((h, p)) => (h, Some(p.parse::<u8>()?)),
        None => (s, None),
    };
    let ips: Vec<IpAddr> = (host, 0)
        .to_socket_addrs()
        .with_context(|| format!("not a network and cannot resolve {host:?}"))?
        .map(|sa| sa.ip())
        .collect();
    if ips.is_empty() {
        bail!("{host} has no addresses");
    }
    let mut out = Vec::new();
    for ip in ips {
        let c = match prefix {
            Some(p) => Cidr::new(ip, p)?,
            None => Cidr::host(ip),
        };
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

/// Orders routes for first-match firewalls: most specific first, and on a
/// tie the exclusion first. (sshuttle's pf method sorted the other way, so a
/// `0/0` include matched before any `-x` exclusion and `-x` had no effect.)
pub fn sort_routes(routes: &mut Vec<Route>) {
    routes.sort_by(|a, b| {
        b.net
            .prefix
            .cmp(&a.net.prefix)
            .then(b.exclude.cmp(&a.exclude))
    });
    routes.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Cidr {
        s.parse().unwrap()
    }

    #[test]
    fn parses_forms() {
        assert_eq!(c("0/0").to_string(), "0.0.0.0/0");
        assert_eq!(c("10/8").to_string(), "10.0.0.0/8");
        assert_eq!(c("192.168/16").to_string(), "192.168.0.0/16");
        assert_eq!(c("10.1.2.3/8").to_string(), "10.0.0.0/8");
        assert_eq!(c("1.2.3.4").to_string(), "1.2.3.4/32");
        assert_eq!(c("::/0").to_string(), "::/0");
        assert_eq!(c("2001:db8::1/32").to_string(), "2001:db8::/32");
        assert!("1.2.3.4/33".parse::<Cidr>().is_err());
        assert!("example.com".parse::<Cidr>().is_err());
    }

    #[test]
    fn most_specific_first_excludes_win_ties() {
        let mut r = vec![
            Route {
                net: c("0/0"),
                exclude: false,
            },
            Route {
                net: c("1.2.3.4"),
                exclude: true,
            },
            Route {
                net: c("10/8"),
                exclude: false,
            },
            Route {
                net: c("10/8"),
                exclude: true,
            },
        ];
        sort_routes(&mut r);
        let order: Vec<String> = r
            .iter()
            .map(|r| format!("{}{}", if r.exclude { "-" } else { "+" }, r.net))
            .collect();
        assert_eq!(
            order,
            ["-1.2.3.4/32", "-10.0.0.0/8", "+10.0.0.0/8", "+0.0.0.0/0"]
        );
    }
}
