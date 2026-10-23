//! macOS pf: redirect outgoing TCP (and DNS) to our local listeners, and
//! recover each connection's original destination. Ported from sshuttle's
//! `methods/pf.py` (Darwin variant). Everything here except `rules` runs in
//! the root helper.

// Only macOS loads pf rules; `rules` is tested everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::fmt::Write as _;
use std::net::IpAddr;

use crate::subnet::Route;

/// pf ruleset for our anchor. Order matters to pf: tables, then translation
/// (`rdr`), then filtering. Filtering rules are `quick`, so the first match
/// wins; `routes` must already be sorted most-specific-first.
///
/// Outgoing packets for included networks are `route-to lo0`; on lo0 they
/// meet the `rdr` rule and land on our listener, where DIOCNATLOOK recovers
/// where they were going.
pub fn rules(routes: &[Route], tcp_port: u16, dns_port: u16, nameservers: &[IpAddr]) -> String {
    let mut tables = String::new();
    let mut rdr = String::new();
    let mut filter = String::new();
    for (inet, lo) in [("inet", "127.0.0.1"), ("inet6", "::1")] {
        let v6 = inet == "inet6";
        let fam: Vec<&Route> = routes.iter().filter(|r| r.net.is_ipv6() == v6).collect();
        let ns: Vec<String> = nameservers
            .iter()
            .filter(|ip| ip.is_ipv6() == v6)
            .map(|ip| ip.to_string())
            .collect();
        for r in &fam {
            if !r.exclude {
                let _ = writeln!(
                    rdr,
                    "rdr pass on lo0 {inet} proto tcp from ! {lo} to {} -> {lo} port {tcp_port}",
                    r.net
                );
            }
        }
        // `quick` matters: other products (e.g. Cloudflare WARP) add anchors
        // with their own quick rules that would otherwise decide instead.
        for r in &fam {
            if r.exclude {
                let _ = writeln!(filter, "pass out quick {inet} proto tcp to {}", r.net);
            } else {
                let _ = writeln!(
                    filter,
                    "pass out quick route-to lo0 {inet} proto tcp to {} keep state",
                    r.net
                );
            }
        }
        if !ns.is_empty() && dns_port != 0 {
            let table = if v6 { "dns_servers6" } else { "dns_servers" };
            let _ = writeln!(tables, "table <{table}> {{{}}}", ns.join(","));
            let _ = writeln!(
                rdr,
                "rdr pass on lo0 {inet} proto udp to <{table}> port 53 -> {lo} port {dns_port}"
            );
            let _ = writeln!(
                filter,
                "pass out quick route-to lo0 {inet} proto udp to <{table}> port 53 keep state"
            );
        }
    }
    tables + &rdr + &filter
}

#[cfg(target_os = "macos")]
pub use darwin::*;

#[cfg(target_os = "macos")]
mod darwin {
    use std::io::Write;
    use std::net::{IpAddr, SocketAddr};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::{Command, Stdio};

    use anyhow::{Context, Result, bail};
    use tracing::{debug, warn};

    // Darwin struct sizes and offsets (xnu bsd/net/pfvar.h), as sshuttle uses.
    const PFIOC_NATLOOK_SIZE: usize = 84;
    const PFIOC_RULE_SIZE: usize = 3104;
    const PFIOC_POOLADDR_SIZE: usize = 1136;
    const RULE_ACTION_OFFSET: usize = 3068;
    const ANCHOR_CALL_OFFSET: usize = 1040;
    const POOL_TICKET_OFFSET: usize = 8;
    const ACTION_OFFSET: usize = 0;
    const MAXPATHLEN: usize = 1024;

    const PF_CHANGE_ADD_TAIL: u32 = 2;
    const PF_CHANGE_GET_TICKET: u32 = 6;
    const PF_PASS: u32 = 0;
    const PF_RDR: u32 = 8;
    const PF_OUT: u8 = 2;

    /// `_IOWR('D', nr, size)`.
    const fn iowr(nr: u64, size: usize) -> u64 {
        0x4000_0000 | 0x8000_0000 | (((size as u64) & 0x1fff) << 16) | ((b'D' as u64) << 8) | nr
    }
    pub const DIOCNATLOOK: u64 = iowr(23, PFIOC_NATLOOK_SIZE);
    pub const DIOCCHANGERULE: u64 = iowr(26, PFIOC_RULE_SIZE);
    pub const DIOCBEGINADDRS: u64 = iowr(51, PFIOC_POOLADDR_SIZE);

    #[repr(C, align(4))]
    struct Buf<const N: usize>([u8; N]);

    impl<const N: usize> Buf<N> {
        fn zeroed() -> Box<Self> {
            Box::new(Buf([0u8; N]))
        }
    }

