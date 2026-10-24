use std::{collections::VecDeque, io, time::Duration};

use mio::{unix::SourceFd, Events, Interest, Poll, Token};
use signal_hook_mio::v1_0::Signals;

#[cfg(feature = "event-stream")]
use crate::event::sys::Waker;
use crate::event::{
    source::EventSource, sys::unix::parse::parse_event, timeout::PollTimeout, Event, InternalEvent,
};
use crate::terminal::sys::file_descriptor::{tty_fd, FileDesc};

// Tokens to identify file descriptor
const TTY_TOKEN: Token = Token(0);
const SIGNAL_TOKEN: Token = Token(1);
#[cfg(feature = "event-stream")]
const WAKE_TOKEN: Token = Token(2);

// I (@zrzka) wasn't able to read more than 1_022 bytes when testing
// reading on macOS/Linux -> we don't need bigger buffer and 1k of bytes
// is enough.
const TTY_BUFFER_SIZE: usize = 1_024;

pub(crate) struct UnixInternalEventSource {
    poll: Poll,
    events: Events,
    parser: Parser,
    tty_buffer: [u8; TTY_BUFFER_SIZE],
    tty_fd: FileDesc<'static>,
    signals: Signals,
    #[cfg(feature = "event-stream")]
    waker: Waker,
}

impl UnixInternalEventSource {
    pub fn new() -> io::Result<Self> {
        UnixInternalEventSource::from_file_descriptor(tty_fd()?)
    }

    pub(crate) fn from_file_descriptor(input_fd: FileDesc<'static>) -> io::Result<Self> {
        let poll = Poll::new()?;
        let registry = poll.registry();

        let tty_raw_fd = input_fd.raw_fd();
        let mut tty_ev = SourceFd(&tty_raw_fd);
        registry.register(&mut tty_ev, TTY_TOKEN, Interest::READABLE)?;

        let mut signals = Signals::new([signal_hook::consts::SIGWINCH])?;
        registry.register(&mut signals, SIGNAL_TOKEN, Interest::READABLE)?;

        #[cfg(feature = "event-stream")]
        let waker = Waker::new(registry, WAKE_TOKEN)?;

        Ok(UnixInternalEventSource {
            poll,
            events: Events::with_capacity(3),
            parser: Parser::default(),
            tty_buffer: [0u8; TTY_BUFFER_SIZE],
            tty_fd: input_fd,
            signals,
            #[cfg(feature = "event-stream")]
            waker,
        })
    }
}

impl EventSource for UnixInternalEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
        if let Some(event) = self.parser.next() {
            return Ok(Some(event));
        }

        let timeout = PollTimeout::new(timeout);

        loop {
            // thurbox patch: returning a parsed event can leave unread bytes
            // behind an edge-triggered readiness notification. Recreate the
            // registration so those bytes are reported on the next poll.
            // The caller's descriptor may be blocking; do not drain it or
            // change flags shared with its other users.
            let tty_raw_fd = self.tty_fd.raw_fd();
            let mut tty_ev = SourceFd(&tty_raw_fd);
            self.poll.registry().deregister(&mut tty_ev)?;
            self.poll
                .registry()
                .register(&mut tty_ev, TTY_TOKEN, Interest::READABLE)?;

            if let Err(e) = self.poll.poll(&mut self.events, timeout.leftover()) {
                // Mio will throw an interrupted error in case of cursor position retrieval. We need to retry until it succeeds.
                // Previous versions of Mio (< 0.7) would automatically retry the poll call if it was interrupted (if EINTR was returned).
                // https://docs.rs/mio/0.7.0/mio/struct.Poll.html#notes
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                } else {
                    return Err(e);
                }
            };

            if self.events.is_empty() {
                // No readiness events = timeout
                return Ok(None);
            }

            for token in self.events.iter().map(|x| x.token()) {
                match token {
                    TTY_TOKEN => {
                        loop {
                            match self.tty_fd.read(&mut self.tty_buffer) {
                                Ok(read_count) => {
                                    // thurbox patch: EOF is not more input.
                                    if read_count == 0 {
                                        return Err(io::Error::new(
                                            io::ErrorKind::UnexpectedEof,
                                            "terminal input closed",
                                        ));
                                    }
                                    self.parser.advance(
                                        &self.tty_buffer[..read_count],
                                        read_count == TTY_BUFFER_SIZE,
                                    );
                                }
                                Err(e) => {
                                    if e.kind() == io::ErrorKind::WouldBlock {
                                        break;
                                    } else if e.kind() == io::ErrorKind::Interrupted {
                                        continue;
                                    }
                                    // thurbox patch: do not spin on input errors.
                                    return Err(e);
                                }
                            };

                            if let Some(event) = self.parser.next() {
                                return Ok(Some(event));
                            }
                            // thurbox patch: a partial escape sequence may be
                            // all that is available on a blocking descriptor.
                            break;
                        }
                    }
                    SIGNAL_TOKEN => {
                        if self.signals.pending().next() == Some(signal_hook::consts::SIGWINCH) {
                            // TODO Should we remove tput?
                            //
                            // This can take a really long time, because terminal::size can
                            // launch new process (tput) and then it parses its output. It's
                            // not a really long time from the absolute time point of view, but
                            // it's a really long time from the mio, async-std/tokio executor, ...
                            // point of view.
                            let new_size = crate::terminal::size()?;
                            return Ok(Some(InternalEvent::Event(Event::Resize(
                                new_size.0, new_size.1,
                            ))));
                        }
                    }
                    #[cfg(feature = "event-stream")]
                    WAKE_TOKEN => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "Poll operation was woken up by `Waker::wake`",
                        ));
                    }
                    _ => unreachable!("Synchronize Evented handle registration & token handling"),
                }
            }

            // Processing above can take some time, check if timeout expired
            if timeout.elapsed() {
                return Ok(None);
            }
        }
    }

    #[cfg(feature = "event-stream")]
    fn waker(&self) -> Waker {
        self.waker.clone()
    }
}

