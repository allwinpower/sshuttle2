//! Async reads and writes on raw file descriptors (the server's stdin and
//! stdout, which sshd connects to pipes). `tokio::io::stdin` would go through
//! a blocking thread pool for every read; this uses epoll/kqueue directly.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf};

pub fn set_nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    let fd = fd.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Grows a pipe's kernel buffer (Linux only; a no-op elsewhere or on
/// non-pipes). Fewer, larger reads and writes between sshd and us.
pub fn grow_pipe(fd: &impl AsRawFd, size: i32) {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::fcntl(fd.as_raw_fd(), libc::F_SETPIPE_SZ, size);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (fd, size);
}

fn register(fd: OwnedFd, interest: Interest) -> io::Result<AsyncFd<OwnedFd>> {
    // SAFETY: the AsyncFd owns the OwnedFd, so the descriptor stays open and
    // unchanged for the AsyncFd's whole life.
    unsafe { AsyncFd::register_with_interest(fd, interest) }.map_err(|e| e.into_parts().1)
}

/// Makes `close()` send RST instead of FIN, so the far end of a connection
/// that broke mid-stream sees an error rather than a clean (truncated) end.
pub fn set_linger_zero(fd: &impl AsRawFd) {
    let l = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&l as *const libc::linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

/// Sets a socket's kernel send and receive buffers.
pub fn set_socket_buffers(fd: &impl AsRawFd, size: i32) {
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                (&size as *const i32).cast(),
                std::mem::size_of::<i32>() as libc::socklen_t,
            );
        }
    }
}

pub struct FdReader {
    fd: AsyncFd<OwnedFd>,
}

impl FdReader {
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        set_nonblocking(&fd)?;
        Ok(FdReader {
            fd: register(fd, Interest::READABLE)?,
        })
    }
}

impl AsyncRead for FdReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.fd.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            let res = guard.try_io(|fd| {
                let n = unsafe {
                    libc::read(fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match res {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

pub struct FdWriter {
    fd: AsyncFd<OwnedFd>,
}

impl FdWriter {
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        set_nonblocking(&fd)?;
        Ok(FdWriter {
            fd: register(fd, Interest::WRITABLE)?,
        })
    }
}

impl AsyncWrite for FdWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // Try the write first and wait for readiness only when it would
        // block. Some descriptors (macOS utun control sockets) may never
        // signal writable through kqueue, though writes to them succeed.
        let n = unsafe { libc::write(self.fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Poll::Ready(Ok(n as usize));
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            return Poll::Ready(Err(err));
        }
        loop {
            let mut guard = ready!(self.fd.poll_write_ready(cx))?;
            let res = guard.try_io(|fd| {
                let n = unsafe { libc::write(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match res {
                Ok(r) => return Poll::Ready(r),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
