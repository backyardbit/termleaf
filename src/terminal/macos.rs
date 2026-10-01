use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

pub(super) fn wait_for_hangup(input: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: kqueue takes no arguments and returns a new descriptor or an error.
    let descriptor = unsafe { libc::kqueue() };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: kqueue returned a new, valid descriptor and this is its sole owner.
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let mut event = libc::kevent {
        ident: usize::try_from(input.as_raw_fd()).map_err(io::Error::other)?,
        filter: libc::EVFILT_READ,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: queue and input remain live; event is one initialized registration and no output buffer is requested.
    let registered = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if registered < 0 {
        return Err(io::Error::last_os_error());
    }
    loop {
        // SAFETY: queue and input remain live; event is writable storage for one notification. No terminal bytes are read.
        let received = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                std::ptr::null(),
            )
        };
        if received < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if event.flags & libc::EV_ERROR != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(event.data).map_err(io::Error::other)?,
            ));
        }
        if event.flags & libc::EV_EOF != 0 {
            return Ok(());
        }
    }
}
