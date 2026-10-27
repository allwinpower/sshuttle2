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
    /// A gateway, and the device it is on when known. A link-local IPv6
    /// gateway (`fe80::1`, as router advertisements give) means nothing
    /// without its device: the kernel refuses such a route.
    Gateway(IpAddr, Option<String>),
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
        // A link-local gateway comes with its scope: `fe80::1%en0`.
        if let Some(gw) = field("gateway:") {
            let (ip, scope) = match gw.split_once('%') {
                Some((ip, scope)) => (ip, Some(scope.to_string())),
                None => (gw.as_str(), None),
            };
            if let Ok(ip) = ip.parse::<IpAddr>() {
                return Some(Via::Gateway(ip, scope));
            }
        }
        field("interface:").map(Via::Interface)
    }

    /// `route -n <verb> -inet|-inet6 -net <net> <gateway | -interface IF>`.
    /// Flags go before the verb: `route add -n` is rejected. An IPv6
    /// gateway's device goes in as its scope (`fe80::1%en0`); `-ifscope`
    /// would instead make a route only sockets bound to that device use.
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
            Via::Gateway(IpAddr::V6(gw), Some(dev)) => a.push(format!("{gw}%{dev}")),
            Via::Gateway(gw, _) => a.push(gw.to_string()),
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

/// Linux: rtnetlink, straight to the kernel, so tun mode needs no iproute2.
/// Only the route and address types of netlink-packet-route are used: its
/// generic message type would pull in decoders for every rtnetlink message
/// (links, traffic control, ...), about 1 MB of the static binary.
#[cfg(target_os = "linux")]
mod os {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::io;
    use std::os::fd::AsRawFd;

    use netlink_packet_core::{
        DecodeError, DefaultNla, Emitable, ErrorBuffer, NLM_F_ACK, NLM_F_CREATE, NLM_F_EXCL,
        NLM_F_REQUEST, NLMSG_ERROR, NetlinkBuffer, NetlinkHeader, Parseable,
    };
    use netlink_packet_route::AddressFamily as Family;
    use netlink_packet_route::address::{AddressAttribute, AddressMessage};
    use netlink_packet_route::link::{LinkFlags, LinkHeader};
    use netlink_packet_route::route::{
        RouteAddress, RouteAttribute, RouteHeader, RouteMessage, RouteProtocol, RouteScope,
        RouteType,
    };
    use nix::sys::socket::{
        AddressFamily, MsgFlags, NetlinkAddr, SockFlag, SockProtocol, SockType, recv, sendto,
        socket,
    };

    const IFLA_MTU: u16 = 4;

    pub fn configure(ifname: &str, mtu: u16) -> Result<()> {
        debug!("netlink: {TUN_V4}/32 and {TUN_V6}/128 on {ifname}, MTU {mtu}, up");
        let index = index(ifname)?;
        add_address(index, TUN_V4.parse()?, 32)
            .with_context(|| format!("adding {TUN_V4}/32 to {ifname}"))?;
        if let Err(e) = add_address(index, TUN_V6.parse()?, 128) {
            warn!("no IPv6 on {ifname}: {e}");
        }
        let link = LinkHeader {
            index,
            flags: LinkFlags::Up,
            change_mask: LinkFlags::Up,
            ..Default::default()
        };
        let mtu_attr = [DefaultNla::new(
            IFLA_MTU,
            u32::from(mtu).to_ne_bytes().into(),
        )];
        request(libc::RTM_NEWLINK, 0, &[&link, &&mtu_attr[..]])
            .with_context(|| format!("setting MTU {mtu} on {ifname} and bringing it up"))?;
        Ok(())
    }

    /// An address the device already has (a second run) counts as added.
    fn add_address(index: u32, ip: IpAddr, prefix: u8) -> io::Result<()> {
        let mut m = AddressMessage::default();
        m.header.family = family(ip);
        m.header.prefix_len = prefix;
        m.header.index = index;
        m.attributes = vec![AddressAttribute::Local(ip), AddressAttribute::Address(ip)];
        match request(libc::RTM_NEWADDR, NLM_F_CREATE | NLM_F_EXCL, &[&m]) {
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => Ok(()),
            r => r.map(drop),
        }
    }

