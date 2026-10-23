//! Facts about the local system the firewall setup needs: DNS servers, and
//! the addresses the ssh connection itself uses (which must stay out of the
//! tunnel, or it would try to carry itself).

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::process::Command;

use tracing::debug;

/// The system's DNS servers. On macOS `scutil --dns` lists every resolver
/// (resolv.conf only has the primary one). With systemd-resolved,
/// resolv.conf names only its local stub, and the servers it forwards to are
/// in /run/systemd/resolve/resolv.conf.
///
/// Loopback servers (a local stub resolver) are kept only if
/// `keep_loopback`: nft redirects them like any other, but redirecting lo0
/// traffic to lo0 is not something pf can do, and tun mode cannot route
/// them.
pub fn nameservers(keep_loopback: bool) -> Vec<IpAddr> {
    let mut found = BTreeSet::new();
    if cfg!(target_os = "macos")
        && let Ok(out) = Command::new("/usr/sbin/scutil").arg("--dns").output()
    {
        found.extend(parse_scutil(&String::from_utf8_lossy(&out.stdout)));
    }
    for path in ["/etc/resolv.conf", "/run/systemd/resolve/resolv.conf"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            found.extend(parse_resolv_conf(&text));
        }
    }
    found
        .into_iter()
        .filter(|ip: &IpAddr| keep_loopback || !ip.is_loopback())
        .collect()
}

fn parse_scutil(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter(|l| l.trim_start().starts_with("nameserver["))
        .filter_map(|l| l.split_once(':').map(|(_, ip)| ip.trim()))
        .filter_map(|ip| ip.split('%').next()?.parse().ok())
        .collect()
}

fn parse_resolv_conf(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|r| r.split_whitespace().next())
        .filter_map(|ip| ip.split('%').next()?.parse().ok())
        .collect()
}

/// Addresses of the ssh destination and its first ProxyJump hop, from
/// `ssh -G` (which applies ~/.ssh/config), resolved to every A and AAAA.
pub fn ssh_destination_ips(ssh_cmd: &str, dest: &str) -> Vec<IpAddr> {
    let Some(mut parts) = shlex::split(ssh_cmd) else {
        return vec![];
    };
    if parts.is_empty() {
        return vec![];
    }
    let prog = parts.remove(0);
    let Ok(out) = Command::new(prog).args(parts).arg("-G").arg(dest).output() else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut hosts = Vec::new();
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("hostname ") {
            hosts.push(h.trim().to_string());
        } else if let Some(j) = line.strip_prefix("proxyjump ") {
            // First hop of "user@host:port,next": the one we connect to.
            let first = j.split(',').next().unwrap_or("");
            let host = first.rsplit('@').next().unwrap_or("");
            let host = host
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or(host);
            let host = if host.matches(':').count() == 1 {
                host.split(':').next().unwrap()
            } else {
                host
            };
            if !host.is_empty() && host != "none" {
                hosts.push(host.to_string());
            }
        }
    }
    let mut ips = BTreeSet::new();
    for h in hosts {
        if let Ok(addrs) = (h.as_str(), 22).to_socket_addrs() {
            ips.extend(addrs.map(|sa| sa.ip()));
        }
    }
    debug!(?ips, "ssh destination addresses");
    ips.into_iter().collect()
}

/// Remote addresses of the TCP connections held by `pid` and its
/// descendants: the ssh connection itself, and the `ssh -W` a ProxyJump
/// starts. Catches what name resolution can miss (ProxyCommand, round-robin
/// DNS).
pub fn process_tree_peers(pid: u32) -> Vec<IpAddr> {
    let peers = tree_peers(pid);
    debug!(?peers, "ssh connection peers");
    peers
}

