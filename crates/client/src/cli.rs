//! The command line. xtask includes this file too, to generate the man page
//! and shell completions, so it uses nothing from the rest of the client.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

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
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) cmd: Option<Cmd>,

    /// ssh destination: [user@]host[:port], ssh://[user@]host[:port] or an
    /// ssh config alias. Without -r, the first argument is the host.
    /// "local" runs the server as a local process instead (for testing).
    #[arg(short = 'r', long = "remote", value_name = "HOST")]
    pub(crate) remote: Option<String>,

    /// Networks to send through the tunnel, e.g. 10.0.0.0/8 (default:
    /// everything). Without -r, the host comes first.
    #[arg(value_name = "SUBNETS")]
    pub(crate) subnets: Vec<String>,

    /// Networks to keep out of the tunnel. Repeatable.
    #[arg(short = 'x', long = "exclude", value_name = "SUBNET")]
    pub(crate) exclude: Vec<String>,

    /// Send DNS queries to the system's nameservers through the tunnel.
    #[arg(long)]
    pub(crate) dns: bool,

    /// Port for the local redirector (default: first free from 12300 down).
    #[arg(short = 'l', long = "listen", value_name = "PORT")]
    pub(crate) listen: Option<u16>,

    /// If pf is set to skip lo0, load "pass on lo0" for the session and
    /// reload /etc/pf.conf afterwards.
    #[arg(long)]
    pub(crate) pf_fix_loopback: bool,

    /// How to capture traffic: tun for TCP, all UDP, DNS and IPv6 through a
    /// virtual network device, or pf (macOS) or nft (Linux) for TCP and DNS
    /// only, through the firewall.
    #[arg(long, value_enum, default_value_t = Method::Tun)]
    pub(crate) method: Method,

    /// Tunnel UDP too. Does nothing now: tun is the default.
    #[arg(long, hide = true)]
    pub(crate) udp: bool,

    /// MTU of the tun device.
    #[arg(long, default_value_t = 1500, value_name = "BYTES")]
    pub(crate) tun_mtu: u16,

    /// Run a SOCKS5 proxy on this address instead of capturing traffic.
    /// Needs no root.
    #[arg(long, value_name = "ADDR")]
    pub(crate) socks5: Option<SocketAddr>,

    /// Nameserver the remote side should send DNS queries to (IP[:PORT]).
    /// Default: the remote host's /etc/resolv.conf.
    #[arg(long = "to-ns", value_name = "IP[:PORT]")]
    pub(crate) to_ns: Option<String>,

    /// The ssh command to run, with any extra options.
    #[arg(
        short = 'e',
        long = "ssh-cmd",
        default_value = "ssh",
        value_name = "CMD"
    )]
    pub(crate) ssh_cmd: String,

    /// Upload this server binary instead of the built-in one.
    #[arg(long, value_name = "PATH")]
    pub(crate) server_bin: Option<PathBuf>,

    /// More output; repeat for more (-vv, -vvv).
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub(crate) verbose: u8,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    /// Same as tun; kept so old command lines still parse.
    #[value(hide = true)]
    Auto,
    Pf,
    Nft,
    Tun,
}

#[derive(Subcommand, Debug)]
pub(crate) enum Cmd {
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
