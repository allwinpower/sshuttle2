//! Wire protocol and stream multiplexer shared by the sshuttle2 client and server.

pub mod addr;
#[cfg(unix)]
pub mod fdio;
pub mod frame;
pub mod handshake;
pub mod mux;

pub use addr::Addr;
pub use mux::{Channel, Inbound, Mux, MuxConfig, Open, PumpEnd, Role, pump};

/// Protocol version spoken by this build. Client and server must match exactly.
pub const PROTOCOL_VERSION: u16 = 1;