/// Linux: straight from /proc, since lsof is often not installed (nor
/// pgrep, on minimal systems). A process's sockets are the `socket:[inode]`
/// links in /proc/<pid>/fd; /proc/<pid>/net/tcp{,6} maps inodes to
/// connections.
#[cfg(target_os = "linux")]
fn tree_peers(pid: u32) -> Vec<IpAddr> {
    let mut parents = Vec::new();
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(p) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{p}/stat"))
            && let Some(pp) = parse_stat_ppid(&stat)
        {
            parents.push((p, pp));
        }
    }
    let mut inodes = std::collections::HashSet::new();
    for p in descendants(pid, &parents) {
        for e in std::fs::read_dir(format!("/proc/{p}/fd"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Ok(link) = std::fs::read_link(e.path())
                && let Some(i) = socket_inode(&link.to_string_lossy())
            {
                inodes.insert(i);
            }
        }
    }
    let mut out = BTreeSet::new();
    for table in ["tcp", "tcp6"] {
        if let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/net/{table}")) {
            out.extend(
                parse_proc_net_tcp(&text)
                    .into_iter()
                    .filter(|(inode, _)| inodes.contains(inode))
                    .map(|(_, ip)| ip),
            );
        }
    }
    out.into_iter().collect()
}

/// macOS: children from `pgrep -P`, connections from `lsof`.
#[cfg(not(target_os = "linux"))]
fn tree_peers(pid: u32) -> Vec<IpAddr> {
    let mut pids = vec![pid];
    let mut i = 0;
    while i < pids.len() {
        if let Ok(out) = Command::new("pgrep")
            .args(["-P", &pids[i].to_string()])
            .output()
        {
            for l in String::from_utf8_lossy(&out.stdout).lines() {
                if let Ok(p) = l.trim().parse() {
                    pids.push(p);
                }
            }
        }
        i += 1;
    }
    let list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let Ok(out) = Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &list,
            "-iTCP",
            "-sTCP:ESTABLISHED",
            "-Fn",
        ])
        .output()
    else {
        return vec![];
    };
    parse_lsof(&String::from_utf8_lossy(&out.stdout))
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_lsof(text: &str) -> Vec<IpAddr> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let Some(name) = line.strip_prefix('n') else {
            continue;
        };
        let Some((_, remote)) = name.split_once("->") else {
            continue;
        };
        let remote = remote.replace(['[', ']'], "");
        if let Some((ip, _port)) = remote.rsplit_once(':')
            && let Ok(ip) = ip.parse::<IpAddr>()
        {
            out.insert(ip);
        }
    }
    out.into_iter().collect()
}

/// `pid` and all its descendants, given every process's (pid, parent pid).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn descendants(pid: u32, parents: &[(u32, u32)]) -> Vec<u32> {
    let mut out = vec![pid];
    let mut i = 0;
    while i < out.len() {
        let p = out[i];
        out.extend(
            parents
                .iter()
                .filter(|(c, pp)| *pp == p && !out.contains(c))
                .map(|(c, _)| *c)
                .collect::<Vec<_>>(),
        );
        i += 1;
    }
    out
}

/// The parent pid from /proc/<pid>/stat: `pid (comm) state ppid ...`. The
/// command name may itself contain spaces and parentheses, so the fields
/// are counted from the last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat_ppid(stat: &str) -> Option<u32> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// Established connections in /proc/net/tcp (or tcp6): (socket inode,
/// remote address).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_net_tcp(text: &str) -> Vec<(u64, IpAddr)> {
    const ESTABLISHED: &str = "01";
    text.lines()
        .skip(1)
        .filter_map(|l| {
            // sl local_address rem_address st tx:rx tr:when retrnsmt uid timeout inode
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 || f[3] != ESTABLISHED {
                return None;
            }
            let ip = parse_proc_addr(f[2].split_once(':')?.0)?;
            Some((f[9].parse().ok()?, ip))
        })
        .collect()
}

/// An address as /proc/net/tcp prints it: each 32-bit word in native byte
/// order, as hex. IPv4-mapped IPv6 addresses come back as IPv4.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_addr(hex: &str) -> Option<IpAddr> {
    if !hex.is_ascii() || !hex.len().is_multiple_of(8) {
        return None;
    }
    let mut bytes = Vec::with_capacity(16);
    for i in (0..hex.len()).step_by(8) {
        let word = u32::from_str_radix(&hex[i..i + 8], 16).ok()?;
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    match bytes.len() {
        4 => Some(IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?))),
        16 => {
            let v6 = Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?);
            Some(v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4))
        }
        _ => None,
    }
}

