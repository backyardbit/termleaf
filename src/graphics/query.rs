use std::io::{self, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Instant;

use ratatui_image::picker::cap_parser::{Parser, QueryStdioOptions, Response};

pub fn read(
    input: BorrowedFd<'_>,
    output: &mut impl Write,
    tmux: bool,
    options: QueryStdioOptions,
) -> io::Result<Vec<Response>> {
    let timeout = options.timeout;
    output.write_all(Parser::query(tmux, options).as_bytes())?;
    output.flush()?;
    let deadline = Instant::now() + timeout;
    let mut parser = Parser::new();
    let mut responses = Vec::new();
    while let Some(byte) = read_byte(input, deadline)? {
        for response in parser.push(char::from(byte)) {
            if response == Response::Status {
                return Ok(responses);
            }
            responses.push(response);
        }
    }
    Ok(responses)
}

fn read_byte(input: BorrowedFd<'_>, deadline: Instant) -> io::Result<Option<u8>> {
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(None);
        };
        let timeout =
            libc::c_int::try_from(left.as_millis().saturating_add(1)).unwrap_or(libc::c_int::MAX);
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor is a live pollfd, its count is one, and input stays borrowed throughout the call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if ready == 0 {
            return Ok(None);
        }
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let mut byte = 0u8;
        // SAFETY: input is a borrowed descriptor and byte is a live one-byte buffer. The startup query is its only reader.
        let read =
            unsafe { libc::read(input.as_raw_fd(), std::ptr::from_mut(&mut byte).cast(), 1) };
        match read {
            1 => return Ok(Some(byte)),
            0 => return Ok(None),
            _ => {
                let error = io::Error::last_os_error();
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    #[test]
    fn an_unanswered_query_leaves_the_next_key_for_the_input_loop() {
        let (mut terminal, mut input) = UnixStream::pair().unwrap();
        let replies = read(
            input.as_fd(),
            &mut Vec::new(),
            false,
            QueryStdioOptions {
                timeout: Duration::from_millis(10),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(replies.is_empty());
        terminal.write_all(b"F").unwrap();
        input
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut key = [0];
        input.read_exact(&mut key).unwrap();
        assert_eq!(&key, b"F");
    }

    #[test]
    fn a_successful_query_keeps_capabilities_and_the_key_after_its_last_reply() {
        let (mut terminal, mut input) = UnixStream::pair().unwrap();
        terminal
            .write_all(b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;OK\x1b\\\x1b[6;20;10t\x1b[0nF")
            .unwrap();
        let replies = read(
            input.as_fd(),
            &mut Vec::new(),
            false,
            QueryStdioOptions::default(),
        )
        .unwrap();
        assert!(replies.contains(&Response::Kitty));
        assert!(replies.contains(&Response::KittyCompression));
        assert!(replies.contains(&Response::CellSize(Some((10, 20)))));
        let mut key = [0];
        input
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        input.read_exact(&mut key).unwrap();
        assert_eq!(&key, b"F");
    }

    #[test]
    fn a_partial_reply_times_out_without_a_background_reader() {
        let (mut terminal, mut input) = UnixStream::pair().unwrap();
        terminal.write_all(b"\x1b[").unwrap();
        read(
            input.as_fd(),
            &mut Vec::new(),
            false,
            QueryStdioOptions {
                timeout: Duration::from_millis(10),
                ..Default::default()
            },
        )
        .unwrap();
        terminal.write_all(b"F").unwrap();
        input
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut key = [0];
        input.read_exact(&mut key).unwrap();
        assert_eq!(&key, b"F");
    }
}
