//! sshuttle2: a VPN over a plain SSH login.

mod bootstrap;
mod closer;
mod helper;
mod ipfrag;
mod nft;
mod pf;
mod routes;
mod selftest;
mod socks5;
mod subnet;
mod sysnet;
mod transparent;
mod transport;
mod tundev;
mod tunmode;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, Subcommand};
use sshuttle2_core::Mux;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use crate::subnet::{Cidr, Route};

// The static Linux build uses mimalloc instead of musl's slow allocator.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser, Debug)]
#[command(
    name = "sshuttle2",
    version,
    about = "A VPN over a plain SSH login (written in Rust, inspired by sshuttle)",
    args_conflicts_with_subcommands = true,
    override_usage = "sshuttle2 [OPTIONS] HOST [SUBNETS]...\n       \
        sshuttle2 [OPTIONS] -r HOST [SUBNETS]...\n       \
        sshuttle2 <COMMAND>",
    after_help = "Examples:\n  \
        sshuttle2 user@host                       everything (TCP, UDP, DNS, IPv6) via host\n  \
        sshuttle2 ssh://user@host:2222            the same, as a URL\n  \
        sshuttle2 user@host 10.0.0.0/8            one network\n  \
        sshuttle2 user@host --socks5 127.0.0.1:1080   proxy only, no root"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// ssh destination: [user@]host[:port], ssh://[user@]host[:port] or an
    /// ssh config alias. Without -r, the first argument is the host.
    /// "local" runs the server as a local process instead (for testing).
    #[arg(short = 'r', long = "remote", value_name = "HOST")]
    remote: Option<String>,

    /// Networks to send through the tunnel, e.g. 10.0.0.0/8 (default:
    /// everything). Without -r, the host comes first.
    #[arg(value_name = "SUBNETS")]
    subnets: Vec<String>,

    /// Networks to keep out of the tunnel. Repeatable.
    #[arg(short = 'x', long = "exclude", value_name = "SUBNET")]
    exclude: Vec<String>,

    /// Send DNS queries to the system's nameservers through the tunnel.
    #[arg(long)]
    dns: bool,

    /// Port for the local redirector (default: first free from 12300 down).
    #[arg(short = 'l', long = "listen", value_name = "PORT")]
    listen: Option<u16>,

    /// If pf is set to skip lo0, load "pass on lo0" for the session and
    /// reload /etc/pf.conf afterwards.
    #[arg(long)]
    pf_fix_loopback: bool,

    /// How to capture traffic: tun for TCP, all UDP, DNS and IPv6 through a
    /// virtual network device, or pf (macOS) or nft (Linux) for TCP and DNS
    /// only, through the firewall.
    #[arg(long, value_enum, default_value_t = Method::Tun)]
    method: Method,

    /// Tunnel UDP too. Does nothing now: tun is the default.
    #[arg(long)]
    udp: bool,

    /// MTU of the tun device.
    #[arg(long, default_value_t = 1500, value_name = "BYTES")]
    tun_mtu: u16,

    /// Run a SOCKS5 proxy on this address instead of capturing traffic.
    /// Needs no root.
    #[arg(long, value_name = "ADDR")]
    socks5: Option<SocketAddr>,

    /// Nameserver the remote side should send DNS queries to (IP[:PORT]).
    /// Default: the remote host's /etc/resolv.conf.
    #[arg(long = "to-ns", value_name = "IP[:PORT]")]
    to_ns: Option<String>,

    /// The ssh command to run, with any extra options.
    #[arg(
        short = 'e',
        long = "ssh-cmd",
        default_value = "ssh",
        value_name = "CMD"
    )]
    ssh_cmd: String,

    /// Upload this server binary instead of the built-in one.
    #[arg(long, value_name = "PATH")]
    server_bin: Option<PathBuf>,

    /// More output; repeat for more (-vv, -vvv).
    #[arg(short, long, action = ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    /// Same as tun; kept so old command lines still parse.
    #[value(hide = true)]
    Auto,
    Pf,
    Nft,
    Tun,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Remove firewall rules left behind by a session that crashed.
    Cleanup,
    /// Check that this machine delivers packets written to a tun device (root).
    #[command(hide = true)]
    TunSelftest,
    /// (internal) The root helper that owns the firewall rules.
    #[command(hide = true)]
    Helper {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        client_pid: i32,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "info",
        1 => "info,sshuttle2_core=info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("SSHUTTLE2_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    if matches!(cli.cmd, Some(Cmd::Helper { .. })) {
        // The helper's log travels to the client over its socket.
        tracing_subscriber::fmt()
            .with_writer(|| helper::LogWriter)
            .with_env_filter(filter)
            .with_target(false)
            .with_ansi(false)
            .without_time()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }

    match &cli.cmd {
        Some(Cmd::Helper { socket, client_pid }) => {
            return match helper::run(socket, *client_pid) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    error!("helper: {e:#}");
                    ExitCode::FAILURE
                }
            };
        }
        Some(Cmd::Cleanup) => return cleanup(),
        Some(Cmd::TunSelftest) => {
            return match selftest::run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    error!("{e:#}");
                    ExitCode::FAILURE
                }
            };
        }
        None => {}
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn cleanup() -> ExitCode {
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        // Re-run ourselves as root.
        let exe = std::env::current_exe().expect("current executable");
        let st = std::process::Command::new("sudo")
            .args(["-p", "[local sudo] Password: "])
            .arg(exe)
            .arg("cleanup")
            .status();
        return match st {
            Ok(s) if s.success() => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        };
    }
    match helper::cleanup() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(mut cli: Cli) -> Result<()> {
    raise_fd_limit();
    let remote = match cli.remote.take() {
        Some(r) => r,
        None => host_from_args(&mut cli.subnets)?,
    };
    let remote = transport::normalize_remote(&remote)?;
    match cli.socks5 {
        Some(socks) => run_socks(cli, remote, socks).await,
        None => run_transparent(cli, remote).await,
    }
}