    fn ioctl<const N: usize>(fd: &OwnedFd, req: u64, buf: &mut Buf<N>) -> std::io::Result<()> {
        let r = unsafe { libc::ioctl(fd.as_raw_fd(), req as libc::c_ulong, buf.0.as_mut_ptr()) };
        if r < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub struct Pf {
        dev: OwnedFd,
        anchor: String,
        /// Reference tokens from `pfctl -E`; each is released with `-X`.
        tokens: Vec<String>,
        restore_main_ruleset: bool,
    }

    fn pfctl(args: &[&str], stdin: Option<&str>) -> Result<(String, String)> {
        debug!("pfctl {}", args.join(" "));
        let mut child = Command::new("/sbin/pfctl")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running pfctl")?;
        if let Some(input) = stdin {
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
        }
        drop(child.stdin.take());
        let out = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if !out.status.success() {
            bail!(
                "pfctl {} failed ({}): {}",
                args.join(" "),
                out.status,
                stderr.trim()
            );
        }
        Ok((stdout, stderr))
    }

    impl Pf {
        pub fn open(anchor: String) -> Result<Pf> {
            let fd = unsafe { libc::open(c"/dev/pf".as_ptr(), libc::O_RDWR) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error()).context("opening /dev/pf");
            }
            Ok(Pf {
                dev: unsafe { OwnedFd::from_raw_fd(fd) },
                anchor,
                tokens: Vec::new(),
                restore_main_ruleset: false,
            })
        }

        /// Loads `rules` into our anchor, hooks the anchor into the main
        /// ruleset, and enables pf.
        pub fn setup(&mut self, rules: &str, fix_loopback: bool) -> Result<()> {
            let (status, _) = pfctl(&["-s", "all"], None)?;
            if has_skip_loopback()? {
                if !fix_loopback {
                    bail!(
                        "pf is set to skip lo0, so redirected connections would never \
                         reach sshuttle2. Rerun with --pf-fix-loopback: it loads \
                         'pass on lo0' into the main ruleset for the session and \
                         reloads /etc/pf.conf afterwards."
                    );
                }
                warn!("pf skips lo0; loading 'pass on lo0' until exit");
                pfctl(&["-f", "/dev/stdin"], Some("pass on lo0\n"))?;
                self.restore_main_ruleset = true;
            }
            let a = &self.anchor;
            if !status.contains(&format!("\nrdr-anchor \"{a}\"")) {
                self.add_anchor_rule(PF_RDR)?;
            }
            if !status.contains(&format!("\nanchor \"{a}\"")) {
                self.add_anchor_rule(PF_PASS)?;
            }
            debug!("pf rules:\n{rules}");
            pfctl(&["-a", a, "-f", "/dev/stdin"], Some(rules))?;
            let (_, stderr) = pfctl(&["-E"], None)?;
            let token = stderr
                .lines()
                .find_map(|l| l.split_once("Token : ").map(|(_, t)| t.trim().to_string()))
                .context("pfctl -E printed no token")?;
            self.tokens.push(token);
            Ok(())
        }

        /// Appends an `anchor` (PF_PASS) or `rdr-anchor` (PF_RDR) rule naming
        /// our anchor to the main ruleset, without reloading pf.conf.
        fn add_anchor_rule(&self, kind: u32) -> Result<()> {
            let mut ppa = Buf::<PFIOC_POOLADDR_SIZE>::zeroed();
            ioctl(&self.dev, DIOCBEGINADDRS, &mut ppa).context("DIOCBEGINADDRS")?;
            let mut pr = Buf::<PFIOC_RULE_SIZE>::zeroed();
            let ticket: [u8; 4] = ppa.0[4..8].try_into().unwrap();
            pr.0[POOL_TICKET_OFFSET..POOL_TICKET_OFFSET + 4].copy_from_slice(&ticket);
            let name = self.anchor.as_bytes();
            let n = name.len().min(MAXPATHLEN - 1);
            pr.0[ANCHOR_CALL_OFFSET..ANCHOR_CALL_OFFSET + n].copy_from_slice(&name[..n]);
            pr.0[RULE_ACTION_OFFSET..RULE_ACTION_OFFSET + 4].copy_from_slice(&kind.to_ne_bytes());
            for action in [PF_CHANGE_GET_TICKET, PF_CHANGE_ADD_TAIL] {
                pr.0[ACTION_OFFSET..ACTION_OFFSET + 4].copy_from_slice(&action.to_ne_bytes());
                ioctl(&self.dev, DIOCCHANGERULE, &mut pr).context("DIOCCHANGERULE")?;
            }
            Ok(())
        }

        /// Where a redirected connection was originally headed.
        pub fn natlook(&self, peer: SocketAddr, local: SocketAddr) -> Result<SocketAddr> {
            if peer.is_ipv4() != local.is_ipv4() {
                bail!("address family mismatch");
            }
            let mut b = Buf::<PFIOC_NATLOOK_SIZE>::zeroed();
            put_addr(&mut b.0[0..16], peer.ip());
            put_addr(&mut b.0[16..32], local.ip());
            b.0[64..66].copy_from_slice(&peer.port().to_be_bytes());
            b.0[68..70].copy_from_slice(&local.port().to_be_bytes());
            b.0[80] = if peer.is_ipv4() {
                libc::AF_INET as u8
            } else {
                libc::AF_INET6 as u8
            };
            b.0[81] = libc::IPPROTO_TCP as u8;
            b.0[83] = PF_OUT;
            ioctl(&self.dev, DIOCNATLOOK, &mut b).context("DIOCNATLOOK")?;
            let port = u16::from_be_bytes([b.0[76], b.0[77]]);
            let ip: IpAddr = if peer.is_ipv4() {
                <[u8; 4]>::try_from(&b.0[48..52]).unwrap().into()
            } else {
                <[u8; 16]>::try_from(&b.0[48..64]).unwrap().into()
            };
            Ok(SocketAddr::new(ip, port))
        }

        /// Removes our rules and releases our hold on pf. Each step is tried
        /// even if an earlier one fails.
        pub fn teardown(&mut self) {
            if let Err(e) = pfctl(&["-a", &self.anchor, "-F", "all"], None) {
                warn!("flushing pf anchor: {e:#}");
            }
            while let Some(t) = self.tokens.pop() {
                if let Err(e) = pfctl(&["-X", &t], None) {
                    warn!("releasing pf token: {e:#}");
                }
            }
            if self.restore_main_ruleset {
                if let Err(e) = pfctl(&["-f", "/etc/pf.conf"], None) {
                    warn!("reloading /etc/pf.conf: {e:#}");
                }
                self.restore_main_ruleset = false;
            }
        }

        pub fn tokens(&self) -> &[String] {
            &self.tokens
        }
    }

