//! Session start: the banner line that marks where the binary stream begins,
//! then one HELLO frame each way.

use anyhow::{Context, Result, bail, ensure};
use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::PROTOCOL_VERSION;
use crate::frame::{self, Frame, Hello, Kind};

pub const BANNER: &str = "SSHUTTLE2";

/// The line the server prints before switching to frames. The nonce comes
/// from the client, so login-script noise can never be mistaken for it.
pub fn banner_line(nonce: &str) -> String {
    format!("{nonce} {BANNER} {PROTOCOL_VERSION}\n")
}

/// Reads lines from `rd` into `buf` until one starts with `nonce`, skipping
/// anything else (MOTD, shell rc output). Returns that line without the
/// nonce, leaving any bytes after it in `buf`.
pub async fn read_tagged_line<R: AsyncRead + Unpin>(
    rd: &mut R,
    buf: &mut BytesMut,
    nonce: &str,
) -> Result<String> {
    const MAX_JUNK: usize = 1 << 20;
    let mut skipped = 0usize;
    loop {
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.split_to(pos + 1);
            let text = String::from_utf8_lossy(&line[..pos]);
            let text = text.trim_end_matches('\r');
            if let Some(rest) = text.strip_prefix(nonce) {
                return Ok(rest.trim_start().to_string());
            }
            skipped += line.len();
            ensure!(skipped < MAX_JUNK, "no startup line from the remote side");
            if !text.trim().is_empty() {
                tracing::debug!("remote: {}", text.escape_debug());
            }
        }
        // A line that never ends counts too.
        ensure!(
            skipped + buf.len() < MAX_JUNK,
            "no startup line from the remote side"
        );
        buf.reserve(4096);
        let n = rd.read_buf(buf).await?;
        if n == 0 {
            let tail = String::from_utf8_lossy(buf).trim().to_string();
            if tail.is_empty() {
                bail!("remote side closed the connection during startup");
            }
            bail!("remote side closed the connection during startup (last output: {tail:?})");
        }
    }
}

/// Waits for the server's banner and checks its protocol version.
pub async fn read_banner<R: AsyncRead + Unpin>(
    rd: &mut R,
    buf: &mut BytesMut,
    nonce: &str,
) -> Result<()> {
    let line = read_tagged_line(rd, buf, nonce).await?;
    let mut words = line.split_whitespace();
    ensure!(
        words.next() == Some(BANNER),
        "unexpected startup line {line:?}"
    );
    let version: u16 = words.next().context("banner without version")?.parse()?;
    ensure!(
        version == PROTOCOL_VERSION,
        "server speaks protocol {version}, this client speaks {PROTOCOL_VERSION}"
    );
    Ok(())
}

pub async fn send_hello<W: AsyncWrite + Unpin>(wr: &mut W, hello: &Hello) -> Result<()> {
    let mut out = BytesMut::new();
    Frame::new(Kind::Hello, 0, hello.encode()).encode(&mut out);
    wr.write_all(&out).await?;
    wr.flush().await?;
    Ok(())
}

/// Reads the first frame, which must be HELLO. Bytes after it stay in `buf`.
pub async fn read_hello<R: AsyncRead + Unpin>(rd: &mut R, buf: &mut BytesMut) -> Result<Hello> {
    loop {
        if let Some(f) = frame::parse(buf)? {
            ensure!(f.kind == Kind::Hello, "expected HELLO, got {:?}", f.kind);
            return Hello::decode(&f.payload);
        }
        buf.reserve(4096);
        if rd.read_buf(buf).await? == 0 {
            bail!("connection closed before HELLO");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn endless_line_is_an_error() {
        let mut rd = tokio::io::repeat(b'x');
        let mut buf = BytesMut::new();
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_tagged_line(&mut rd, &mut buf, "n0nce"),
        )
        .await
        .expect("gave up in time");
        assert!(r.is_err());
        assert!(buf.len() <= 1 << 20);
    }
}
