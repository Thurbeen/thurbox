use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

static ARMED: AtomicPtr<AtomicU64> = AtomicPtr::new(std::ptr::null_mut());

pub fn arm(seq: Option<&AtomicU64>) {
    let target = seq.map_or(std::ptr::null_mut(), |seq| {
        (seq as *const AtomicU64).cast_mut()
    });
    ARMED.store(target, Ordering::SeqCst);
}

#[cfg(unix)]
mod pipe {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::sync::OnceLock;

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

pub fn notify(seq: &AtomicU64) {
    // SeqCst, pairing with `arm`: the loop arms, then reads the output
    // sequence; a reader bumps the sequence, then reads this. One of the two
    // sees the other, so an echo is never both unannounced and unnoticed.
    std::sync::atomic::fence(Ordering::SeqCst);
    if std::ptr::eq(ARMED.load(Ordering::SeqCst), seq) {
        #[cfg(unix)]
        pipe::poke();
    }
}

#[cfg(unix)]
pub fn read_fd() -> Option<std::os::fd::RawFd> {
    pipe::read_fd()
}

#[cfg(unix)]
pub fn drain() {
    pipe::drain();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn woken() -> bool {
        let mut fds = [libc::pollfd {
            fd: read_fd().expect("pipe"),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one valid pollfd, not blocking.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) } > 0;
        drain();
        ready
    }

    #[test]
    fn only_the_armed_pane_wakes_the_loop_and_only_while_armed() {
        let echoing = AtomicU64::new(0);
        let flooding = AtomicU64::new(0);
        drain();

        arm(Some(&echoing));
        notify(&flooding);
        assert!(!woken(), "another pane's output woke the loop");
        notify(&echoing);
        assert!(
            woken(),
            "the pane the echo is owed by did not wake the loop"
        );

        arm(None);
        notify(&echoing);
        assert!(!woken(), "output woke the loop with nothing owed");
    }
}