/// Without -r, the first positional argument is the host and the rest are
/// the networks.
fn host_from_args(args: &mut Vec<String>) -> Result<String> {
    let Some(first) = args.first() else {
        bail!("say where to connect: `sshuttle2 user@host` (see --help)");
    };
    if first.contains('/') && first.parse::<Cidr>().is_ok() {
        bail!("missing host: put it first, e.g. `sshuttle2 user@host 10.0.0.0/8`");
    }
    Ok(args.remove(0))
}

fn tunnel_opts(cli: &Cli, remote: &str) -> transport::TunnelOpts {
    transport::TunnelOpts {
        remote: remote.to_string(),
        ssh_cmd: cli.ssh_cmd.clone(),
        server_bin: cli.server_bin.clone(),
        verbose: cli.verbose.saturating_sub(1),
        to_ns: cli.to_ns.clone(),
    }
}

async fn run_socks(cli: Cli, remote: String, socks: SocketAddr) -> Result<()> {
    if !cli.subnets.is_empty() || !cli.exclude.is_empty() || cli.dns {
        warn!("subnets, -x and --dns are ignored in --socks5 mode");
    }
    let listener = TcpListener::bind(socks)
        .await
        .with_context(|| format!("listening on {socks}"))?;

    let tunnel = transport::open(&tunnel_opts(&cli, &remote)).await?;
    info!("connected to {remote}; SOCKS5 proxy on {socks}");
    tracing::debug!(server = ?tunnel.server_hello, "server details");
    let mut child = tunnel.child;
    if cli.verbose > 0 {
        tokio::spawn(report(tunnel.mux.clone()));
    }

    let mux = tunnel.mux.clone();
    tokio::select! {
        r = socks5::serve(listener, mux.clone()) => r?,
        reason = mux.closed() => bail!("tunnel closed: {reason}"),
        st = child.wait() => bail!("ssh exited ({})", st.map(|s| s.to_string()).unwrap_or_default()),
        _ = shutdown_signal() => {
            info!("shutting down");
            mux.close();
            // Let the EXIT frame reach the server before ssh is killed.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Ok(())
}

/// Capture traffic for the given networks with a tun device or the firewall.
async fn run_transparent(cli: Cli, remote: String) -> Result<()> {
    let method = match (cli.method, cli.udp) {
        (Method::Pf | Method::Nft, true) => {
            bail!("--udp needs --method tun (pf and nft cannot carry UDP)")
        }
        (Method::Auto, _) => Method::Tun,
        (m, _) => m,
    };
    // The firewall mode that works where tun does not.
    let firewall = if cfg!(target_os = "macos") {
        "pf"
    } else {
        "nft"
    };
    if method == Method::Pf && !cfg!(target_os = "macos") {
        bail!("--method pf is macOS only; use --method nft or tun");
    }
    if method == Method::Nft && !cfg!(target_os = "linux") {
        bail!("--method nft is Linux only; use --method pf or tun");
    }
    // Say so now, not after the password prompt and the ssh login.
    if method == Method::Nft && helper::which("nft").is_none() {
        bail!("the nft command was not found: install nftables, or use --method tun");
    }
    debug!("capture method: {method:?}");
    let tun = method == Method::Tun;
    // No networks named: everything.
    let subnets = if cli.subnets.is_empty() {
        vec!["0/0".to_string()]
    } else {
        cli.subnets.clone()
    };
    let mut routes = Vec::new();
    for s in &subnets {
        for net in subnet::parse_or_resolve(s)? {
            routes.push(Route {
                net,
                exclude: false,
            });
        }
    }
    // In tun mode "everything" means IPv6 too, or IPv6 traffic would leave
    // outside the tunnel. Without IPv6 on the server it is refused at once
    // and apps use IPv4.
    if tun
        && routes.iter().any(|r| r.net.prefix == 0 && !r.net.is_ipv6())
        && !routes.iter().any(|r| r.net.is_ipv6())
    {
        routes.push(Route {
            net: "::/0".parse()?,
            exclude: false,
        });
    }
    let routed: Vec<String> = routes.iter().map(|r| r.net.to_string()).collect();
    for s in &cli.exclude {
        for net in subnet::parse_or_resolve(s)? {
            routes.push(Route { net, exclude: true });
        }
    }
    let nameservers: Vec<IpAddr> = if cli.dns {
        sysnet::nameservers(method == Method::Nft)
    } else {
        vec![]
    };
    if cli.dns && nameservers.is_empty() {
        warn!("--dns: found no system nameservers to redirect");
    }

    // The sudo prompt comes first, before ssh asks for anything.
    let helper = helper::Helper::start(cli.verbose).await?;
    let listeners = if tun {
        None
    } else {
        Some(transparent::bind(cli.listen, cli.dns).await?)
    };
    let opts = tunnel_opts(&cli, &remote);
    let tunnel = match transport::open(&opts).await {
        Ok(t) => t,
        Err(e) => {
            helper.shutdown().await;
            return Err(e);
        }
    };

    // Keep the tunnel's own connection, and our listeners, out of the tunnel.
    let mut keep_out: Vec<IpAddr> = if tun {
        vec![]
    } else {
        transparent::listen_ips().to_vec()
    };
    if remote != "local" {
        keep_out.extend(sysnet::ssh_destination_ips(
            &cli.ssh_cmd,
            &transport_dest(&remote),
        ));
        if let Some(pid) = tunnel.child.id() {
            keep_out.extend(sysnet::process_tree_peers(pid));
        }
    }
    keep_out.sort();
    keep_out.dedup();
    for ip in &keep_out {
        routes.push(Route {
            net: Cidr::host(*ip),
            exclude: true,
        });
    }
    subnet::sort_routes(&mut routes);
    for r in &routes {
        debug!(
            "{} {}",
            if r.exclude { "exclude" } else { "route  " },
            r.net
        );
    }
    let setup_routes_default = routes.iter().any(|r| !r.exclude && r.net.prefix == 0);
    let setup = helper::Setup {
        routes,
        nameservers: nameservers.clone(),
        tcp_port: listeners.as_ref().map_or(0, |l| l.tcp_port),
        dns_port: listeners.as_ref().map_or(0, |l| l.dns_port),
        fix_loopback: cli.pf_fix_loopback,
        tun_mtu: tun.then_some(cli.tun_mtu),
        // Routing everything, or asked to: system DNS goes through the tunnel.
        tun_dns: tun && (cli.dns || setup_routes_default),
    };
    let tun_dev = if tun {
        match helper.setup_tun(&setup).await {
            Ok((ifname, mtu, fd)) => {
                debug!("tun device {ifname}, MTU {mtu}");
                Some((fd, mtu))
            }
            Err(e) => {
                helper.shutdown().await;
                bail!("setting up the tun device: {e:#}; or use --method {firewall}");
            }
        }
    } else {
        if let Err(e) = helper.setup(&setup).await {
            helper.shutdown().await;
            return Err(e.context("setting up the firewall"));
        }
        None
    };
    let server_ipv6 = tunnel.server_hello.get("ipv6") == Some("1");
    info!(
        "connected to {remote}; routing {}{}{}",
        routed.join(" "),
        if tun { " (TCP and UDP)" } else { " (TCP)" },
        if cli.dns {
            format!(" and DNS ({} servers)", nameservers.len())
        } else {
            String::new()
        }
    );
    if tun && !server_ipv6 {
        info!("the server has no IPv6; IPv6 connections are refused so apps use IPv4");
    }
    if cli.verbose > 0 {
        tokio::spawn(report(tunnel.mux.clone()));
    }

    let helper = Arc::new(helper);
    let mux = tunnel.mux.clone();
    let mut child = tunnel.child;
    let engine = async {
        match (tun_dev, listeners) {
            (Some((dev, mtu)), _) => {
                tunmode::run(
                    dev,
                    mux.clone(),
                    tunmode::Opts {
                        mtu,
                        ipv6: server_ipv6,
                    },
                )
                .await
            }
            (None, Some(l)) => transparent::serve(l, mux.clone(), helper.clone()).await,
            (None, None) => unreachable!("pf mode always has listeners"),
        }
    };
    let res = tokio::select! {
        r = engine => r,
        reason = mux.closed() => Err(anyhow::anyhow!("tunnel closed: {reason}")),
        _ = helper.gone() => Err(anyhow::anyhow!("firewall helper exited; traffic is no longer redirected")),
        st = child.wait() => Err(anyhow::anyhow!("ssh exited ({})", st.map(|s| s.to_string()).unwrap_or_default())),
        _ = shutdown_signal() => {
            info!("shutting down");
            mux.close();
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        }
    };
    // Rules and routes come down before we exit, whatever happened above.
    helper.shutdown().await;
    res
}

/// The ssh destination without a `:port` suffix, for `ssh -G`.
fn transport_dest(remote: &str) -> String {
    let (user, host) = match remote.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, remote),
    };
    let host = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else if host.matches(':').count() == 1 {
        host.split(':').next().unwrap()
    } else {
        host
    };
    match user {
        Some(u) => format!("{u}@{host}"),
        None => host.to_string(),
    }
}