//
// Following `Parser` structure exists for two reasons:
//
//  * mimic anes Parser interface
//  * move the advancing, parsing, ... stuff out of the `try_read` method
//
#[derive(Debug)]
struct Parser {
    buffer: Vec<u8>,
    internal_events: VecDeque<InternalEvent>,
}

impl Default for Parser {
    fn default() -> Self {
        Parser {
            // This buffer is used for -> 1 <- ANSI escape sequence. Are we
            // aware of any ANSI escape sequence that is bigger? Can we make
            // it smaller?
            //
            // Probably not worth spending more time on this as "there's a plan"
            // to use the anes crate parser.
            buffer: Vec::with_capacity(256),
            // TTY_BUFFER_SIZE is 1_024 bytes. How many ANSI escape sequences can
            // fit? What is an average sequence length? Let's guess here
            // and say that the average ANSI escape sequence length is 8 bytes. Thus
            // the buffer size should be 1024/8=128 to avoid additional allocations
            // when processing large amounts of data.
            //
            // There's no need to make it bigger, because when you look at the `try_read`
            // method implementation, all events are consumed before the next TTY_BUFFER
            // is processed -> events pushed.
            internal_events: VecDeque::with_capacity(128),
        }
    }
}

impl Parser {
    fn advance(&mut self, buffer: &[u8], more: bool) {
        for (idx, byte) in buffer.iter().enumerate() {
            let more = idx + 1 < buffer.len() || more;

            self.buffer.push(*byte);

            match parse_event(&self.buffer, more) {
                Ok(Some(ie)) => {
                    self.internal_events.push_back(ie);
                    self.buffer.clear();
                }
                Ok(None) => {
                    // Event can't be parsed, because we don't have enough bytes for
                    // the current sequence. Keep the buffer and process next bytes.
                }
                Err(_) => {
                    // Event can't be parsed (not enough parameters, parameter is not a number, ...).
                    // Clear the buffer and continue with another sequence.
                    self.buffer.clear();
                }
            }
        }
    }
}

impl Iterator for Parser {
    type Item = InternalEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.internal_events.pop_front()
    }
}

// thurbox patch: selector/descriptor regressions and parser chunks that pin
// state kept between reads. No parser chunk ends on a lone ESC: with nothing
// after it, that is the Escape key. CI's Nextest job runs these with
// `cargo test --locked --manifest-path vendor/crossterm/Cargo.toml --lib thurbox_patch`.
#[cfg(test)]
mod thurbox_patch_tests {
    use super::{Parser, UnixInternalEventSource};
    use crate::event::source::EventSource;
    use crate::event::{Event, InternalEvent, KeyCode, KeyEvent};
    use crate::terminal::sys::file_descriptor::FileDesc;
    use std::{io::Write, os::fd::AsRawFd, os::unix::net::UnixStream, time::Duration};

    // thurbox patch: exercise the real selector with a borrowed blocking
    // non-TTY descriptor as well as the public-API PTY test in tui_e2e.
    #[test]
    fn a_prewritten_burst_needs_no_second_readiness_edge() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let burst = vec![b'x'; 2050];
        assert_eq!(writer.write(&burst).unwrap(), burst.len());
        #[cfg(feature = "libc")]
        let fd = FileDesc::new(reader.as_raw_fd(), false);
        #[cfg(not(feature = "libc"))]
        let fd =
            FileDesc::Borrowed(unsafe { rustix::fd::BorrowedFd::borrow_raw(reader.as_raw_fd()) });
        let mut source = UnixInternalEventSource::from_file_descriptor(fd).unwrap();
        for index in 0..burst.len() {
            assert!(
                matches!(
                    source.try_read(Some(Duration::from_millis(100))).unwrap(),
                    Some(InternalEvent::Event(Event::Key(KeyEvent {
                        code: KeyCode::Char('x'),
                        ..
                    })))
                ),
                "byte {index} was stranded"
            );
        }
        assert!(source.try_read(Some(Duration::ZERO)).unwrap().is_none());

        writer.write_all(b"\x1b[").unwrap();
        assert!(source
            .try_read(Some(Duration::from_millis(10)))
            .unwrap()
            .is_none());
        writer.write_all(b"A").unwrap();
        assert!(matches!(
            source.try_read(Some(Duration::from_millis(100))).unwrap(),
            Some(InternalEvent::Event(Event::Key(KeyEvent {
                code: KeyCode::Up,
                ..
            })))
        ));
        drop(writer);
        assert_eq!(
            source
                .try_read(Some(Duration::from_millis(100)))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        drop(source);
        assert!(reader.peer_addr().is_ok(), "borrowed input was closed");
    }

    fn typed(chunks: &[&[u8]]) -> String {
        let mut parser = Parser::default();
        for chunk in chunks {
            parser.advance(chunk, false);
        }
        parser
            .map(|event| match event {
                InternalEvent::Event(Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                })) => c,
                other => panic!("not a typed character: {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_coalesced_secondary_device_attributes_reply_is_consumed() {
        assert_eq!(typed(&[b"before\x1b[>0;10;1cafter"]), "beforeafter");
    }

    #[test]
    fn a_secondary_device_attributes_reply_split_across_reads_is_consumed() {
        assert_eq!(
            typed(&[b"before\x1b[", b">", b"0;", b"10;1", b"c", b"after"]),
            "beforeafter"
        );
    }
}
