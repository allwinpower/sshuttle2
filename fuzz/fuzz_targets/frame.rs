#![no_main]
//! Frames as a hostile peer could send them: any bytes, arriving in any
//! chunk size, then every payload decoder on each frame that comes out.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use sshuttle2_core::Addr;
use sshuttle2_core::frame::{self, Hello};

fuzz_target!(|data: &[u8]| {
    let Some((&chunk, rest)) = data.split_first() else {
        return;
    };
    let mut buf = BytesMut::new();
    for piece in rest.chunks(chunk.max(1) as usize) {
        buf.extend_from_slice(piece);
        loop {
            match frame::parse(&mut buf) {
                Ok(Some(f)) => {
                    let _ = Hello::decode(&f.payload);
                    let _ = frame::parse_u32(&f.payload);
                    let _ = frame::parse_u64(&f.payload);
                    let _ = frame::parse_connect_err(&f.payload);
                    let mut p = f.payload.clone();
                    if let Ok(a) = Addr::decode(&mut p) {
                        let mut out = BytesMut::new();
                        a.encode(&mut out);
                        assert_eq!(out.len(), a.encoded_len());
                    }
                }
                Ok(None) => break,
                // The mux ends the session on a parse error.
                Err(_) => return,
            }
        }
    }
});
