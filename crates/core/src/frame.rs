//! Frame layout: `[kind u8][channel u32][length u32][payload]`, big-endian.
//!
//! See `docs/PROTOCOL.md` for what each kind carries.

use anyhow::{Result, bail, ensure};
use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::addr::Addr;

pub const HEADER_LEN: usize = 9;

/// Largest DATA payload a sender produces. Small enough that one bulk channel
/// cannot hold the link for long before another channel gets a turn.
pub const DATA_CHUNK: usize = 32 * 1024;

/// Largest payload a receiver accepts. A UDP datagram (up to 64 KiB) plus its
/// address is the biggest legitimate frame.
pub const MAX_PAYLOAD: usize = 65_536 + 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Hello = 1,
    Ping = 2,
    Pong = 3,
    TcpConnect = 4,
    ConnectOk = 5,
    ConnectErr = 6,
    Data = 7,
    Window = 8,
    Eof = 9,
    Rst = 10,
    UdpOpen = 11,
    UdpData = 12,
    UdpClose = 13,
    DnsReq = 14,
    DnsResp = 15,
    Exit = 16,
}

impl TryFrom<u8> for Kind {
    type Error = anyhow::Error;

    fn try_from(v: u8) -> Result<Self> {
        Ok(match v {
            1 => Kind::Hello,
            2 => Kind::Ping,
            3 => Kind::Pong,
            4 => Kind::TcpConnect,
            5 => Kind::ConnectOk,
            6 => Kind::ConnectErr,
            7 => Kind::Data,
            8 => Kind::Window,
            9 => Kind::Eof,
            10 => Kind::Rst,
            11 => Kind::UdpOpen,
            12 => Kind::UdpData,
            13 => Kind::UdpClose,
            14 => Kind::DnsReq,
            15 => Kind::DnsResp,
            16 => Kind::Exit,
            other => bail!("unknown frame kind {other}"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub chan: u32,
    pub payload: Bytes,
}

impl Frame {
    pub fn new(kind: Kind, chan: u32, payload: impl Into<Bytes>) -> Self {
        Frame {
            kind,
            chan,
            payload: payload.into(),
        }
    }

    pub fn empty(kind: Kind, chan: u32) -> Self {
        Frame {
            kind,
            chan,
            payload: Bytes::new(),
        }
    }

    pub fn wire_len(&self) -> usize {
        HEADER_LEN + self.payload.len()
    }

    pub fn encode(&self, out: &mut BytesMut) {
        out.reserve(self.wire_len());
        out.put_u8(self.kind as u8);
        out.put_u32(self.chan);
        out.put_u32(self.payload.len() as u32);
        out.put_slice(&self.payload);
    }

    // Constructors for frames whose payload has structure.

    pub fn window(chan: u32, credit: u32) -> Self {
        Frame::new(Kind::Window, chan, credit.to_be_bytes().to_vec())
    }

    pub fn ping(kind: Kind, stamp: u64) -> Self {
        Frame::new(kind, 0, stamp.to_be_bytes().to_vec())
    }

    pub fn tcp_connect(chan: u32, addr: &Addr) -> Self {
        Frame::new(Kind::TcpConnect, chan, addr.to_bytes())
    }

    pub fn connect_err(chan: u32, code: ConnectError, msg: &str) -> Self {
        let mut b = BytesMut::with_capacity(1 + msg.len());
        b.put_u8(code as u8);
        b.put_slice(msg.as_bytes());
        Frame::new(Kind::ConnectErr, chan, b.freeze())
    }

    pub fn udp_open(chan: u32, ipv6: bool) -> Self {
        Frame::new(Kind::UdpOpen, chan, vec![if ipv6 { 6 } else { 4 }])
    }

    /// `addr` is the destination going out and the source coming back.
    pub fn udp_data(chan: u32, addr: &Addr, payload: &[u8]) -> Self {
        let mut b = BytesMut::with_capacity(addr.encoded_len() + payload.len());
        addr.encode(&mut b);
        b.put_slice(payload);
        Frame::new(Kind::UdpData, chan, b.freeze())
    }
}

/// Pulls one complete frame off the front of `buf`, or returns `None` when
/// more bytes are needed. The payload shares `buf`'s allocation (no copy).
pub fn parse(buf: &mut BytesMut) -> Result<Option<Frame>> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) as usize;
    ensure!(
        len <= MAX_PAYLOAD,
        "frame payload of {len} bytes exceeds limit"
    );
    if buf.len() < HEADER_LEN + len {
        buf.reserve(HEADER_LEN + len - buf.len());
        return Ok(None);
    }
    let kind = Kind::try_from(buf[0])?;
    let chan = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);
    buf.advance(HEADER_LEN);
    let payload = buf.split_to(len).freeze();
    Ok(Some(Frame {
        kind,
        chan,
        payload,
    }))
}

pub fn parse_u32(payload: &Bytes) -> Result<u32> {
    ensure!(
        payload.len() == 4,
        "expected 4-byte payload, got {}",
        payload.len()
    );
    Ok(u32::from_be_bytes(payload[..4].try_into().unwrap()))
}

pub fn parse_u64(payload: &Bytes) -> Result<u64> {
    ensure!(
        payload.len() == 8,
        "expected 8-byte payload, got {}",
        payload.len()
    );
    Ok(u64::from_be_bytes(payload[..8].try_into().unwrap()))
}

/// Why a server could not connect. The values are SOCKS5 reply codes, so the
/// SOCKS front end can hand them straight to its client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ConnectError {
    General = 1,
    NotAllowed = 2,
    NetworkUnreachable = 3,
    HostUnreachable = 4,
    Refused = 5,
    TimedOut = 6,
}

