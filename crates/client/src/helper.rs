//! The privileged helper: a second copy of this binary, run as root through
//! sudo, that owns the firewall rules. It removes them when the client's
//! connection closes, however the client ends.
//!
//! The two talk over a Unix socket in a private directory the client makes,
//! not over sudo's stdio: sudo may relay stdio through pipes (I/O logging,
//! `use_pty`) and closes extra file descriptors, either of which would break
//! passing descriptors later (the tun device). The client accepts only a peer
//! running as root.
//!
//! Line protocol. Helper first sends `READY <version>`. Requests carry an id
//! and get `OK <id> [result]` or `ERR <id> <message>`, in order:
//! - `SETUP <id>`, then `ROUTE +|- <cidr>`, `NS <ip>`, `PORTS <tcp> <dns>`,
//!   `OPT fix-loopback`, `OPT tun <mtu>`, `OPT tun-dns`, and `END`. The
//!   helper loads pf rules (macOS) or an nftables table (Linux), or in tun
//!   mode creates the device; then the reply carries the interface name, and
//!   the helper connects a second time to hand over the device's descriptor
//!   (SCM_RIGHTS).
//! - `NATLOOK <id> <peer> <local>`: the original destination of a
//!   redirected connection (macOS; on Linux the client asks the kernel).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::Command;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::subnet::{Cidr, Route};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything the helper needs to set up redirection.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    pub routes: Vec<Route>,
    pub nameservers: Vec<IpAddr>,
    pub tcp_port: u16,
    pub dns_port: u16,
    pub fix_loopback: bool,
    /// Tun mode with this MTU, instead of firewall redirection.
    pub tun_mtu: Option<u16>,
    /// Tun mode: make the system resolver use the tunnel.
    pub tun_dns: bool,
}

impl Setup {
    fn encode(&self, id: u64) -> String {
        let mut s = format!("SETUP {id}\n");
        for r in &self.routes {
            s += &format!("ROUTE {} {}\n", if r.exclude { '-' } else { '+' }, r.net);
        }
        for ns in &self.nameservers {
            s += &format!("NS {ns}\n");
        }
        s += &format!("PORTS {} {}\n", self.tcp_port, self.dns_port);
        if self.fix_loopback {
            s += "OPT fix-loopback\n";
        }
        if let Some(mtu) = self.tun_mtu {
            s += &format!("OPT tun {mtu}\n");
        }
        if self.tun_dns {
            s += "OPT tun-dns\n";
        }
        s += "END\n";
        s
    }

