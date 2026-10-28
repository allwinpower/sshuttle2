//! Creating the tun device (root helper) and handing its descriptor to the
//! unprivileged client over a Unix socket.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result, bail};
use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, recvmsg, sendmsg};

/// Bytes in front of each packet: macOS utun prefixes the address family as
/// a big-endian u32; Linux tun (IFF_NO_PI) has no prefix.
pub const HEADER_LEN: usize = if cfg!(target_os = "macos") { 4 } else { 0 };

/// Writes the frame prefix for an IP packet starting with `first_byte`.
pub fn header_for(first_byte: u8) -> [u8; 4] {
    let af: u32 = if first_byte >> 4 == 6 {
        libc::AF_INET6 as u32
    } else {
        libc::AF_INET as u32
    };
    af.to_be_bytes()
}

/// Creates a utun device. Returns the descriptor and the interface name.
#[cfg(target_os = "macos")]
pub fn create() -> Result<(OwnedFd, String)> {
    // SAFETY: the structs are plain data, zeroed or filled in before use, and
    // every pointer passed comes with the size of what it points to.
    unsafe {
        let fd = libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL);
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("socket(PF_SYSTEM)");
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut info: libc::ctl_info = std::mem::zeroed();
        for (i, b) in b"com.apple.net.utun_control".iter().enumerate() {
            info.ctl_name[i] = *b as libc::c_char;
        }
        if libc::ioctl(fd.as_raw_fd(), libc::CTLIOCGINFO, &mut info) < 0 {
            return Err(std::io::Error::last_os_error()).context("CTLIOCGINFO");
        }
        let mut addr: libc::sockaddr_ctl = std::mem::zeroed();
        addr.sc_len = std::mem::size_of::<libc::sockaddr_ctl>() as u8;
        addr.sc_family = libc::AF_SYSTEM as u8;
        addr.ss_sysaddr = libc::AF_SYS_CONTROL as u16;
        addr.sc_id = info.ctl_id;
        addr.sc_unit = 0; // first free utunN
        if libc::connect(
            fd.as_raw_fd(),
            (&addr as *const libc::sockaddr_ctl).cast(),
            std::mem::size_of::<libc::sockaddr_ctl>() as libc::socklen_t,
        ) < 0
        {
            return Err(std::io::Error::last_os_error()).context("connecting utun control");
        }
        let mut name = [0u8; 32];
        let mut len = name.len() as libc::socklen_t;
        if libc::getsockopt(
            fd.as_raw_fd(),
            libc::SYSPROTO_CONTROL,
            libc::UTUN_OPT_IFNAME,
            name.as_mut_ptr().cast(),
            &mut len,
        ) < 0
        {
            return Err(std::io::Error::last_os_error()).context("UTUN_OPT_IFNAME");
        }
        libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        let name = String::from_utf8_lossy(&name[..len as usize])
            .trim_end_matches('\0')
            .to_string();
        Ok((fd, name))
    }
}

/// Creates a Linux tun device (no packet-information prefix).
#[cfg(target_os = "linux")]
pub fn create() -> Result<(OwnedFd, String)> {
    // SAFETY: IfReq has the layout TUNSETIFF expects, and the descriptor is
    // owned as soon as open returns it.
    unsafe {
        let fd = libc::open(c"/dev/net/tun".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("opening /dev/net/tun");
        }
        let fd = OwnedFd::from_raw_fd(fd);
        #[repr(C)]
        struct IfReq {
            name: [u8; libc::IFNAMSIZ],
            flags: libc::c_short,
            _pad: [u8; 22],
        }
        let mut req = IfReq {
            name: [0; libc::IFNAMSIZ],
            flags: 0,
            _pad: [0; 22],
        };
        req.name[..5].copy_from_slice(b"tts%d");
        req.flags = (libc::IFF_TUN | libc::IFF_NO_PI) as libc::c_short;
        if libc::ioctl(fd.as_raw_fd(), libc::TUNSETIFF as _, &mut req) < 0 {
            return Err(std::io::Error::last_os_error()).context("TUNSETIFF");
        }
        let end = req
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(req.name.len());
        Ok((fd, String::from_utf8_lossy(&req.name[..end]).into_owned()))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn create() -> Result<(OwnedFd, String)> {
    bail!("tun mode is not supported on this OS")
}

/// Sends `fd` over a connected Unix socket, with one byte of payload.
pub fn send_fd(sock: &impl AsRawFd, fd: RawFd) -> Result<()> {
    let fds = [fd];
    let cmsg = [ControlMessage::ScmRights(&fds)];
    sendmsg::<()>(
        sock.as_raw_fd(),
        &[IoSlice::new(b"F")],
        &cmsg,
        MsgFlags::empty(),
        None,
    )
    .context("sending the tun descriptor")?;
    Ok(())
}

/// Receives one descriptor sent by `send_fd`.
pub fn recv_fd(sock: &impl AsRawFd) -> Result<OwnedFd> {
    let mut byte = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut byte)];
    let mut space = nix::cmsg_space!([RawFd; 1]);
    let msg = recvmsg::<()>(
        sock.as_raw_fd(),
        &mut iov,
        Some(&mut space),
        MsgFlags::empty(),
    )
    .context("receiving the tun descriptor")?;
    for c in msg.cmsgs().context("reading control messages")? {
        if let ControlMessageOwned::ScmRights(fds) = c
            && let Some(&fd) = fds.first()
        {
            // SAFETY: the kernel just installed this descriptor for us.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            // Keep it out of anything we start later.
            // SAFETY: the descriptor is open; F_SETFD only changes its flags.
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
            return Ok(fd);
        }
    }
    bail!("no descriptor in the helper's message")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_a_descriptor() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let (r, w) = std::os::unix::net::UnixStream::pair().unwrap();
        send_fd(&a, w.as_raw_fd()).unwrap();
        let got = recv_fd(&b).unwrap();
        // SAFETY: `got` is open.
        let flags = unsafe { libc::fcntl(got.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        // Writing through the received copy reaches the original pair.
        let mut f = std::fs::File::from(got);
        std::io::Write::write_all(&mut f, b"hi").unwrap();
        let mut buf = [0u8; 2];
        std::io::Read::read_exact(&mut &r, &mut buf).unwrap();
        assert_eq!(&buf, b"hi");
    }

    #[test]
    fn utun_header() {
        assert_eq!(header_for(0x45), (libc::AF_INET as u32).to_be_bytes());
        assert_eq!(header_for(0x60), (libc::AF_INET6 as u32).to_be_bytes());
    }
}