impl ConnectError {
    pub fn from_code(code: u8) -> Self {
        match code {
            2 => ConnectError::NotAllowed,
            3 => ConnectError::NetworkUnreachable,
            4 => ConnectError::HostUnreachable,
            5 => ConnectError::Refused,
            6 => ConnectError::TimedOut,
            _ => ConnectError::General,
        }
    }

    pub fn from_io(e: &std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::ConnectionRefused => ConnectError::Refused,
            K::TimedOut => ConnectError::TimedOut,
            K::PermissionDenied => ConnectError::NotAllowed,
            K::NetworkUnreachable => ConnectError::NetworkUnreachable,
            K::HostUnreachable => ConnectError::HostUnreachable,
            _ => ConnectError::General,
        }
    }
}

pub fn parse_connect_err(payload: &Bytes) -> (ConnectError, String) {
    match payload.split_first() {
        Some((code, msg)) => (
            ConnectError::from_code(*code),
            String::from_utf8_lossy(msg).into_owned(),
        ),
        None => (ConnectError::General, String::new()),
    }
}

/// HELLO payload: `key=value` lines. Unknown keys are ignored, which leaves
/// room to add options without a version bump.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hello {
    pub pairs: Vec<(String, String)>,
}

impl Hello {
    pub fn with(mut self, key: &str, value: impl ToString) -> Self {
        self.pairs.push((key.to_string(), value.to_string()));
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn encode(&self) -> Bytes {
        let mut s = String::new();
        for (k, v) in &self.pairs {
            s.push_str(k);
            s.push('=');
            s.push_str(v);
            s.push('\n');
        }
        Bytes::from(s)
    }

    pub fn decode(payload: &Bytes) -> Result<Hello> {
        let text = std::str::from_utf8(payload)?;
        let pairs = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Ok(Hello { pairs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_parse_round_trip() {
        let frames = [
            Frame::window(7, 123_456),
            Frame::new(Kind::Data, u32::MAX, vec![0xab; 1000]),
            Frame::empty(Kind::Eof, 3),
            Frame::tcp_connect(9, &Addr::Domain("example.org".into(), 443)),
        ];
        let mut buf = BytesMut::new();
        for f in &frames {
            f.encode(&mut buf);
        }
        // Feed the bytes back one at a time: the parser must never return a
        // partial frame.
        let all = buf.freeze();
        let mut feed = BytesMut::new();
        let mut out = Vec::new();
        for b in all.iter() {
            feed.put_u8(*b);
            while let Some(f) = parse(&mut feed).unwrap() {
                out.push(f);
            }
        }
        assert_eq!(out, frames);
        assert!(feed.is_empty());
    }

    #[test]
    fn rejects_oversized_and_unknown() {
        let mut buf = BytesMut::new();
        buf.put_u8(Kind::Data as u8);
        buf.put_u32(1);
        buf.put_u32((MAX_PAYLOAD + 1) as u32);
        assert!(parse(&mut buf).is_err());

        let mut buf = BytesMut::new();
        buf.put_u8(200);
        buf.put_u32(1);
        buf.put_u32(0);
        assert!(parse(&mut buf).is_err());
    }

    #[test]
    fn hello_round_trip() {
        let h = Hello::default()
            .with("version", 1)
            .with("dns", "1.1.1.1:53");
        let back = Hello::decode(&h.encode()).unwrap();
        assert_eq!(back.get("version"), Some("1"));
        assert_eq!(back.get("dns"), Some("1.1.1.1:53"));
        assert_eq!(back.get("missing"), None);
    }

    #[test]
    fn connect_err_round_trip() {
        let f = Frame::connect_err(5, ConnectError::Refused, "nope");
        assert_eq!(
            parse_connect_err(&f.payload),
            (ConnectError::Refused, "nope".into())
        );
    }
}