    pub fn current_route(ip: IpAddr) -> Result<Via> {
        let mut m = RouteMessage::default();
        m.header.address_family = family(ip);
        m.header.destination_prefix_length = Cidr::host(ip).prefix;
        m.attributes.push(RouteAttribute::Destination(ip.into()));
        let routes = request(libc::RTM_GETROUTE, 0, &[&m])
            .with_context(|| format!("looking up the route to {ip}"))?;
        let via = routes.iter().find_map(|r| via_of(r, name));
        debug!("netlink: route to {ip}: {via:?}");
        via.with_context(|| format!("no route to {ip}"))
    }

    /// The next hop in the kernel's answer to a route lookup: its gateway
    /// and device, or else its device (`name` turns the index into a name).
    pub fn via_of(r: &RouteMessage, name: impl Fn(u32) -> Option<String>) -> Option<Via> {
        let (mut gateway, mut oif) = (None, None);
        for a in &r.attributes {
            match a {
                RouteAttribute::Gateway(RouteAddress::Inet(g)) => gateway = Some((*g).into()),
                RouteAttribute::Gateway(RouteAddress::Inet6(g)) => gateway = Some((*g).into()),
                RouteAttribute::Oif(i) => oif = Some(*i),
                _ => {}
            }
        }
        let dev = oif.and_then(name);
        match gateway {
            Some(g) => Some(Via::Gateway(g, dev)),
            None => dev.map(Via::Interface),
        }
    }

    /// What `ip route add|del ... [via G] [dev D]` sends: the main table; on
    /// adding, a unicast route of protocol "boot", scope link when it names
    /// only a device.
    pub fn route_message(net: Cidr, via: &Via, add: bool) -> Result<RouteMessage> {
        let mut m = RouteMessage::default();
        let h = &mut m.header;
        h.address_family = family(net.ip);
        h.destination_prefix_length = net.prefix;
        h.table = RouteHeader::RT_TABLE_MAIN;
        if add {
            h.protocol = RouteProtocol::Boot;
            h.kind = RouteType::Unicast;
            if matches!(via, Via::Interface(_)) {
                h.scope = RouteScope::Link;
            }
        } else {
            // Matches the route whatever its scope.
            h.scope = RouteScope::NoWhere;
        }
        if net.prefix > 0 {
            m.attributes
                .push(RouteAttribute::Destination(net.ip.into()));
        }
        let dev = match via {
            Via::Gateway(g, dev) => {
                m.attributes.push(RouteAttribute::Gateway((*g).into()));
                dev.as_deref()
            }
            Via::Interface(i) => Some(i.as_str()),
        };
        if let Some(dev) = dev {
            m.attributes.push(RouteAttribute::Oif(index(dev)?));
        }
        Ok(m)
    }

    fn describe(via: &Via) -> String {
        match via {
            Via::Gateway(g, Some(d)) => format!("via {g} dev {d}"),
            Via::Gateway(g, None) => format!("via {g}"),
            Via::Interface(i) => format!("dev {i}"),
        }
    }

    pub fn add(net: Cidr, via: &Via) -> Result<()> {
        debug!("netlink: route add {net} {}", describe(via));
        route_message(net, via, true)
            .and_then(|m| {
                request(libc::RTM_NEWROUTE, NLM_F_CREATE | NLM_F_EXCL, &[&m])?;
                Ok(())
            })
            .with_context(|| format!("adding route {net} {}", describe(via)))
    }

    pub fn delete(net: Cidr, via: &Via) -> Result<()> {
        debug!("netlink: route del {net} {}", describe(via));
        route_message(net, via, false)
            .and_then(|m| {
                request(libc::RTM_DELROUTE, 0, &[&m])?;
                Ok(())
            })
            .with_context(|| format!("removing route {net} {}", describe(via)))
    }

    fn family(ip: IpAddr) -> Family {
        if ip.is_ipv6() {
            Family::Inet6
        } else {
            Family::Inet
        }
    }

    fn index(ifname: &str) -> Result<u32> {
        let c = CString::new(ifname)?;
        // SAFETY: `c` is a NUL-terminated string that outlives the call.
        match unsafe { libc::if_nametoindex(c.as_ptr()) } {
            0 => Err(io::Error::last_os_error()).with_context(|| format!("no device {ifname}")),
            i => Ok(i),
        }
    }

    fn name(index: u32) -> Option<String> {
        let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
        // SAFETY: `buf` is IF_NAMESIZE bytes, as if_indextoname requires.
        let p = unsafe { libc::if_indextoname(index, buf.as_mut_ptr()) };
        if p.is_null() {
            return None;
        }
        // SAFETY: not null, so `p` points into `buf`, which now holds a
        // NUL-terminated name.
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }

