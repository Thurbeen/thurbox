//! A way for agent output to wake the render loop.
//!
//! The loop sleeps in the terminal's input poll, which only the terminal can
//! wake, so output landing mid-sleep waited out the rest of it. While the loop
//! is waiting on a keystroke's echo it sleeps in `poll(2)` on the terminal
//! **and** on this self-pipe instead, and every reader loop writes a byte into
//! it after parsing — but only while [`arm`]ed, so output nobody is waiting on
//! costs one atomic load.
//!
//! Unix only. Elsewhere [`read_fd`] is `None` and the loop keeps polling the
//! terminal in short slices.

use std::sync::atomic::{AtomicBool, Ordering};

static ARMED: AtomicBool = AtomicBool::new(false);

/// Ask the reader loops to wake the render loop on their next output, or stop.
pub fn arm(on: bool) {
    ARMED.store(on, Ordering::SeqCst);
}

#[cfg(unix)]
mod pipe {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::sync::OnceLock;

    /// `(read, write)`, both non-blocking; `None` if the pipe could not be made.
    fn ends() -> Option<&'static (OwnedFd, OwnedFd)> {
        static PIPE: OnceLock<Option<(OwnedFd, OwnedFd)>> = OnceLock::new();
        PIPE.get_or_init(|| {
            let mut fds = [0; 2];
            // SAFETY: `pipe` writes two descriptors into a two-element array.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return None;
            }
            for fd in fds {
                // SAFETY: plain fcntl calls on descriptors this function owns.
                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFL);
                    libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
            }
            // SAFETY: both were just returned by `pipe` and are owned by nothing
            // else.
            Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
        })
        .as_ref()
    }

    pub(super) fn read_fd() -> Option<RawFd> {
        ends().map(|(read, _)| read.as_raw_fd())
    }

    pub(super) fn poke() {
        if let Some((_, write)) = ends() {
            // A full pipe already holds a wake-up, so a failed write loses none.
            // SAFETY: a one-byte write from a valid buffer to an owned fd.
            let _ = unsafe { libc::write(write.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        }
    }

    pub(super) fn drain() {
        if let Some((read, _)) = ends() {
            let mut buf = [0u8; 64];
            // SAFETY: reads into a valid local buffer from an owned,
            // non-blocking fd, until it is empty.
            while unsafe { libc::read(read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
        }
    }
}

/// Called by a reader loop once it has parsed a chunk of output.
pub fn notify() {
    // SeqCst, pairing with `arm`: the loop arms, then reads the output
    // sequence; a reader bumps the sequence, then reads this. One of the two
    // sees the other, so an echo is never both unannounced and unnoticed.
    std::sync::atomic::fence(Ordering::SeqCst);
    if ARMED.load(Ordering::SeqCst) {
        #[cfg(unix)]
        pipe::poke();
    }
}

/// The descriptor that becomes readable when armed output arrives.
#[cfg(unix)]
pub fn read_fd() -> Option<std::os::fd::RawFd> {
    pipe::read_fd()
}

/// Empty the pipe, so the next wait sleeps until the next output.
#[cfg(unix)]
pub fn drain() {
    pipe::drain();
}
