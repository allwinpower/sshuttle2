//! Linux nftables: redirect outgoing TCP (and DNS) to our local listeners,
//! the counterpart of pf mode on macOS. Ported from sshuttle's
//! `methods/nft.py`. The client reads each connection's original
//! destination itself (`SO_ORIGINAL_DST`, in `transparent.rs`), so all the
//! root helper does here is load the table and delete it again.

// Only Linux loads the table; `rules` is tested everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::fmt::Write as _;
use std::io::Write as _;
use std::net::IpAddr;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use tracing::{debug, warn};

use crate::subnet::Route;

/// nft script that (re)creates our table. `routes` must already be sorted
/// most-specific-first: a chain runs its rules in order and both `return`
/// and `redirect` end it, so the first match wins, as with pf's `quick`.
///
/// One `inet` table covers IPv4 and IPv6. The script's first two lines
/// create the table if it is missing and then delete it, so a table left by
/// a crashed session on the same port goes in the same atomic transaction
/// that loads ours.
pub fn rules(
    table: &str,
    routes: &[Route],
    tcp_port: u16,
    dns_port: u16,
    nameservers: &[IpAddr],
) -> String {
    let mut capture = String::new();
    // DNS comes before the local-address check, so a local stub resolver
    // (systemd-resolved's 127.0.0.53, Docker's 127.0.0.11) is caught too,
    // as sshuttle does.
    if dns_port != 0 {
        for (ip, v6) in [("ip", false), ("ip6", true)] {
            let ns: Vec<String> = nameservers
                .iter()
                .filter(|a| a.is_ipv6() == v6)
                .map(|a| a.to_string())
                .collect();
            if !ns.is_empty() {
                let _ = writeln!(
                    capture,
                    "\t\t{ip} daddr {{ {} }} udp dport 53 redirect to :{dns_port}",
                    ns.join(", ")
                );
            }
        }
    }
    // Connections to this machine's own addresses stay local, even when an
    // included network covers them (0/0 covers 127.0.0.1).
    capture += "\t\tfib daddr type local return\n";
    for (ip, v6) in [("ip", false), ("ip6", true)] {
        for r in routes.iter().filter(|r| r.net.is_ipv6() == v6) {
            let action = if r.exclude {
                "return".to_string()
            } else {
                format!("redirect to :{tcp_port}")
            };
            let _ = writeln!(
                capture,
                "\t\t{ip} daddr {} meta l4proto tcp {action}",
                r.net
            );
        }
    }
    let mut s = format!(
        "table inet {table}\ndelete table inet {table}\ntable inet {table} {{\n\
         \tchain capture {{\n{capture}\t}}\n"
    );
    // `output` takes this machine's own connections. `prerouting` takes those
    // it forwards (containers, VMs), as in sshuttle; REDIRECT sends them to
    // the incoming interface's address, where the loopback-only redirector
    // does not listen, so they are refused rather than leaving outside the
    // tunnel.
    //
    // Priority -110, ahead of the usual dstnat (-100): of several NAT chains
    // on one hook only the first that maps a connection counts, and Docker's
    // rules for its 127.0.0.11 resolver, or a sshuttle running alongside,
    // would otherwise take it.
    for hook in ["output", "prerouting"] {
        let _ = write!(
            s,
            "\tchain {hook} {{\n\t\ttype nat hook {hook} priority -110; policy accept;\n\
             \t\tjump capture\n\t}}\n"
        );
    }
    s + "}\n"
}

/// Our table, loaded. `teardown` deletes it.
pub struct Nft {
    table: String,
}

impl Nft {
    /// Loads a script from `rules()`, which creates `table`.
    pub fn load(table: String, rules: &str) -> Result<Nft> {
        debug!("nft rules:\n{rules}");
        nft(&["-f", "-"], Some(rules))?;
        Ok(Nft { table })
    }

    pub fn teardown(&mut self) {
        if let Err(e) = delete_table(&self.table) {
            warn!("removing nft table {}: {e:#}", self.table);
        }
    }
}

/// Deletes one of our tables (also used by crash cleanup).
pub fn delete_table(table: &str) -> Result<()> {
    nft(&["delete", "table", "inet", table], None).map(|_| ())
}

fn nft(args: &[&str], stdin: Option<&str>) -> Result<String> {
    debug!("nft {}", args.join(" "));
    let child = Command::new("nft")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("the nft command was not found: install nftables, or use --method tun")
        }
        r => r.context("running nft")?,
    };
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes())?;
    }
    drop(child.stdin.take());
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "nft {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subnet::sort_routes;

    #[test]
    fn rules_text() {
        let mut routes = vec![
            Route {
                net: "0/0".parse().unwrap(),
                exclude: false,
            },
            Route {
                net: "203.0.113.10".parse().unwrap(),
                exclude: true,
            },
            Route {
                net: "10.0.0.0/8".parse().unwrap(),
                exclude: true,
            },
            Route {
                net: "10.1.0.0/16".parse().unwrap(),
                exclude: false,
            },
            Route {
                net: "2001:db8::/32".parse().unwrap(),
                exclude: false,
            },
        ];
        sort_routes(&mut routes);
        let ns = [
            "127.0.0.53".parse().unwrap(),
            "192.0.2.53".parse().unwrap(),
            "2001:4860:4860::8888".parse().unwrap(),
        ];
        let r = rules("sshuttle2-12300", &routes, 12300, 12299, &ns);
        let expected = "\
table inet sshuttle2-12300
delete table inet sshuttle2-12300
table inet sshuttle2-12300 {
\tchain capture {
\t\tip daddr { 127.0.0.53, 192.0.2.53 } udp dport 53 redirect to :12299
\t\tip6 daddr { 2001:4860:4860::8888 } udp dport 53 redirect to :12299
\t\tfib daddr type local return
\t\tip daddr 203.0.113.10/32 meta l4proto tcp return
\t\tip daddr 10.1.0.0/16 meta l4proto tcp redirect to :12300
\t\tip daddr 10.0.0.0/8 meta l4proto tcp return
\t\tip daddr 0.0.0.0/0 meta l4proto tcp redirect to :12300
\t\tip6 daddr 2001:db8::/32 meta l4proto tcp redirect to :12300
\t}
\tchain output {
\t\ttype nat hook output priority -110; policy accept;
\t\tjump capture
\t}
\tchain prerouting {
\t\ttype nat hook prerouting priority -110; policy accept;
\t\tjump capture
\t}
}
";
        assert_eq!(r, expected);
    }

    /// Without --dns there is no DNS listener and no DNS rule.
    #[test]
    fn no_dns_rules_without_dns_port() {
        let routes = [Route {
            net: "10.0.0.0/8".parse().unwrap(),
            exclude: false,
        }];
        let r = rules("t", &routes, 12300, 0, &["8.8.8.8".parse().unwrap()]);
        assert!(!r.contains("udp"), "{r}");
        assert!(r.contains("\t\tfib daddr type local return\n\t\tip daddr 10.0.0.0/8 meta"));
    }
}