    fn apply_line(&mut self, line: &str) -> Result<()> {
        let mut w = line.split_whitespace();
        match (w.next(), w.next(), w.next()) {
            (Some("ROUTE"), Some(sign), Some(net)) => self.routes.push(Route {
                net: net.parse::<Cidr>()?,
                exclude: sign == "-",
            }),
            (Some("NS"), Some(ip), None) => self.nameservers.push(ip.parse()?),
            (Some("PORTS"), Some(t), Some(d)) => {
                self.tcp_port = t.parse()?;
                self.dns_port = d.parse()?;
            }
            (Some("OPT"), Some("fix-loopback"), None) => self.fix_loopback = true,
            (Some("OPT"), Some("tun"), Some(mtu)) => self.tun_mtu = Some(mtu.parse()?),
            (Some("OPT"), Some("tun-dns"), None) => self.tun_dns = true,
            _ => bail!("bad setup line {line:?}"),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- client side

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<String, String>>>>>;

pub struct Helper {
    listener: UnixListener,
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    pending: Pending,
    next_id: std::sync::atomic::AtomicU64,
    dir: PathBuf,
    gone: tokio::sync::watch::Receiver<bool>,
}

/// A directory only we can enter, for the rendezvous socket.
fn private_dir() -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let mut rnd = [0u8; 8];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut rnd)?;
    let name = format!(
        "sshuttle2-{}-{}",
        std::process::id(),
        rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    // Unix socket paths are limited to 104 bytes on macOS; a long $TMPDIR
    // would not fit, so fall back to /tmp.
    let mut base = std::env::temp_dir();
    if base.join(&name).join("helper.sock").as_os_str().len() > 100 {
        base = PathBuf::from("/tmp");
    }
    let dir = base.join(name);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}

/// Finds a program in PATH or the usual places, including the sbin
/// directories a user's PATH often lacks (Debian's nft is /usr/sbin/nft).
pub fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain([
            "/usr/bin".into(),
            "/usr/local/bin".into(),
            "/opt/homebrew/bin".into(),
            "/usr/sbin".into(),
            "/sbin".into(),
        ])
        .map(|d| d.join(prog))
        .find(|p| p.is_file())
}

impl Helper {
    /// Starts the helper, through sudo (or doas) unless already root. Do this
    /// before ssh, so the password prompt comes first.
    pub async fn start(verbose: u8) -> Result<Helper> {
        let dir = private_dir()?;
        let sock = dir.join("helper.sock");
        let listener = UnixListener::bind(&sock)?;
        let exe = std::env::current_exe()?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        let mut cmd = if root {
            Command::new(&exe)
        } else if let Some(sudo) = which("sudo") {
            let mut c = Command::new(sudo);
            c.args(["-p", "[local sudo] Password: "]).arg(&exe);
            c
        } else if let Some(doas) = which("doas") {
            let mut c = Command::new(doas);
            c.arg(&exe);
            c
        } else {
            bail!("need root for the firewall, but found neither sudo nor doas");
        };
        cmd.arg("helper").arg("--socket").arg(&sock);
        cmd.arg("--client-pid").arg(std::process::id().to_string());
        if verbose > 0 {
            cmd.arg(format!("-{}", "v".repeat(verbose as usize)));
        }
        // Not kill_on_drop: SIGKILL would leave the firewall rules behind.
        // The helper exits by itself once our connection closes.
        let mut child = cmd.spawn().context("starting the firewall helper")?;

        let stream = tokio::select! {
            biased;
            r = listener.accept() => r?.0,
            st = child.wait() => {
                let _ = std::fs::remove_dir_all(&dir);
                bail!("firewall helper did not start ({})", st.map(|s| s.to_string()).unwrap_or_default());
            }
            _ = tokio::time::sleep(Duration::from_secs(300)) => {
                let _ = std::fs::remove_dir_all(&dir);
                bail!("timed out waiting for the firewall helper (sudo password?)");
            }
        };
        let cred = stream.peer_cred()?;
        if cred.uid() != 0 {
            bail!("firewall helper connected as uid {}, not root", cred.uid());
        }
        // The helper detaches once connected, so sudo exits now; reap it.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        let (rd, wr) = stream.into_split();
        let mut lines = BufReader::new(rd).lines();
        let ready = lines
            .next_line()
            .await?
            .context("helper closed before READY")?;
        match ready.split_once(' ') {
            Some(("READY", v)) if v == VERSION => {}
            _ => bail!("unexpected greeting from helper: {ready:?}"),
        }
        let pending: Pending = Arc::default();
        let p = pending.clone();
        let (gone_tx, gone) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(msg) = line.strip_prefix("LOG ") {
                    info!("helper: {msg}");
                    continue;
                }
                let mut parts = line.splitn(3, ' ');
                let (Some(kind), Some(id)) = (parts.next(), parts.next()) else {
                    continue;
                };
                let Ok(id) = id.parse::<u64>() else { continue };
                let rest = parts.next().unwrap_or("").to_string();
                let res = if kind == "OK" { Ok(rest) } else { Err(rest) };
                if let Some(tx) = p.lock().unwrap().remove(&id) {
                    let _ = tx.send(res);
                }
            }
            // Helper gone: fail everything still waiting.
            p.lock().unwrap().clear();
            let _ = gone_tx.send(true);
        });
        Ok(Helper {
            listener,
            writer: tokio::sync::Mutex::new(wr),
            pending,
            next_id: 1.into(),
            dir,
            gone,
        })
    }

    async fn request(&self, build: impl FnOnce(u64) -> String) -> Result<String> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = build(id);
        self.writer
            .lock()
            .await
            .write_all(msg.as_bytes())
            .await
            .context("firewall helper is gone")?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow!("{e}")),
            Err(_) => Err(anyhow!("firewall helper is gone")),
        }
    }

    pub async fn setup(&self, s: &Setup) -> Result<()> {
        self.request(|id| s.encode(id)).await.map(|_| ())
    }

    /// Sets up tun mode. Returns the interface name, its MTU and the device.
    pub async fn setup_tun(&self, s: &Setup) -> Result<(String, u16, std::os::fd::OwnedFd)> {
        let reply = self.request(|id| s.encode(id)).await?;
        let (ifname, mtu) = reply.split_once(' ').context("bad tun reply")?;
        let (ifname, mtu) = (ifname.to_string(), mtu.parse::<u16>()?);
        let stream = tokio::time::timeout(Duration::from_secs(10), self.listener.accept())
            .await
            .context("helper did not hand over the tun device")??
            .0;
        let (uid, fd) = receive_handoff(stream).await?;
        if uid != 0 {
            bail!("tun device offered by a non-root process");
        }
        Ok((ifname, mtu, fd))
    }

    // Linux reads SO_ORIGINAL_DST instead (transparent.rs).
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub async fn natlook(&self, peer: SocketAddr, local: SocketAddr) -> Result<SocketAddr> {
        let r = self
            .request(|id| format!("NATLOOK {id} {peer} {local}\n"))
            .await?;
        Ok(r.parse()?)
    }

    /// Closes the connection, which makes the helper remove its rules, and
    /// waits for it to finish.
    pub async fn shutdown(&self) {
        let _ = self.writer.lock().await.shutdown().await;
        match tokio::time::timeout(Duration::from_secs(15), self.gone()).await {
            Ok(_) => debug!("firewall helper finished"),
            Err(_) => warn!("firewall helper still running after 15 s"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// Resolves when the helper's connection closes (it died, or was
    /// killed): the firewall rules are then gone.
    pub async fn gone(&self) {
        let mut rx = self.gone.clone();
        let _ = rx.wait_for(|g| *g).await;
    }
}

/// Client end of the tun handoff: the sender's uid and the descriptor.
/// The credentials are read first, while the sender is still connected.
async fn receive_handoff(stream: tokio::net::UnixStream) -> Result<(u32, std::os::fd::OwnedFd)> {
    let uid = stream.peer_cred()?.uid();
    let stream = stream.into_std()?;
    stream.set_nonblocking(false)?;
    let fd = tokio::task::spawn_blocking(move || -> Result<_> {
        let fd = crate::tundev::recv_fd(&stream)?;
        std::io::Write::write_all(&mut &stream, b"A")?;
        Ok(fd)
    })
    .await??;
    Ok((uid, fd))
}

/// Helper end: connects to the client's socket, sends the descriptor, and
/// stays connected until the client acknowledges. macOS cannot report a
/// peer's credentials once the peer has closed, and the client checks that
/// the descriptor comes from root.
fn send_handoff(socket: &Path, dev: &std::os::fd::OwnedFd) -> std::io::Result<()> {
    let mut c = std::os::unix::net::UnixStream::connect(socket)?;
    crate::tundev::send_fd(&c, std::os::fd::AsRawFd::as_raw_fd(dev))
        .map_err(std::io::Error::other)?;
    c.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut ack = [0u8; 1];
    std::io::Read::read_exact(&mut c, &mut ack)
}

// ------------------------------------------------------------------ root side

static CLIENT_PID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn forward_signal(_: libc::c_int) {
    // Ctrl+C can land on the helper (sudo with use_pty, or a manual kill).
    // Pass it to the client, which shuts down and closes our connection;
    // cleanup happens then.
    let pid = CLIENT_PID.load(std::sync::atomic::Ordering::Relaxed);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe and takes no pointers.
        unsafe { libc::kill(pid, libc::SIGINT) };
    }
}

/// Firewall state owned by the helper. Dropping it removes the rules.
struct Firewall {
    kind: FwKind,
    state_file: PathBuf,
}

enum FwKind {
    #[cfg(target_os = "macos")]
    Pf(crate::pf::Pf),
    #[cfg(target_os = "linux")]
    Nft(crate::nft::Nft),
    /// Held only to be dropped: that removes the DNS service and routes.
    #[allow(dead_code)]
    Tun(TunState),
}

/// Held only to be dropped. Fields drop in order: the DNS service goes
/// before the routes.
#[allow(dead_code)]
struct TunState {
    dns: Option<crate::routes::SystemDns>,
    routes: crate::routes::TunRoutes,
}

impl Firewall {
    /// Tun mode: creates the device, addresses and routes it. Returns the
    /// device for the client.
    fn setup_tun(s: &Setup, mtu: u16) -> Result<(Firewall, std::os::fd::OwnedFd, String)> {
        #[cfg(target_os = "macos")]
        for anchor in crate::pf::anchors_capturing_tcp() {
            warn!(
                "pf anchor {anchor} sends TCP to lo0 before routing, so tun mode never sees it \
                 (sshuttle still running, or its rules left behind). Stop sshuttle, or clear \
                 the rules: sudo pfctl -a {anchor} -F all"
            );
        }
        let mut routes = s.routes.clone();
        crate::subnet::sort_routes(&mut routes);
        let (fd, ifname) = crate::tundev::create()?;
        let state_file = state_dir().join(format!("{ifname}.state"));
        let mtu = crate::routes::configure(&ifname, mtu)?;
        // DNS servers go into the tunnel even when their network is
        // excluded (a LAN router, say): the client answers port 53 itself.
        let mut into_tun = s.nameservers.clone();
        if s.tun_dns && cfg!(target_os = "linux") {
            // The server systemd-resolved is pointed at below. (On macOS
            // the point-to-point address routes it.)
            into_tun.push(crate::routes::TUN_PEER_V4.parse()?);
        }
        let routes_obj = crate::routes::TunRoutes::install(&ifname, &routes, &into_tun)?;
        let dns = if !s.tun_dns {
            None
        } else if cfg!(target_os = "macos") {
            let d = crate::routes::SystemDns::publish(&ifname)?;
            info!("system DNS now goes through the tunnel");
            Some(d)
        } else {
            // Without systemd-resolved the tunnel still works; only the
            // system's DNS settings stay as they are.
            match crate::routes::SystemDns::publish(&ifname) {
                Ok(d) => {
                    info!("system DNS now goes through the tunnel (resolvectl)");
                    Some(d)
                }
                Err(e) => {
                    warn!(
                        "system DNS is NOT redirected ({e:#}): lookups go to the usual \
                         nameservers, through the tunnel only if their addresses are routed \
                         into it (--dns does that). Or set nameserver {} in /etc/resolv.conf",
                        crate::routes::TUN_PEER_V4
                    );
                    None
                }
            }
        };
        let mut records = routes_obj.records();
        if let Some(d) = &dns {
            records.push(d.record());
        }
        write_state(&state_file, &format!("tun {ifname}"), &[], &records);
        info!("{ifname} up, MTU {mtu}");
        Ok((
            Firewall {
                kind: FwKind::Tun(TunState {
                    dns,
                    routes: routes_obj,
                }),
                state_file,
            },
            fd,
            format!("{ifname} {mtu}"),
        ))
    }

    fn setup(s: &Setup) -> Result<Firewall> {
        let anchor = format!("sshuttle2-{}", s.tcp_port);
        let state_file = state_dir().join(format!("{anchor}.state"));
        #[cfg(target_os = "macos")]
        {
            let mut routes = s.routes.clone();
            crate::subnet::sort_routes(&mut routes);
            let rules = crate::pf::rules(&routes, s.tcp_port, s.dns_port, &s.nameservers);
            let mut pf = crate::pf::Pf::open(anchor.clone())?;
            // Record the session before touching pf, so `sshuttle2
            // cleanup` can undo a crashed one.
            write_state(&state_file, &format!("anchor {anchor}"), &[], &[]);
            if let Err(e) = pf.setup(&rules, s.fix_loopback) {
                pf.teardown();
                let _ = std::fs::remove_file(&state_file);
                return Err(e);
            }
            write_state(&state_file, &format!("anchor {anchor}"), pf.tokens(), &[]);
            Ok(Firewall {
                kind: FwKind::Pf(pf),
                state_file,
            })
        }
        #[cfg(target_os = "linux")]
        {
            let mut routes = s.routes.clone();
            crate::subnet::sort_routes(&mut routes);
            let rules = crate::nft::rules(&anchor, &routes, s.tcp_port, s.dns_port, &s.nameservers);
            // Recorded first, as for pf. Loading is one transaction: if it
            // fails, there is nothing to undo.
            write_state(&state_file, &format!("nft {anchor}"), &[], &[]);
            match crate::nft::Nft::load(anchor, &rules) {
                Ok(nft) => Ok(Firewall {
                    kind: FwKind::Nft(nft),
                    state_file,
                }),
                Err(e) => {
                    let _ = std::fs::remove_file(&state_file);
                    Err(e)
                }
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = state_file;
            bail!("capturing traffic is not supported on this OS yet; use --socks5")
        }
    }

    fn natlook(&self, peer: SocketAddr, local: SocketAddr) -> Result<SocketAddr> {
        match &self.kind {
            #[cfg(target_os = "macos")]
            FwKind::Pf(pf) => pf.natlook(peer, local),
            #[cfg(target_os = "linux")]
            FwKind::Nft(_) => bail!("no NAT lookups with nft: the client reads SO_ORIGINAL_DST"),
            FwKind::Tun(_) => {
                let _ = (peer, local);
                bail!("no NAT lookups in tun mode")
            }
        }
    }
}

impl Drop for Firewall {
    fn drop(&mut self) {
        info!("removing firewall rules");
        match &mut self.kind {
            #[cfg(target_os = "macos")]
            FwKind::Pf(pf) => pf.teardown(),
            #[cfg(target_os = "linux")]
            FwKind::Nft(nft) => nft.teardown(),
            // Routes come down when the TunRoutes is dropped below.
            FwKind::Tun(_) => {}
        }
        self.kind = FwKind::Tun(TunState {
            dns: None,
            routes: crate::routes::TunRoutes::empty(),
        });
        let _ = std::fs::remove_file(&self.state_file);
    }
}

fn state_dir() -> PathBuf {
    PathBuf::from("/var/run/sshuttle2")
}

/// Records a session so `sshuttle2 cleanup` can undo it after a crash.
fn write_state(path: &Path, what: &str, tokens: &[String], routes: &[String]) {
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let mut body = format!("{what}\n");
    for t in tokens {
        body += &format!("token {t}\n");
    }
    for r in routes {
        body += &format!("{r}\n");
    }
    if let Err(e) = std::fs::write(path, body) {
        warn!("cannot record session in {}: {e}", path.display());
    }
}

/// Leaves sudo behind: forks, and the parent exits as soon as the child has
/// its own session. sudo then returns, which matters because with `use_pty`
/// (the default since sudo 1.9.14) it keeps the user's terminal in raw mode
/// for as long as the command runs: log lines stair-step and Ctrl+C stops
/// working. The child keeps only the socket; its log goes over it.
fn detach() -> Result<()> {
    let mut fds = [0i32; 2];
    // SAFETY: the helper is single-threaded here, so the child may run any
    // code after fork; every pointer passed is to a live local.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error()).context("pipe");
        }
        match libc::fork() {
            -1 => Err(std::io::Error::last_os_error()).context("fork"),
            0 => {
                libc::close(fds[0]);
                libc::setsid();
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if null >= 0 {
                    for fd in 0..3 {
                        libc::dup2(null, fd);
                    }
                    if null > 2 {
                        libc::close(null);
                    }
                }
                libc::write(fds[1], [1u8].as_ptr().cast(), 1);
                libc::close(fds[1]);
                Ok(())
            }
            _ => {
                libc::close(fds[1]);
                let mut b = [0u8; 1];
                libc::read(fds[0], b.as_mut_ptr().cast(), 1);
                libc::_exit(0);
            }
        }
    }
}

