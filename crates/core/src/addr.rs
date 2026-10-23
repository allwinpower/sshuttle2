//! Destination addresses as carried on the wire.
//!
//! The encoding borrows SOCKS5's address types so the SOCKS front end can pass
//! a request through untouched: `1` = IPv4, `3` = domain name, `4` = IPv6,
//! each followed by a big-endian port.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::{Result, bail, ensure};
use bytes::{Buf, BufMut, Bytes, BytesMut};

pub const ATYP_V4: u8 = 1;
pub const ATYP_DOMAIN: u8 = 3;
pub const ATYP_V6: u8 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Addr {
    Ip(SocketAddr),
    /// A name the far end resolves, so DNS happens where the connection does.
    Domain(String, u16),
}

impl Addr {
    pub fn port(&self) -> u16 {
        match self {
            Addr::Ip(sa) => sa.port(),
            Addr::Domain(_, port) => *port,
        }
    }

    pub fn encode(&self, out: &mut BytesMut) {
        match self {
            Addr::Ip(SocketAddr::V4(sa)) => {
                out.put_u8(ATYP_V4);
                out.put_slice(&sa.ip().octets());
                out.put_u16(sa.port());
            }
            Addr::Ip(SocketAddr::V6(sa)) => {
                out.put_u8(ATYP_V6);
                out.put_slice(&sa.ip().octets());
                out.put_u16(sa.port());
            }
            Addr::Domain(name, port) => {
                // Names longer than 255 bytes are not valid DNS names; they are
                // rejected where they enter (SOCKS front end) so this cannot truncate.
                debug_assert!(name.len() <= 255);
                out.put_u8(ATYP_DOMAIN);
                out.put_u8(name.len() as u8);
                out.put_slice(name.as_bytes());
                out.put_u16(*port);
            }
        }
    }

    pub fn encoded_len(&self) -> usize {
        match self {
            Addr::Ip(SocketAddr::V4(_)) => 1 + 4 + 2,
            Addr::Ip(SocketAddr::V6(_)) => 1 + 16 + 2,
            Addr::Domain(name, _) => 1 + 1 + name.len() + 2,
        }
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut out = BytesMut::with_capacity(self.encoded_len());
        self.encode(&mut out);
        out.freeze()
    }

    /// Reads one address from the front of `buf`, leaving the rest.
    pub fn decode(buf: &mut Bytes) -> Result<Addr> {
        ensure!(buf.has_remaining(), "empty address");
        let atyp = buf.get_u8();
        match atyp {
            ATYP_V4 => {
                ensure!(buf.remaining() >= 6, "short IPv4 address");
                let mut ip = [0u8; 4];
                buf.copy_to_slice(&mut ip);
                let port = buf.get_u16();
                Ok(Addr::Ip(SocketAddr::new(Ipv4Addr::from(ip).into(), port)))
            }
            ATYP_V6 => {
                ensure!(buf.remaining() >= 18, "short IPv6 address");
                let mut ip = [0u8; 16];
                buf.copy_to_slice(&mut ip);
                let port = buf.get_u16();
                Ok(Addr::Ip(SocketAddr::new(Ipv6Addr::from(ip).into(), port)))
            }
            ATYP_DOMAIN => {
                ensure!(buf.has_remaining(), "short domain address");
                let len = buf.get_u8() as usize;
                ensure!(buf.remaining() >= len + 2, "short domain address");
                let name = buf.split_to(len);
                let port = buf.get_u16();
                let name = String::from_utf8(name.to_vec())?;
                Ok(Addr::Domain(name, port))
            }
            other => bail!("unknown address type {other}"),
        }
    }
}

impl From<SocketAddr> for Addr {
    fn from(sa: SocketAddr) -> Self {
        Addr::Ip(sa)
    }
}

impl From<(IpAddr, u16)> for Addr {
    fn from((ip, port): (IpAddr, u16)) -> Self {
        Addr::Ip(SocketAddr::new(ip, port))
    }
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Addr::Ip(sa) => write!(f, "{sa}"),
            Addr::Domain(name, port) => write!(f, "{name}:{port}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let cases = [
            Addr::Ip("1.2.3.4:443".parse().unwrap()),
            Addr::Ip("[2001:db8::1]:53".parse().unwrap()),
            Addr::Domain("example.com".into(), 80),
        ];
        for a in cases {
            let mut b = a.to_bytes();
            assert_eq!(b.len(), a.encoded_len());
            b = [b, Bytes::from_static(b"tail")].concat().into();
            assert_eq!(Addr::decode(&mut b).unwrap(), a);
            assert_eq!(&b[..], b"tail");
        }
    }

    #[test]
    fn rejects_truncated() {
        let mut b = Bytes::from_static(&[ATYP_V4, 1, 2, 3]);
        assert!(Addr::decode(&mut b).is_err());
        let mut b = Bytes::from_static(&[ATYP_DOMAIN, 10, b'a']);
        assert!(Addr::decode(&mut b).is_err());
        let mut b = Bytes::from_static(&[9]);
        assert!(Addr::decode(&mut b).is_err());
    }
}