/// Ctrl+C or SIGTERM. Background jobs started by a script inherit SIGINT
/// ignored, so `kill -TERM` must stop us cleanly too.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// With -v: throughput, round-trip time and open channels every 5 seconds.
async fn report(mux: Mux) {
    let period = Duration::from_secs(5);
    let (mut up, mut down) = (0u64, 0u64);
    loop {
        tokio::time::sleep(period).await;
        if mux.is_closed() {
            return;
        }
        let s = mux.stats();
        let (u, d) = (s.bytes_up.load(Relaxed), s.bytes_down.load(Relaxed));
        let mbit = |b: u64| (b as f64 * 8.0) / period.as_secs_f64() / 1e6;
        info!(
            "down {:.1} Mbit/s, up {:.1} Mbit/s, rtt {} ms, {} open channels",
            mbit(d - down),
            mbit(u - up),
            mux.rtt().as_millis(),
            mux.open_channels()
        );
        if tunmode::STATS.from_dev.load(Relaxed) > 0 {
            info!("{}", tunmode::STATS.summary());
        }
        (up, down) = (u, d);
    }
}

/// macOS starts processes with a soft limit of 256 open files, which a busy
/// browser exhausts quickly.
fn raise_fd_limit() {
    // SAFETY: getrlimit and setrlimit only touch `lim`, which outlives both.
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        // macOS refuses values above OPEN_MAX even when the hard limit is
        // "unlimited".
        let cap: libc::rlim_t = if cfg!(target_os = "macos") {
            10_240
        } else {
            65_536
        };
        let want = lim.rlim_max.min(cap);
        if lim.rlim_cur < want {
            lim.rlim_cur = want;
            libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_comes_first() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut a = args(&["user@host", "10/8"]);
        assert_eq!(host_from_args(&mut a).unwrap(), "user@host");
        assert_eq!(a, args(&["10/8"]));
        // A bare address is a host, not a network.
        let mut a = args(&["10.1.2.3"]);
        assert_eq!(host_from_args(&mut a).unwrap(), "10.1.2.3");
        assert!(a.is_empty());
        let mut a = args(&["ssh://host/x"]);
        assert_eq!(host_from_args(&mut a).unwrap(), "ssh://host/x");
        assert!(host_from_args(&mut args(&["0/0"])).is_err());
        assert!(host_from_args(&mut args(&["10.0.0.0/8", "host"])).is_err());
        assert!(host_from_args(&mut vec![]).is_err());
    }
}
