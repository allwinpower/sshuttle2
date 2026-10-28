#![no_main]
//! The startup lines and HELLO frame, as a hostile server could send them.

use std::sync::LazyLock;

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use sshuttle2_core::handshake;

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
});

fuzz_target!(|data: &[u8]| {
    RT.block_on(async {
        let mut rd = data;
        let mut buf = BytesMut::new();
        if handshake::read_banner(&mut rd, &mut buf, "n0nce").await.is_ok() {
            let _ = handshake::read_hello(&mut rd, &mut buf).await;
        }
    });
});
