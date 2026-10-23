//! IPv4 fragmentation for tun mode. Apps sending UDP datagrams larger than
//! the device MTU hand us fragments, which are reassembled here; replies
//! larger than the MTU are split before they go to the device (macOS drops
//! oversized packets written to a utun).

use std::collections::HashMap;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING: usize = 64;
const MAX_DATAGRAM: usize = 65_535;

/// True for an IPv4 packet that is a fragment (more-fragments set, or a
/// non-zero offset).
pub fn is_fragment(pkt: &[u8]) -> bool {
    pkt.len() >= 20 && pkt[0] >> 4 == 4 && u16::from_be_bytes([pkt[6], pkt[7]]) & 0x3fff != 0
}

struct Pending {
    header: Vec<u8>,
    parts: Vec<(usize, Vec<u8>)>,
    total: Option<usize>,
    started: Instant,
}

#[derive(Default)]
pub struct Reassembler {
    pending: HashMap<([u8; 4], [u8; 4], u16, u8), Pending>,
}

impl Reassembler {
    /// Adds one fragment. Returns the whole packet once every byte is in.
    pub fn add(&mut self, pkt: &[u8]) -> Option<Vec<u8>> {
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        let total_len = (u16::from_be_bytes([pkt[2], pkt[3]]) as usize).min(pkt.len());
        if ihl < 20 || total_len < ihl {
            return None;
        }
        let flags_off = u16::from_be_bytes([pkt[6], pkt[7]]);
        let more = flags_off & 0x2000 != 0;
        let offset = (flags_off & 0x1fff) as usize * 8;
        let data = &pkt[ihl..total_len];
        if offset + data.len() > MAX_DATAGRAM {
            return None;
        }
        let key = (
            pkt[12..16].try_into().unwrap(),
            pkt[16..20].try_into().unwrap(),
            u16::from_be_bytes([pkt[4], pkt[5]]),
            pkt[9],
        );
        let now = Instant::now();
        self.pending
            .retain(|_, p| now.duration_since(p.started) < TIMEOUT);
        if !self.pending.contains_key(&key) && self.pending.len() >= MAX_PENDING {
            return None;
        }
        let p = self.pending.entry(key).or_insert_with(|| Pending {
            header: Vec::new(),
            parts: Vec::new(),
            total: None,
            started: now,
        });
        if offset == 0 {
            p.header = pkt[..ihl].to_vec();
        }
        if !more {
            p.total = Some(offset + data.len());
        }
        if !p.parts.iter().any(|(o, _)| *o == offset) {
            p.parts.push((offset, data.to_vec()));
        }
        let total = p.total?;
        if p.header.is_empty() {
            return None;
        }
        p.parts.sort_by_key(|(o, _)| *o);
        let mut have = 0;
        for (o, d) in &p.parts {
            if *o > have {
                return None; // a hole
            }
            have = have.max(o + d.len());
        }
        if have < total {
            return None;
        }
        let p = self.pending.remove(&key).unwrap();
        let mut out = p.header.clone();
        out.resize(p.header.len() + total, 0);
        for (o, d) in &p.parts {
            let end = (o + d.len()).min(total);
            out[p.header.len() + o..p.header.len() + end].copy_from_slice(&d[..end - o]);
        }
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_be_bytes());
        out[6] = 0;
        out[7] = 0;
        set_checksum(&mut out[..p.header.len()]);
        Some(out)
    }
}

/// Splits an IPv4 packet into fragments of at most `mtu` bytes. Packets that
/// fit, or that are not IPv4, come back unchanged.
pub fn fragment(pkt: Vec<u8>, mtu: usize) -> Vec<Vec<u8>> {
    if pkt.len() <= mtu || pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return vec![pkt];
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    let data = &pkt[ihl..];
    // Fragment payloads are multiples of 8 bytes, except the last.
    let chunk = (mtu - ihl) / 8 * 8;
    let mut out = Vec::new();
    let mut off = 0;
    while off < data.len() {
        let end = (off + chunk).min(data.len());
        let more = end < data.len();
        let mut f = Vec::with_capacity(ihl + end - off);
        f.extend_from_slice(&pkt[..ihl]);
        f.extend_from_slice(&data[off..end]);
        let len = f.len() as u16;
        f[2..4].copy_from_slice(&len.to_be_bytes());
        let flags_off = ((off / 8) as u16) | if more { 0x2000 } else { 0 };
        f[6..8].copy_from_slice(&flags_off.to_be_bytes());
        set_checksum(&mut f[..ihl]);
        out.push(f);
        off = end;
    }
    out
}

fn set_checksum(header: &mut [u8]) {
    header[10] = 0;
    header[11] = 0;
    let mut sum: u32 = 0;
    for c in header.chunks(2) {
        sum += u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    header[10..12].copy_from_slice(&(!(sum as u16)).to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_udp() -> Vec<u8> {
        let payload: Vec<u8> = (0..8000u32).map(|i| i as u8).collect();
        let mut out = Vec::new();
        etherparse::PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
            .udp(1000, 2000)
            .write(&mut out, &payload)
            .unwrap();
        out
    }

    #[test]
    fn fragment_then_reassemble() {
        let pkt = big_udp();
        let frags = fragment(pkt.clone(), 1500);
        assert!(frags.len() > 1);
        assert!(frags.iter().all(|f| f.len() <= 1500 && is_fragment(f)));
        // Out of order, with a duplicate.
        let mut r = Reassembler::default();
        let mut order: Vec<&Vec<u8>> = frags.iter().rev().collect();
        order.insert(1, &frags[2]);
        let mut whole = None;
        for f in order {
            if let Some(p) = r.add(f) {
                whole = Some(p);
            }
        }
        let whole = whole.expect("reassembled");
        // The builder sets don't-fragment; the reassembled packet, being
        // the product of fragmentation, has no flags. Otherwise identical.
        let mut want = pkt.clone();
        want[6] = 0;
        set_checksum(&mut want[..20]);
        assert_eq!(whole, want);
        let (h, _) = etherparse::Ipv4HeaderSlice::from_slice(&whole)
            .map(|h| (h, ()))
            .unwrap();
        assert_eq!(h.header_checksum(), h.to_header().calc_header_checksum());
    }

    #[test]
    fn small_packets_pass_through() {
        let mut out = Vec::new();
        etherparse::PacketBuilder::ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
            .udp(1, 2)
            .write(&mut out, b"hi")
            .unwrap();
        assert_eq!(fragment(out.clone(), 1500), vec![out.clone()]);
        assert!(!is_fragment(&out));
    }
}