/// True if `dst` is one of our own listening addresses.
pub fn is_own_listener(dst: SocketAddr, ports: &[u16]) -> bool {
    dst.ip().is_loopback() && ports.contains(&dst.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scutil_output() {
        let text = "DNS configuration\n\nresolver #1\n  search domain[0] : lan\n  nameserver[0] : 192.0.2.53\n  nameserver[1] : 192.0.2.52\n  if_index : 15 (en0)\n\nresolver #2\n  nameserver[0] : fe80::1%en0\n";
        let v = parse_scutil(text);
        assert_eq!(
            v,
            vec![
                "192.0.2.53".parse::<IpAddr>().unwrap(),
                "192.0.2.52".parse().unwrap(),
                "fe80::1".parse().unwrap()
            ]
        );
    }

    /// /proc/net/tcp and tcp6 captured on aarch64 Linux, plus a crafted
    /// line for an ssh connection to 203.0.113.10:22. Only established
    /// connections count (01; 0A is listening, 05 closing).
    #[cfg(target_endian = "little")]
    #[test]
    fn proc_net_tcp() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:08AE 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2772706 1 00000000b8b8bdfb 100 0 0 10 0
   1: 0100007F:A62E 0100007F:08AE 01 00000000:00000000 00:00000000 00000000     0        0 2772707 2 00000000b9a69c74 20 0 0 10 -1
   2: 030011AC:D4E8 0A7100CB:0016 01 00000000:00000000 00:00000000 00000000  1000        0 2790001 1 0000000000000000 20 4 30 10 -1
   3: 030011AC:D4EA 84026597:01BB 05 00000000:00000000 03:0000176E 00000000     0        0 0 3 00000000694b4f62
";
        assert_eq!(
            parse_proc_net_tcp(tcp),
            vec![
                (2772707, "127.0.0.1".parse::<IpAddr>().unwrap()),
                (2790001, "203.0.113.10".parse().unwrap()),
            ]
        );
        let tcp6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:08AF 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2772709 1 00000000382f875b 100 0 0 10 0
   1: 00000000000000000000000001000000:AA86 00000000000000000000000001000000:08AF 01 00000000:00000000 00:00000000 00000000     0        0 2772710 2 0000000008be5a22 20 0 0 10 -1
   2: 0000000000000000FFFF00000100007F:A632 0000000000000000FFFF00000100007F:08AE 01 00000000:00000000 00:00000000 00000000     0        0 2772712 2 00000000eda78054 20 0 0 10 -1
   3: B80D0120000000000000000009000000:D4E8 B80D0120000000000000000005000000:0016 01 00000000:00000000 00:00000000 00000000  1000        0 2790002 1 0000000000000000 20 4 30 10 -1
";
        assert_eq!(
            parse_proc_net_tcp(tcp6),
            vec![
                (2772710, "::1".parse::<IpAddr>().unwrap()),
                (2772712, "127.0.0.1".parse().unwrap()),
                (2790002, "2001:db8::5".parse().unwrap()),
            ]
        );
    }

    #[test]
    fn proc_fields() {
        assert_eq!(socket_inode("socket:[2772707]"), Some(2772707));
        assert_eq!(socket_inode("pipe:[2773339]"), None);
        assert_eq!(socket_inode("/dev/null"), None);
        let stat = "10 (python3) R 1 1 1 0 -1 4194560 1765 0 0 0 0 0 0 0 20 0 1 0";
        assert_eq!(parse_stat_ppid(stat), Some(1));
        let odd = "4242 (ssh (x) y) S 77 4242 1 0 -1";
        assert_eq!(parse_stat_ppid(odd), Some(77));
    }

    #[test]
    fn process_tree() {
        // 10 is ssh; 11 its ProxyJump `ssh -W`, 12 a grandchild; 20 unrelated.
        let parents = [(1, 0), (10, 5), (11, 10), (12, 11), (20, 1), (21, 20)];
        assert_eq!(descendants(10, &parents), vec![10, 11, 12]);
        assert_eq!(descendants(99, &parents), vec![99]);
    }

    #[test]
    fn lsof_output() {
        let text = "p123\nf3\nn192.168.1.5:50122->203.0.113.10:22\np124\nf4\nn[2001:db8::5]:5000->[2001:db8::9]:22\nf5\nn*:8080\n";
        assert_eq!(
            parse_lsof(text),
            vec![
                "203.0.113.10".parse::<IpAddr>().unwrap(),
                "2001:db8::9".parse().unwrap()
            ]
        );
    }
}