type Sink = Arc<Mutex<std::os::unix::net::UnixStream>>;
static LOG_SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// The helper's tracing output: `LOG <line>` messages to the client, which
/// prints them. (Its stderr is /dev/null once detached.)
pub struct LogWriter;

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let sink = LOG_SINK.lock().unwrap().clone();
        match sink {
            Some(s) => {
                let text = String::from_utf8_lossy(buf);
                let mut out = String::new();
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    out += &format!("LOG {}\n", line.trim_end());
                }
                let _ = s.lock().unwrap().write_all(out.as_bytes());
            }
            None => {
                let _ = std::io::stderr().write_all(buf);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `sshuttle2 helper`: runs as root until the client disconnects.
pub fn run(socket: &Path, client_pid: i32) -> Result<()> {
    use std::io::{BufRead, Write};

    CLIENT_PID.store(client_pid, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: forward_signal only does async-signal-safe work.
    unsafe {
        // Keep running (and cleaning up) if the terminal goes away.
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(
            libc::SIGINT,
            forward_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            forward_signal as *const () as libc::sighandler_t,
        );
    }
    // Root runs system tools only (nft, resolvectl): a PATH kept from the
    // user's environment could put another program of the same name first.
    // SAFETY: single-threaded at this point.
    unsafe { std::env::set_var("PATH", "/usr/sbin:/usr/bin:/sbin:/bin") };

    let stream = std::os::unix::net::UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    detach()?;
    let out = Arc::new(Mutex::new(stream.try_clone()?));
    *LOG_SINK.lock().unwrap() = Some(out.clone());
    let send = |msg: &str| out.lock().unwrap().write_all(msg.as_bytes());
    let mut lines = std::io::BufReader::new(stream).lines();
    send(&format!("READY {VERSION}\n"))?;

    let mut fw: Option<Firewall> = None;
    let mut tun_dev: Option<std::os::fd::OwnedFd> = None;
    while let Some(line) = lines.next() {
        let line = line?;
        let mut w = line.split_whitespace();
        let (cmd, id) = (w.next().unwrap_or(""), w.next().unwrap_or("0"));
        let reply = match cmd {
            "SETUP" => {
                let mut s = Setup::default();
                let mut res = Ok(());
                for l in lines.by_ref() {
                    let l = l?;
                    if l == "END" {
                        break;
                    }
                    if res.is_ok() {
                        res = s.apply_line(&l);
                    }
                }
                res.and_then(|()| {
                    if fw.is_some() {
                        bail!("already set up");
                    }
                    match s.tun_mtu {
                        None => {
                            fw = Some(Firewall::setup(&s)?);
                            Ok(String::new())
                        }
                        Some(mtu) => {
                            let (f, dev, ifname) = Firewall::setup_tun(&s, mtu)?;
                            fw = Some(f);
                            tun_dev = Some(dev);
                            Ok(ifname)
                        }
                    }
                })
            }
            "NATLOOK" => (|| {
                let peer: SocketAddr = w.next().context("missing peer")?.parse()?;
                let local: SocketAddr = w.next().context("missing local")?.parse()?;
                let fw = fw.as_ref().context("not set up")?;
                Ok(fw.natlook(peer, local)?.to_string())
            })(),
            other => Err(anyhow!("unknown command {other:?}")),
        };
        let msg = match reply {
            Ok(v) if v.is_empty() => format!("OK {id}\n"),
            Ok(v) => format!("OK {id} {v}\n"),
            Err(e) => format!("ERR {id} {}\n", format!("{e:#}").replace('\n', " ")),
        };
        if send(&msg).is_err() {
            break;
        }
        // Hand the tun device over on a second connection, then let go of
        // our copy: the interface lives exactly as long as the client.
        if let Some(dev) = tun_dev.take() {
            let handoff = send_handoff(socket, &dev);
            if let Err(e) = handoff {
                warn!("handing over the tun device: {e}");
                break;
            }
        }
    }
    // Client gone: `fw` drops here and removes the rules.
    drop(fw);
    Ok(())
}

/// `sshuttle2 cleanup`: removes rules left by sessions that crashed.
pub fn cleanup() -> Result<()> {
    let dir = state_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("nothing to clean up");
        return Ok(());
    };
    let mut n = 0;
    for e in entries.flatten() {
        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
        let mut anchor = None;
        let mut tokens = Vec::new();
        for l in text.lines() {
            match l.split_once(' ') {
                Some(("anchor", a)) => anchor = Some(a.to_string()),
                Some(("token", t)) => tokens.push(t.to_string()),
                Some(("route", r)) => crate::routes::delete_recorded(r),
                Some(("scutil", key)) => crate::routes::remove_system_dns(key),
                Some(("resolvectl", ifname)) => crate::routes::revert_resolvectl(ifname),
                Some(("nft", table)) => {
                    // Fails harmlessly if the table is already gone.
                    let _ = crate::nft::delete_table(table);
                    println!("removed {table}");
                    n += 1;
                }
                Some(("tun", ifname)) => {
                    println!("removed routes of {ifname}");
                    n += 1;
                }
                _ => {}
            }
        }
        if let Some(a) = anchor {
            #[cfg(target_os = "macos")]
            crate::pf::cleanup_stale(&a, &tokens);
            println!("removed {a}");
            n += 1;
        }
        let _ = std::fs::remove_file(e.path());
    }
    if n == 0 {
        println!("nothing to clean up");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live macOS run failed here: the helper closed before the client
    /// read its credentials.
    #[tokio::test]
    async fn tun_handoff() {
        let dir = std::env::temp_dir().join(format!("tsho-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("h.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (r, w) = std::os::unix::net::UnixStream::pair().unwrap();
        let dev = std::os::fd::OwnedFd::from(w);
        let path = sock.clone();
        let sender = std::thread::spawn(move || send_handoff(&path, &dev));
        let (stream, _) = listener.accept().await.unwrap();
        let (uid, fd) = receive_handoff(stream).await.unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        assert_eq!(uid, unsafe { libc::geteuid() });
        sender.join().unwrap().unwrap();
        let mut f = std::fs::File::from(fd);
        std::io::Write::write_all(&mut f, b"ok").unwrap();
        let mut buf = [0u8; 2];
        std::io::Read::read_exact(&mut &r, &mut buf).unwrap();
        assert_eq!(&buf, b"ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_round_trip() {
        let s = Setup {
            routes: vec![
                Route {
                    net: "0/0".parse().unwrap(),
                    exclude: false,
                },
                Route {
                    net: "1.2.3.4".parse().unwrap(),
                    exclude: true,
                },
            ],
            nameservers: vec!["8.8.8.8".parse().unwrap()],
            tcp_port: 12300,
            dns_port: 12299,
            fix_loopback: true,
            tun_mtu: Some(9000),
            tun_dns: true,
        };
        let text = s.encode(7);
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("SETUP 7"));
        let mut back = Setup::default();
        for l in lines.take_while(|l| *l != "END") {
            back.apply_line(l).unwrap();
        }
        assert_eq!(back.routes, s.routes);
        assert_eq!(back.nameservers, s.nameservers);
        assert_eq!(
            (back.tcp_port, back.dns_port, back.fix_loopback),
            (12300, 12299, true)
        );
        assert_eq!(back.tun_mtu, Some(9000));
        assert!(back.tun_dns);
    }
}