    /// A request of type `kind` (RTM_*) as sent, asking the kernel to
    /// acknowledge it. One socket per request, so the sequence number is
    /// always 1.
    pub fn encode(kind: u16, flags: u16, body: &[&dyn Emitable]) -> Vec<u8> {
        let mut h = NetlinkHeader::default();
        h.length = (h.buffer_len() + body.iter().map(|b| b.buffer_len()).sum::<usize>()) as u32;
        h.message_type = kind;
        h.flags = NLM_F_REQUEST | NLM_F_ACK | flags;
        h.sequence_number = 1;
        let mut buf = vec![0; h.length as usize];
        let mut at = 0;
        for part in [&h as &dyn Emitable].iter().chain(body) {
            part.emit(&mut buf[at..at + part.buffer_len()]);
            at += part.buffer_len();
        }
        buf
    }

    /// Sends one request and waits for its acknowledgement. Returns the
    /// routes the kernel answered with before that (a lookup's result).
    fn request(kind: u16, flags: u16, body: &[&dyn Emitable]) -> io::Result<Vec<RouteMessage>> {
        let fd = socket(
            AddressFamily::Netlink,
            SockType::Raw,
            SockFlag::SOCK_CLOEXEC,
            SockProtocol::NetlinkRoute,
        )?;
        let kernel = NetlinkAddr::new(0, 0);
        sendto(
            fd.as_raw_fd(),
            &encode(kind, flags, body),
            &kernel,
            MsgFlags::empty(),
        )?;
        let mut routes = Vec::new();
        let mut buf = vec![0; 32 * 1024];
        loop {
            let n = recv(fd.as_raw_fd(), &mut buf, MsgFlags::empty())?;
            if take_replies(&buf[..n], &mut routes)? {
                return Ok(routes);
            }
        }
    }

    /// Decodes one datagram from the kernel, keeping the routes in it.
    /// Returns true once the acknowledgement has arrived, and its error if
    /// it carries one.
    pub fn take_replies(mut buf: &[u8], routes: &mut Vec<RouteMessage>) -> io::Result<bool> {
        let invalid = |e: DecodeError| io::Error::new(io::ErrorKind::InvalidData, e);
        while !buf.is_empty() {
            let msg = NetlinkBuffer::new_checked(buf).map_err(invalid)?;
            match msg.message_type() {
                NLMSG_ERROR => {
                    let code = ErrorBuffer::new_checked(msg.payload())
                        .map_err(invalid)?
                        .code();
                    return match code {
                        None => Ok(true),
                        Some(e) => Err(io::Error::from_raw_os_error(-e.get())),
                    };
                }
                libc::RTM_NEWROUTE => {
                    routes.push(RouteMessage::parse(msg.payload()).map_err(invalid)?)
                }
                _ => {}
            }
            // Messages are padded to 4 bytes.
            let len = (msg.length() as usize).next_multiple_of(4);
            buf = &buf[len.min(buf.len())..];
        }
        Ok(false)
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
            .map(|(net, via)| record(*net, via))
            .collect()
    }
}

