use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::thread;

pub fn restore(mut terminal: ratatui::DefaultTerminal) {
    let _ = ratatui::try_restore();
    if terminal.show_cursor().is_err() {
        std::mem::forget(terminal);
    }
}

pub fn on_hangup(ended: impl FnOnce() + Send + 'static) {
    thread::spawn(move || {
        let input = io::stdin();
        let _ = wait_for_hangup(input.as_fd());
        ended();
    });
}

fn wait_for_hangup(input: BorrowedFd<'_>) -> io::Result<()> {
    loop {
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: 0,
            revents: 0,
        };
        // SAFETY: descriptor is one live pollfd and input remains borrowed until poll returns. Poll observes hangup without reading input.
        let result = unsafe { libc::poll(&mut descriptor, 1, -1) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn terminal_closure_is_observed_without_consuming_queued_user_input() {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: both descriptor outputs are valid pointers; null optional arguments request the default terminal settings.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "{}", io::Error::last_os_error());
        // SAFETY: openpty returned a new, valid master descriptor and this is its sole owner.
        let mut terminal = unsafe { File::from_raw_fd(master) };
        // SAFETY: openpty returned a new, valid slave descriptor and this is its sole owner.
        let mut input = unsafe { File::from_raw_fd(slave) };
        terminal.write_all(b"F/page\n").unwrap();
        let watched = input.try_clone().unwrap();
        let (send, done) = mpsc::channel();
        thread::spawn(move || {
            send.send(wait_for_hangup(watched.as_fd())).unwrap();
        });
        let mut keys = [0; 7];
        input.read_exact(&mut keys).unwrap();
        assert_eq!(&keys, b"F/page\n");
        drop(terminal);
        done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    }
}
