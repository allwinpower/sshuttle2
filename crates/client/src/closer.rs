//! Closing finished sockets off the async worker threads.
//!
//! `close()` is normally instant, but on macOS hosts with socket-filter
//! network extensions (endpoint security and VPN clients, Fortinet's among
//! them) it can block in the kernel for 9-12 seconds. Done on a tokio worker,
//! that would stall every task scheduled there. sshuttle hit the same problem
//! (see its `ssnet.close_later`).

use std::net::TcpStream;
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

const CLOSERS: usize = 4;
/// Past this many sockets waiting, close inline: an unbounded queue would
/// trade a stall for running out of file descriptors.
const MAX_BACKLOG: usize = 512;

static QUEUE: OnceLock<SyncSender<TcpStream>> = OnceLock::new();

fn queue() -> &'static SyncSender<TcpStream> {
    QUEUE.get_or_init(|| {
        let (tx, rx) = sync_channel::<TcpStream>(MAX_BACKLOG);
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        for i in 0..CLOSERS {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("closer-{i}"))
                .spawn(move || {
                    loop {
                        let next = rx.lock().unwrap().recv();
                        match next {
                            Ok(sock) => drop(sock),
                            Err(_) => return,
                        }
                    }
                })
                .expect("spawn closer thread");
        }
        tx
    })
}

pub fn close_later(sock: tokio::net::TcpStream) {
    let Ok(sock) = sock.into_std() else { return };
    match queue().try_send(sock) {
        Ok(()) => {}
        Err(TrySendError::Full(sock) | TrySendError::Disconnected(sock)) => drop(sock),
    }
}