/// The crash-cleanup record of one route: `route NET gw G [dev D]` or
/// `route NET if D`.
fn record(net: Cidr, via: &Via) -> String {
    match via {
        Via::Gateway(g, Some(d)) => format!("route {net} gw {g} dev {d}"),
        Via::Gateway(g, None) => format!("route {net} gw {g}"),
        Via::Interface(i) => format!("route {net} if {i}"),
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
    if let Some((net, via)) = parse_record(line) {
        let _ = os::delete(net, &via);
    }
}

/// A `records()` line without its `route ` word. Lines from before
/// gateways kept their device (`NET gw G`) still parse.
fn parse_record(line: &str) -> Option<(Cidr, Via)> {
    let w: Vec<&str> = line.split_whitespace().collect();
    let (net, rest) = w.split_first()?;
    let via = match rest {
        ["gw", g] => Via::Gateway(g.parse().ok()?, None),
        ["gw", g, "dev", d] => Via::Gateway(g.parse().ok()?, Some(d.to_string())),
        ["if", d] => Via::Interface(d.to_string()),
        _ => return None,
    };
    Some((net.parse().ok()?, via))
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

    /// Crash-cleanup records parse back to the route they undo, including
    /// lines written before gateways kept their device.
    #[test]
    fn route_records() {
        let net: Cidr = "2001:db8::/32".parse().unwrap();
        let fe80 = "fe80::1".parse().unwrap();
        for (via, line) in [
            (
                Via::Gateway(fe80, Some("eth0".into())),
                "route 2001:db8::/32 gw fe80::1 dev eth0",
            ),
            (Via::Gateway(fe80, None), "route 2001:db8::/32 gw fe80::1"),
            (
                Via::Interface("utun4".into()),
                "route 2001:db8::/32 if utun4",
            ),
        ] {
            assert_eq!(record(net, &via), line);
            assert_eq!(parse_record(&line["route ".len()..]), Some((net, via)));
        }
        for bad in [
            "",
            "10.0.0.0/8",
            "10.0.0.0/8 gw",
            "10.0.0.0/8 gw x",
            "x if eth0",
        ] {
            assert_eq!(parse_record(bad), None, "{bad:?}");
        }
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
            &Via::Gateway("10.0.0.1".parse().unwrap(), None),
        );
        assert_eq!(
            a,
            ["-n", "delete", "-inet", "-net", "1.2.3.4/32", "10.0.0.1"]
        );
        // A link-local gateway keeps its scope, or route(8) fails.
        let a = os::args(
            "add",
            "2001:db8::/32".parse().unwrap(),
            &Via::Gateway("fe80::1".parse().unwrap(), Some("en0".into())),
        );
        assert_eq!(
            a,
            [
                "-n",
                "add",
                "-inet6",
                "-net",
                "2001:db8::/32",
                "fe80::1%en0"
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_macos_route_get() {
        let out = "   route to: 203.0.113.10\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>\n";
        assert_eq!(
            os::parse_route_get(out),
            Some(Via::Gateway("192.168.1.1".parse().unwrap(), None))
        );
        let v6 = "   route to: 2001:db8::10\ndestination: default\n       mask: default\n    gateway: fe80::1%en0\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>\n";
        assert_eq!(
            os::parse_route_get(v6),
            Some(Via::Gateway("fe80::1".parse().unwrap(), Some("en0".into())))
        );
        let onlink = "   route to: 192.168.1.20\ndestination: 192.168.1.0\n  interface: en0\n";
        assert_eq!(
            os::parse_route_get(onlink),
            Some(Via::Interface("en0".into()))
        );
    }

    /// Requests as `ip route add|del` sends them (same header and
    /// attributes).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_route_requests() {
        use netlink_packet_core::{NLM_F_CREATE, NLM_F_EXCL};
        let gw = |ip: &str| Via::Gateway(ip.parse().unwrap(), None);
        let add = |net: &str, via: Via| {
            let m = os::route_message(net.parse().unwrap(), &via, true).unwrap();
            os::encode(libc::RTM_NEWROUTE, NLM_F_CREATE | NLM_F_EXCL, &[&m])
        };
        let header = |len: u8, kind: u8, flags: [u8; 2]| {
            [
                len, 0, 0, 0, kind, 0, flags[0], flags[1], 1, 0, 0, 0, 0, 0, 0, 0,
            ]
        };
        let mut want = header(44, 24, [0x05, 0x06]).to_vec(); // RTM_NEWROUTE, create, excl
        want.extend([2, 8, 0, 0, 254, 3, 0, 1, 0, 0, 0, 0]); // main, boot, universe, unicast
        want.extend([8, 0, 1, 0, 10, 0, 0, 0]); // RTA_DST
        want.extend([8, 0, 5, 0, 192, 168, 1, 1]); // RTA_GATEWAY
        assert_eq!(add("10/8", gw("192.168.1.1")), want);

        // lo is device 1 in every network namespace.
        let mut want = header(44, 24, [0x05, 0x06]).to_vec();
        want.extend([2, 1, 0, 0, 254, 3, 253, 1, 0, 0, 0, 0]); // scope link
        want.extend([8, 0, 1, 0, 0, 0, 0, 0]); // RTA_DST
        want.extend([8, 0, 4, 0, 1, 0, 0, 0]); // RTA_OIF
        assert_eq!(add("0/1", Via::Interface("lo".into())), want);

        let m = os::route_message("2001:db8::/32".parse().unwrap(), &gw("fe80::1"), false);
        let mut want = header(68, 25, [0x05, 0]).to_vec(); // RTM_DELROUTE
        want.extend([10, 32, 0, 0, 254, 0, 255, 0, 0, 0, 0, 0]); // any scope
        want.extend([20, 0, 1, 0, 0x20, 0x01, 0x0d, 0xb8]); // RTA_DST
        want.extend([0; 12]);
        want.extend([20, 0, 5, 0, 0xfe, 0x80]); // RTA_GATEWAY
        want.extend([0; 13]);
        want.push(1);
        assert_eq!(os::encode(libc::RTM_DELROUTE, 0, &[&m.unwrap()]), want);

        // A gateway with its device, as `ip route add ... via fe80::1 dev lo`
        // sends it: without the device the kernel refuses a link-local one.
        let via = Via::Gateway("fe80::1".parse().unwrap(), Some("lo".into()));
        let mut want = header(76, 24, [0x05, 0x06]).to_vec();
        want.extend([10, 32, 0, 0, 254, 3, 0, 1, 0, 0, 0, 0]); // main, boot, universe, unicast
        want.extend([20, 0, 1, 0, 0x20, 0x01, 0x0d, 0xb8]); // RTA_DST
        want.extend([0; 12]);
        want.extend([20, 0, 5, 0, 0xfe, 0x80]); // RTA_GATEWAY
        want.extend([0; 13]);
        want.push(1);
        want.extend([8, 0, 4, 0, 1, 0, 0, 0]); // RTA_OIF
        assert_eq!(add("2001:db8::/32", via), want);
    }

    /// Kernel replies to RTM_GETROUTE: the route, then the acknowledgement.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_route_replies() {
        let reply = |gateway: bool| {
            let mut b = vec![0, 0, 0, 0, 24, 0, 0, 0, 1, 0, 0, 0, 0xd2, 0x04, 0, 0]; // RTM_NEWROUTE
            b.extend([2, 32, 0, 0, 254, 0, 0, 1, 0, 2, 0, 0]); // RTM_F_CLONED
            b.extend([8, 0, 15, 0, 254, 0, 0, 0]); // RTA_TABLE
            b.extend([8, 0, 1, 0, 10, 0, 0, 7]); // RTA_DST
            b.extend([8, 0, 4, 0, 2, 0, 0, 0]); // RTA_OIF
            b.extend([8, 0, 7, 0, 10, 0, 0, 5]); // RTA_PREFSRC
            if gateway {
                b.extend([8, 0, 5, 0, 10, 0, 0, 1]); // RTA_GATEWAY
            }
            b.extend([8, 0, 25, 0, 0, 0, 0, 0]); // RTA_UID
            b[0] = b.len() as u8;
            b
        };
        let ack = |code: i32| {
            let mut b = vec![36, 0, 0, 0, 2, 0, 0, 1, 1, 0, 0, 0, 0xd2, 0x04, 0, 0]; // NLMSG_ERROR
            b.extend(code.to_le_bytes());
            b.extend([36, 0, 0, 0, 26, 0, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0]); // the request's header
            b
        };
        let name = |i: u32| (i == 2).then(|| "eth0".to_string());
        let route = |datagrams: &[Vec<u8>]| {
            let mut routes = Vec::new();
            for (i, d) in datagrams.iter().enumerate() {
                let done = os::take_replies(d, &mut routes).unwrap();
                assert_eq!(done, i == datagrams.len() - 1);
            }
            let [r] = &routes[..] else {
                panic!("{routes:?}")
            };
            r.clone()
        };
        let r = route(&[[reply(true), ack(0)].concat()]);
        let gw = "10.0.0.1".parse().unwrap();
        assert_eq!(
            os::via_of(&r, name),
            Some(Via::Gateway(gw, Some("eth0".into())))
        );
        assert_eq!(os::via_of(&r, |_| None), Some(Via::Gateway(gw, None)));
        assert_eq!(
            os::via_of(&route(&[reply(false), ack(0)]), name),
            Some(Via::Interface("eth0".into()))
        );
        let e = os::take_replies(&ack(-libc::EEXIST), &mut Vec::new()).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EEXIST));
    }

    /// A real lookup (no root needed).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_route_lookup() {
        assert_eq!(
            os::current_route("127.0.0.1".parse().unwrap()).unwrap(),
            Via::Interface("lo".into())
        );
    }
}
