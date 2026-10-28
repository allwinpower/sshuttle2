#![no_main]
//! IPv4 fragments as a local app could send them into the tun device, then
//! the reassembled datagram split again for the way back.

use libfuzzer_sys::fuzz_target;

#[path = "../../crates/client/src/ipfrag.rs"]
#[allow(dead_code)]
mod ipfrag;

fuzz_target!(|data: &[u8]| {
    let mut r = ipfrag::Reassembler::default();
    // Each record: flags and offset (u16), id (u8), length (u8), then that
    // many bytes. The reserved flag bit sends the bytes as a raw packet.
    let mut rest = data;
    while rest.len() >= 4 {
        let flags_off = u16::from_be_bytes([rest[0], rest[1]]);
        let id = rest[2];
        let len = (rest[3] as usize).min(rest.len() - 4);
        let body = &rest[4..4 + len];
        rest = &rest[4 + len..];
        let pkt = if flags_off & 0x8000 != 0 {
            body.to_vec()
        } else {
            let mut p = vec![0u8; 20];
            p[0] = 0x45;
            p[2..4].copy_from_slice(&((20 + len) as u16).to_be_bytes());
            p[5] = id;
            p[6..8].copy_from_slice(&flags_off.to_be_bytes());
            p[9] = 17;
            p[12..16].copy_from_slice(&[10, 0, 0, 1]);
            p[16..20].copy_from_slice(&[10, 0, 0, 2]);
            p.extend_from_slice(body);
            p
        };
        // tun mode only hands fragments to the reassembler.
        if !ipfrag::is_fragment(&pkt) {
            continue;
        }
        if let Some(whole) = r.add(&pkt) {
            assert!(whole.len() >= 20);
            for f in ipfrag::fragment(whole, 576) {
                assert!(f.len() <= 576);
            }
        }
    }
});
