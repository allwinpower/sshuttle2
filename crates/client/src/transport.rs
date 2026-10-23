//! Starting the server end of the tunnel: over `ssh`, or as a local child
//! process (`-r local`, for tests).

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use sshuttle2_core::fdio::set_socket_buffers;
use sshuttle2_core::frame::Hello;
use sshuttle2_core::{Mux, MuxConfig, Role, handshake};
use tokio::process::{Child, Command};
use tracing::debug;

use crate::bootstrap;

pub struct TunnelOpts {
    /// `[user@]host[:port]`, an ssh config alias, or `local`.
    pub remote: String,
    pub ssh_cmd: String,
    pub server_bin: Option<PathBuf>,
    pub verbose: u8,
    /// Nameserver the server should send DNS queries to (`--to-ns`).
    pub to_ns: Option<String>,
}

pub struct Tunnel {
    pub mux: Mux,
    pub server_hello: Hello,
    /// The ssh (or local server) process. Killed when dropped.
    pub child: Child,
}

pub async fn open(opts: &TunnelOpts) -> Result<Tunnel> {
    let nonce = nonce()?;
    let local = opts.remote == "local";
    let server_args = match opts.verbose {
        0 => String::new(),
        n => format!("-{}", "v".repeat(n as usize)),
    };

    // A socket pair rather than pipes: one fd for both directions, and room
    // for big kernel buffers (macOS defaults unix sockets to 8 KiB).
    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    set_socket_buffers(&ours, 1 << 20);
    set_socket_buffers(&theirs, 1 << 20);
    let theirs_out = theirs.try_clone()?;

    let mut cmd = if local {
        local_command(opts, &nonce, &server_args)?
    } else {
        ssh_command(opts, &nonce, &server_args)?
    };
    cmd.stdin(Stdio::from(OwnedFd::from(theirs)))
        .stdout(Stdio::from(OwnedFd::from(theirs_out)))
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    debug!("running {:?}", cmd.as_std());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {:?}", cmd.as_std().get_program()))?;
    // Drop our copies of the child's ends, or we would never see EOF.
    drop(cmd);

    ours.set_nonblocking(true)?;
    let ours = tokio::net::UnixStream::from_std(ours)?;
    let (mut rd, mut wr) = ours.into_split();
    let mut buf = BytesMut::new();

    let started = async {
        if !local {
            bootstrap::run(
                &mut rd,
                &mut wr,
                &mut buf,
                &nonce,
                opts.server_bin.as_deref(),
            )
            .await?;
        }
        handshake::read_banner(&mut rd, &mut buf, &nonce).await?;
        let mut hello = Hello::default().with("version", env!("CARGO_PKG_VERSION"));
        if let Some(ns) = &opts.to_ns {
            hello = hello.with("dns", ns);
        }
        handshake::send_hello(&mut wr, &hello).await?;
        handshake::read_hello(&mut rd, &mut buf).await
    }
    .await;

    let server_hello = match started {
        Ok(h) => h,
        Err(e) => {
            let status = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            let hint = match status {
                Ok(Ok(st)) => match st.code() {
                    Some(255) if !local => {
                        " (ssh exited with 255: it could not connect or log in)".to_string()
                    }
                    Some(127) => " (exit 127: /bin/sh or the server binary was not found)".into(),
                    Some(c) => format!(" (exited with {c})"),
                    None => " (killed by a signal)".into(),
                },
                _ => String::new(),
            };
            return Err(e.context(format!("starting the server failed{hint}")));
        }
    };
    debug!(?server_hello, "server hello");
    let (mux, _) = Mux::start(rd, wr, buf, Role::Client, MuxConfig::default());
    Ok(Tunnel {
        mux,
        server_hello,
        child,
    })
}

fn local_command(opts: &TunnelOpts, nonce: &str, server_args: &str) -> Result<Command> {
    let program = match &opts.server_bin {
        Some(p) => p.clone(),
        None => std::env::current_exe()?
            .parent()
            .context("no directory for the current executable")?
            .join("sshuttle2-server"),
    };
    let mut cmd = Command::new(program);
    cmd.args(["server", "--nonce", nonce]);
    if !server_args.is_empty() {
        cmd.arg(server_args);
    }
    Ok(cmd)
}

fn ssh_command(opts: &TunnelOpts, nonce: &str, server_args: &str) -> Result<Command> {
    let mut parts = shlex::split(&opts.ssh_cmd).context("cannot parse --ssh-cmd")?;
    if parts.is_empty() {
        bail!("--ssh-cmd is empty");
    }
    let mut cmd = Command::new(parts.remove(0));
    cmd.args(parts);
    cmd.args([
        // No pty: a pty would mangle the binary stream.
        "-T",
        "-e",
        "none",
        // The data is mostly already compressed or encrypted; zlib on one
        // core would become the new bottleneck.
        "-o",
        "Compression=no",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        // Prefer AES-GCM (hardware accelerated nearly everywhere) but keep
        // the defaults as fallback.
        "-o",
        "Ciphers=^aes128-gcm@openssh.com",
    ]);
    let (dest, port) = split_host_port(&opts.remote);
    if let Some(port) = port {
        cmd.args(["-p", &port]);
    }
    cmd.arg(dest)
        .arg("--")
        .arg(bootstrap::remote_command(nonce, server_args));
    Ok(cmd)
}

/// Splits `[user@]host[:port]`, handling `[v6]:port`. A bare IPv6 address
/// (several colons, no brackets) has no port.
fn split_host_port(remote: &str) -> (String, Option<String>) {
    let (user, hostport) = match remote.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, remote),
    };
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, tail)) => (h.to_string(), tail.strip_prefix(':').map(str::to_string)),
            None => (hostport.to_string(), None),
        }
    } else if hostport.matches(':').count() == 1 {
        let (h, p) = hostport.split_once(':').unwrap();
        (h.to_string(), Some(p.to_string()))
    } else {
        (hostport.to_string(), None)
    };
    let dest = match user {
        Some(u) => format!("{u}@{host}"),
        None => host,
    };
    (dest, port.filter(|p| !p.is_empty()))
}

fn nonce() -> Result<String> {
    let mut bytes = [0u8; 9];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut bytes)?;
    Ok(format!(
        "ts{}",
        bootstrap::sha256_hex(&bytes)[..16].to_owned()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_forms() {
        let s = |r: &str| split_host_port(r);
        assert_eq!(s("host"), ("host".into(), None));
        assert_eq!(s("root@1.2.3.4"), ("root@1.2.3.4".into(), None));
        assert_eq!(
            s("root@1.2.3.4:2222"),
            ("root@1.2.3.4".into(), Some("2222".into()))
        );
        assert_eq!(
            s("[2001:db8::1]:22"),
            ("2001:db8::1".into(), Some("22".into()))
        );
        assert_eq!(s("u@[2001:db8::1]"), ("u@2001:db8::1".into(), None));
        assert_eq!(s("2001:db8::1"), ("2001:db8::1".into(), None));
    }
}
