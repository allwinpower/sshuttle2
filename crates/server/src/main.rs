//! Server half of sshuttle2. The client uploads this binary over ssh and
//! runs `sshuttle2-server server --nonce N`; stdin/stdout then carry the
//! multiplexed tunnel.

mod dns;
mod relay;

use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use bytes::BytesMut;
use sha2::{Digest, Sha256};
use sshuttle2_core::fdio::{FdReader, FdWriter, grow_pipe};
use sshuttle2_core::frame::Hello;
use sshuttle2_core::handshake;
use sshuttle2_core::{Mux, MuxConfig, Open, PROTOCOL_VERSION, Role};
use tracing::{debug, info};

const USAGE: &str = "usage: sshuttle2-server server --nonce NONCE [-v...]
       sshuttle2-server --self-check SHA256
       sshuttle2-server --version";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("--version") => {
            println!(
                "sshuttle2-server {} (protocol {PROTOCOL_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        Some("--self-check") => return self_check(args.get(1).map(String::as_str)),
        Some("server") => run(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!(" s: fatal: {e:#}");
            ExitCode::from(99)
        }
    }
}

/// Exits 0 if this executable's SHA-256 matches `expected`. The bootstrap
/// script uses it to verify uploads and cache hits without needing
/// sha256sum on the server.
fn self_check(expected: Option<&str>) -> ExitCode {
    let Some(expected) = expected else {
        return ExitCode::from(2);
    };
    let path = std::fs::read_link("/proc/self/exe").or_else(|_| std::env::current_exe());
    let Ok(bytes) = path.and_then(std::fs::read) else {
        return ExitCode::from(3);
    };
    let actual = hex(&Sha256::digest(&bytes));
    if actual.eq_ignore_ascii_case(expected) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Opts {
    nonce: String,
    verbose: u8,
    threads: usize,
}

fn parse(args: &[String]) -> Result<Opts> {
    let mut opts = Opts {
        nonce: String::new(),
        verbose: 0,
        threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(4),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--nonce" => opts.nonce = it.next().context("--nonce needs a value")?.clone(),
            "--threads" => opts.threads = it.next().context("--threads needs a value")?.parse()?,
            v if v.starts_with("-v") && v[1..].chars().all(|c| c == 'v') => {
                opts.verbose += (v.len() - 1) as u8
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    if opts.nonce.is_empty() {
        bail!("--nonce is required\n{USAGE}");
    }
    Ok(opts)
}

fn run(args: &[String]) -> Result<()> {
    let opts = parse(args)?;
    let level = match opts.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::new(level))
        .with_target(false)
        .without_time()
        .init();

    // SAFETY: fds 0 and 1 are our stdin and stdout, open for the life of the
    // process; nothing else in this program uses them.
    let (stdin, stdout) = unsafe { (OwnedFd::from_raw_fd(0), OwnedFd::from_raw_fd(1)) };
    grow_pipe(&stdin, 1 << 20);
    grow_pipe(&stdout, 1 << 20);

    // The banner goes out before the fds turn non-blocking.
    {
        let mut out = std::io::stdout().lock();
        out.write_all(handshake::banner_line(&opts.nonce).as_bytes())?;
        out.flush()?;
    }

    let rt = if opts.threads <= 1 {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(opts.threads)
            .enable_all()
            .build()?
    };
    rt.block_on(serve(stdin, stdout))
}

async fn serve(stdin: OwnedFd, stdout: OwnedFd) -> Result<()> {
    let mut rd = FdReader::new(stdin)?;
    let mut wr = FdWriter::new(stdout)?;
    let mut buf = BytesMut::new();

    let hello = handshake::read_hello(&mut rd, &mut buf).await?;
    debug!(?hello, "client hello");
    let resolver = Arc::new(dns::Resolver::new(hello.get("dns"))?);
    let reply = Hello::default()
        .with("version", env!("CARGO_PKG_VERSION"))
        .with("ipv6", u8::from(relay::has_ipv6_route()));
    handshake::send_hello(&mut wr, &reply).await?;
    info!("tunnel up; resolving DNS via {}", resolver.describe());

    let (mux, mut opens) = Mux::start(rd, wr, buf, Role::Server, MuxConfig::default());
    while let Some(open) = opens.recv().await {
        match open {
            Open::Tcp { chan, addr } => {
                tokio::spawn(relay::tcp(chan, addr));
            }
            Open::Udp { chan, ipv6 } => {
                tokio::spawn(relay::udp(chan, ipv6));
            }
            Open::Dns { chan, query } => {
                tokio::spawn(dns::answer(resolver.clone(), chan, query));
            }
        }
    }
    info!("tunnel closed: {}", mux.closed().await);
    Ok(())
}
