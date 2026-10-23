//! Routing for tun mode (root helper). Included networks point at the tun
//! device; excluded ones, and the ssh server, keep the route they have now,
//! pinned as more specific entries before the tun routes go in.

use std::net::IpAddr;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::{debug, warn};

use crate::subnet::{Cidr, Route};

/// Address of our end of the tun link (RFC 2544 benchmarking range, which
/// real networks do not use).
pub const TUN_V4: &str = "198.18.0.1";
pub const TUN_V6: &str = "fd74:7262:6f00::1";
/// The far end of the link (nothing answers there, but the client answers
/// DNS sent to it): the system resolver's server in tun mode.
pub const TUN_PEER_V4: &str = "198.18.0.2";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Via {
    Gateway(IpAddr),
    Interface(String),
}

/// Routes we added; dropping removes them, newest first.
pub struct TunRoutes {
    added: Vec<(Cidr, Via)>,
}

fn run(prog: &str, args: &[&str]) -> Result<String> {
    debug!("{prog} {}", args.join(" "));
    let out = Command::new(prog)
        .args(args)
        .output()
        .with_context(|| format!("running {prog}"))?;
    if !out.status.success() {
        bail!(
            "{prog} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `0/0` would replace the default route; two halves are more specific and
/// leave it in place for when the tunnel goes away.
pub fn split_default(net: Cidr) -> Vec<Cidr> {
    if net.prefix != 0 {
        return vec![net];
    }
    let halves: [&str; 2] = if net.is_ipv6() {
        ["::/1", "8000::/1"]
    } else {
        ["0.0.0.0/1", "128.0.0.0/1"]
    };
    halves.iter().map(|s| s.parse().unwrap()).collect()
}

#[cfg(target_os = "macos")]
mod os {
    use super::*;

    pub fn configure(ifname: &str, mtu: u16) -> Result<()> {
        let mtu = mtu.to_string();
        run(
            "/sbin/ifconfig",
            &[
                ifname,
                "inet",
                TUN_V4,
                TUN_PEER_V4,
                "netmask",
                "255.255.255.255",
                "mtu",
                &mtu,
                "up",
            ],
        )?;
        if let Err(e) = run(
            "/sbin/ifconfig",
            &[ifname, "inet6", TUN_V6, "prefixlen", "128"],
        ) {
            warn!("no IPv6 on {ifname}: {e:#}");
        }
        Ok(())
    }

    pub fn current_route(ip: IpAddr) -> Result<Via> {
        let fam = if ip.is_ipv6() { "-inet6" } else { "-inet" };
        let out = run("/sbin/route", &["-n", "get", fam, &ip.to_string()])?;
        parse_route_get(&out).with_context(|| format!("no route to {ip}"))
    }

    pub fn parse_route_get(out: &str) -> Option<Via> {
        let field = |name: &str| {
            out.lines()
                .find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim().to_string()))
        };
        if let Some(gw) = field("gateway:")
            && let Ok(ip) = gw.split('%').next().unwrap_or("").parse::<IpAddr>()
        {
            return Some(Via::Gateway(ip));
        }
        field("interface:").map(Via::Interface)
    }

    /// `route -n <verb> -inet|-inet6 -net <net> <gateway | -interface IF>`.
    /// Flags go before the verb: `route add -n` is rejected.
    pub fn args(verb: &str, net: Cidr, via: &Via) -> Vec<String> {
        let fam = if net.is_ipv6() { "-inet6" } else { "-inet" };
        let mut a = vec![
            "-n".into(),
            verb.into(),
            fam.into(),
            "-net".into(),
            net.to_string(),
        ];
        match via {
            Via::Gateway(gw) => a.push(gw.to_string()),
            Via::Interface(i) => {
                a.push("-interface".into());
                a.push(i.clone());
            }
        }
        a
    }

    pub fn add(net: Cidr, via: &Via) -> Result<()> {
        let a = args("add", net, via);
        run(
            "/sbin/route",
            &a.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .map(|_| ())
    }

    pub fn delete(net: Cidr, via: &Via) -> Result<()> {
        let a = args("delete", net, via);
        run(
            "/sbin/route",
            &a.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .map(|_| ())
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::*;

    pub fn configure(ifname: &str, mtu: u16) -> Result<()> {
        match run(
            "ip",
            &["addr", "add", &format!("{TUN_V4}/32"), "dev", ifname],
        ) {
            Err(e) if !format!("{e:#}").contains("File exists") => return Err(e),
            _ => {}
        }
        if let Err(e) = run(
            "ip",
            &["-6", "addr", "add", &format!("{TUN_V6}/128"), "dev", ifname],
        ) {
            warn!("no IPv6 on {ifname}: {e:#}");
        }
        run(
            "ip",
            &["link", "set", ifname, "mtu", &mtu.to_string(), "up"],
        )?;
        Ok(())
    }

    pub fn current_route(ip: IpAddr) -> Result<Via> {
        let out = run("ip", &["route", "get", &ip.to_string()])?;
        parse_route_get(&out).with_context(|| format!("no route to {ip}"))
    }

    /// `1.1.1.1 via 10.0.0.1 dev eth0 src ...` or `10.0.0.5 dev eth0 src ...`.
    pub fn parse_route_get(out: &str) -> Option<Via> {
        let words: Vec<&str> = out.split_whitespace().collect();
        let after = |k: &str| {
            words
                .iter()
                .position(|w| *w == k)
                .and_then(|i| words.get(i + 1))
        };
        if let Some(gw) = after("via")
            && let Ok(ip) = gw.parse()
        {
            return Some(Via::Gateway(ip));
        }
        after("dev").map(|d| Via::Interface(d.to_string()))
    }

    fn args(verb: &str, net: Cidr, via: &Via) -> Vec<String> {
        let mut a = vec![
            if net.is_ipv6() { "-6" } else { "-4" }.to_string(),
            "route".into(),
            verb.into(),
            net.to_string(),
        ];
        match via {
            Via::Gateway(gw) => {
                a.push("via".into());
                a.push(gw.to_string());
            }
            Via::Interface(i) => {
                a.push("dev".into());
                a.push(i.clone());
            }
        }
        a
    }

    pub fn add(net: Cidr, via: &Via) -> Result<()> {
        let a = args("add", net, via);
        run("ip", &a.iter().map(String::as_str).collect::<Vec<_>>()).map(|_| ())
    }

    pub fn delete(net: Cidr, via: &Via) -> Result<()> {
        let a = args("del", net, via);
        run("ip", &a.iter().map(String::as_str).collect::<Vec<_>>()).map(|_| ())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod os {
    use super::*;
    pub fn configure(_: &str, _: u16) -> Result<()> {
        bail!("unsupported OS")
    }
    pub fn current_route(_: IpAddr) -> Result<Via> {
        bail!("unsupported OS")
    }
    pub fn add(_: Cidr, _: &Via) -> Result<()> {
        bail!("unsupported OS")
    }
    pub fn delete(_: Cidr, _: &Via) -> Result<()> {
        bail!("unsupported OS")
    }
}

/// Addresses the device and brings it up. If the kernel refuses `mtu`,
/// falls back to 1500. Returns the MTU in effect.
pub fn configure(ifname: &str, mtu: u16) -> Result<u16> {
    match os::configure(ifname, mtu) {
        Ok(()) => Ok(mtu),
        Err(e) if mtu != 1500 => {
            warn!("MTU {mtu} refused ({e:#}); using 1500");
            os::configure(ifname, 1500)?;
            Ok(1500)
        }
        Err(e) => Err(e),
    }
}

impl TunRoutes {
    pub fn empty() -> TunRoutes {
        TunRoutes { added: Vec::new() }
    }

    /// Pins exclusions to their current next hop, then points included
    /// networks and `extra_into_tun` (DNS servers) at `ifname`. On error,
    /// whatever was added is removed again.
    pub fn install(ifname: &str, routes: &[Route], extra_into_tun: &[IpAddr]) -> Result<TunRoutes> {
        let mut t = TunRoutes { added: Vec::new() };
        for r in routes.iter().filter(|r| r.exclude) {
            match os::current_route(r.net.ip) {
                Ok(Via::Interface(i)) if i == ifname => {}
                Ok(via) => t.add(r.net, via)?,
                Err(e) => warn!("keeping {} out of the tunnel: {e:#}", r.net),
            }
        }
        for r in routes.iter().filter(|r| !r.exclude) {
            for net in split_default(r.net) {
                t.add(net, Via::Interface(ifname.to_string()))?;
            }
        }
        for ip in extra_into_tun {
            t.add(Cidr::host(*ip), Via::Interface(ifname.to_string()))?;
        }
        Ok(t)
    }

    fn add(&mut self, net: Cidr, via: Via) -> Result<()> {
        // A route that already exists (say, the same exclusion twice) is fine.
        match os::add(net, &via) {
            Ok(()) => {
                self.added.push((net, via));
                Ok(())
            }
            Err(e) if format!("{e:#}").contains("File exists") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Commands that undo these routes, for the crash-cleanup record.
    pub fn records(&self) -> Vec<String> {
        self.added
            .iter()
            .map(|(net, via)| match via {
                Via::Gateway(g) => format!("route {net} gw {g}"),
                Via::Interface(i) => format!("route {net} if {i}"),
            })
            .collect()
    }
}

impl Drop for TunRoutes {
    fn drop(&mut self) {
        while let Some((net, via)) = self.added.pop() {
            if let Err(e) = os::delete(net, &via) {
                // Routes through the tun device vanish with it anyway.
                debug!("removing route {net}: {e:#}");
            }
        }
    }
}

/// Points the system resolver at the far end of the link. Dropping it
/// undoes that.
///
/// macOS: registers the tun device as a network service whose DNS server
/// is the far end of the link, and makes it the primary service.
/// mDNSResponder ignores DNS answers that arrive on an interface no service
/// owns, so without this the system resolver fails in tun mode even though
/// the DNS packets themselves get through. (OpenConnect's vpnc-script does
/// the same.)
///
/// Linux: tells systemd-resolved to use that server for every domain (`~.`)
/// on the tun device. Its own upstream servers are usually on the LAN, whose
/// route is more specific than the tunnel's, so queries would otherwise
/// leave outside it.
pub struct SystemDns {
    /// macOS: the configd key; Linux: the interface.
    key: String,
}

impl SystemDns {
    #[cfg(target_os = "macos")]
    pub fn publish(ifname: &str) -> Result<SystemDns> {
        let key = format!("State:/Network/Service/sshuttle2-{ifname}");
        let peer = TUN_PEER_V4;
        let script = format!(
            "d.init\nd.add Addresses * {TUN_V4}\nd.add DestAddresses * {peer}\n\
             d.add InterfaceName {ifname}\nd.add Router {peer}\nd.add OverridePrimary # 1\n\
             set {key}/IPv4\nd.init\nd.add ServerAddresses * {peer}\nset {key}/DNS\nquit\n"
        );
        scutil(&script)?;
        Ok(SystemDns { key })
    }

    #[cfg(target_os = "linux")]
    pub fn publish(ifname: &str) -> Result<SystemDns> {
        let resolvectl = crate::helper::which("resolvectl")
            .context("resolvectl not found (systemd-resolved is not in use)")?;
        let resolvectl = resolvectl.to_string_lossy();
        run(&resolvectl, &["dns", ifname, TUN_PEER_V4])?;
        // From here on, dropping it reverts what is set.
        let d = SystemDns {
            key: ifname.to_string(),
        };
        run(&resolvectl, &["domain", ifname, "~."])?;
        Ok(d)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn publish(_ifname: &str) -> Result<SystemDns> {
        bail!("system DNS registration is not supported on this OS")
    }

    /// The crash-cleanup record that undoes this.
    pub fn record(&self) -> String {
        if cfg!(target_os = "macos") {
            format!("scutil {}", self.key)
        } else {
            format!("resolvectl {}", self.key)
        }
    }
}

impl Drop for SystemDns {
    fn drop(&mut self) {
        if cfg!(target_os = "macos") {
            remove_system_dns(&self.key);
        } else {
            revert_resolvectl(&self.key);
        }
    }
}

/// Drops the DNS settings `SystemDns::publish` gave a link (also used by
/// crash cleanup, when the device is usually gone and resolved has
/// forgotten them already).
pub fn revert_resolvectl(ifname: &str) {
    if let Err(e) = run("resolvectl", &["revert", ifname]) {
        debug!("resolvectl revert {ifname}: {e:#}");
    }
}

fn scutil(script: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("/usr/sbin/scutil")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .context("running scutil")?;
    child.stdin.take().unwrap().write_all(script.as_bytes())?;
    let st = child.wait()?;
    if !st.success() {
        bail!("scutil failed: {st}");
    }
    Ok(())
}

/// Removes a service registered by `SystemDns::publish` (also used by
/// crash cleanup).
pub fn remove_system_dns(key: &str) {
    if let Err(e) = scutil(&format!("remove {key}/IPv4\nremove {key}/DNS\nquit\n")) {
        warn!("removing {key}: {e:#}");
    }
}

/// Removes one route from a crash-cleanup record line.
pub fn delete_recorded(line: &str) {
    let w: Vec<&str> = line.split_whitespace().collect();
    if let [net, kind, target] = w[..]
        && let Ok(net) = net.parse::<Cidr>()
    {
        let via = match kind {
            "gw" => match target.parse() {
                Ok(ip) => Via::Gateway(ip),
                Err(_) => return,
            },
            _ => Via::Interface(target.to_string()),
        };
        let _ = os::delete(net, &via);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_defaults() {
        let s = |x: &str| {
            split_default(x.parse().unwrap())
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(s("0/0"), ["0.0.0.0/1", "128.0.0.0/1"]);
        assert_eq!(s("::/0"), ["::/1", "8000::/1"]);
        assert_eq!(s("10/8"), ["10.0.0.0/8"]);
    }

    /// The user's first live run failed on `route add -q -n ...`: macOS
    /// wants flags before the verb. (Unprivileged `route` checks for root
    /// before parsing, so the real binary cannot vouch for the syntax here.)
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_flags_before_verb() {
        let a = os::args(
            "add",
            "0.0.0.0/1".parse().unwrap(),
            &Via::Interface("utun4".into()),
        );
        assert_eq!(
            a,
            [
                "-n",
                "add",
                "-inet",
                "-net",
                "0.0.0.0/1",
                "-interface",
                "utun4"
            ]
        );
        let a = os::args(
            "delete",
            "1.2.3.4/32".parse().unwrap(),
            &Via::Gateway("10.0.0.1".parse().unwrap()),
        );
        assert_eq!(
            a,
            ["-n", "delete", "-inet", "-net", "1.2.3.4/32", "10.0.0.1"]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_macos_route_get() {
        let out = "   route to: 203.0.113.10\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>\n";
        assert_eq!(
            os::parse_route_get(out),
            Some(Via::Gateway("192.168.1.1".parse().unwrap()))
        );
        let onlink = "   route to: 192.168.1.20\ndestination: 192.168.1.0\n  interface: en0\n";
        assert_eq!(
            os::parse_route_get(onlink),
            Some(Via::Interface("en0".into()))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_linux_route_get() {
        assert_eq!(
            os::parse_route_get("1.1.1.1 via 10.0.0.1 dev eth0 src 10.0.0.5 uid 0\n    cache\n"),
            Some(Via::Gateway("10.0.0.1".parse().unwrap()))
        );
        assert_eq!(
            os::parse_route_get("10.0.0.7 dev eth0 src 10.0.0.5 uid 0\n"),
            Some(Via::Interface("eth0".into()))
        );
    }
}