    fn put_addr(dst: &mut [u8], ip: IpAddr) {
        match ip {
            IpAddr::V4(v4) => dst[..4].copy_from_slice(&v4.octets()),
            IpAddr::V6(v6) => dst[..16].copy_from_slice(&v6.octets()),
        }
    }

    fn has_skip_loopback() -> Result<bool> {
        let (out, _) = pfctl(&["-s", "Interfaces", "-i", "lo0", "-v"], None)?;
        Ok(out.contains("skip"))
    }

    /// sshuttle (or sshuttle2 pf mode) anchors whose rules redirect TCP
    /// to lo0. While loaded, they take TCP before the routing table does.
    /// The prefix matches both: `sshuttle-*` and `sshuttle2-*`.
    pub fn anchors_capturing_tcp() -> Vec<String> {
        let Ok((list, _)) = pfctl(&["-s", "Anchors"], None) else {
            return vec![];
        };
        list.split_whitespace()
            .filter(|a| a.starts_with("sshuttle"))
            .filter(|a| {
                pfctl(&["-a", a, "-s", "rules"], None)
                    .map(|(r, _)| r.contains("route-to lo0") && r.contains("proto tcp"))
                    .unwrap_or(false)
            })
            .map(str::to_string)
            .collect()
    }

    /// Flushes an anchor left by a crashed session and releases its token.
    pub fn cleanup_stale(anchor: &str, tokens: &[String]) {
        let _ = pfctl(&["-a", anchor, "-F", "all"], None);
        for t in tokens {
            let _ = pfctl(&["-X", t], None);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Values computed by sshuttle 2.0.0's ctypes definitions.
        #[test]
        fn ioctl_numbers_match_sshuttle() {
            assert_eq!(DIOCNATLOOK, 0xc054_4417);
            assert_eq!(DIOCCHANGERULE, 0xcc20_441a);
            assert_eq!(DIOCBEGINADDRS, 0xc470_4433);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subnet::{Route, sort_routes};

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
                net: "2001:db8::/32".parse().unwrap(),
                exclude: false,
            },
        ];
        sort_routes(&mut routes);
        let ns = ["192.0.2.53".parse().unwrap(), "192.0.2.52".parse().unwrap()];
        let r = rules(&routes, 12300, 12299, &ns);
        let expected = "\
table <dns_servers> {192.0.2.53,192.0.2.52}
rdr pass on lo0 inet proto tcp from ! 127.0.0.1 to 0.0.0.0/0 -> 127.0.0.1 port 12300
rdr pass on lo0 inet proto udp to <dns_servers> port 53 -> 127.0.0.1 port 12299
rdr pass on lo0 inet6 proto tcp from ! ::1 to 2001:db8::/32 -> ::1 port 12300
pass out quick inet proto tcp to 203.0.113.10/32
pass out quick route-to lo0 inet proto tcp to 0.0.0.0/0 keep state
pass out quick route-to lo0 inet proto udp to <dns_servers> port 53 keep state
pass out quick route-to lo0 inet6 proto tcp to 2001:db8::/32 keep state
";
        assert_eq!(r, expected);
    }
}
