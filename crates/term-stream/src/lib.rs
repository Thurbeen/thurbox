//! The terminal stream (docs/runtime/README.md#the-terminal-stream): the
//! frames a view and the process `fleet attach` names exchange over that
//! process's stdin and stdout, and their codec.
//!
//! A frame is a type byte, a payload length as a big-endian `u32`, then the
//! payload. Output, input and state are raw bytes; paste is UTF-8 text; every
//! other payload is a JSON object. This crate is the wire format and nothing
//! else: no process, no runtime, no transport.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// The stream protocol version a `hello` names.
pub const VERSION: u32 = 1;

/// The largest payload either side accepts: a snapshot with a long history
/// is the largest frame.
pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

/// What the end that keeps the screen can do, as `hello` lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// A `snapshot` follows `hello`, so a view never starts on an empty grid.
    Snapshot,
    /// The snapshot carries history above the screen.
    Scrollback,
    /// Several views may attach at once.
    Viewers,
    /// A `resize` is honoured.
    Resize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    pub capabilities: Vec<Capability>,
}

impl Hello {
    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// The screen at one point of the output: every `output` before it is in
/// it, and none after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    /// `(column, row)` from the top-left of the screen.
    pub cursor: (u16, u16),
    /// The normal screen's history, then its rows, one line each, styled
    /// with SGR sequences.
    pub normal: Vec<String>,
    /// The alternate screen's rows, when that is the one showing.
    pub alternate: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

/// How the process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exit {
    /// Its exit status, `None` for a signal.
    Known { code: Option<i64> },
    /// The end that keeps the screen could not tell.
    Unknown { why: String },
}

/// A frame from the end that keeps the screen to the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Down {
    Hello(Hello),
    Snapshot(Snapshot),
    Output(Vec<u8>),
    Resized(Size),
    /// Whether another view sizes the screen.
    SizedElsewhere(bool),
    /// A snapshot replacing the view's grid after dropped output.
    Resync(Snapshot),
    /// Title and input modes as terminal bytes: parsed, not counted as output.
    State(Vec<u8>),
    Exit(Exit),
}

/// A frame from the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Up {
    /// Bytes, sent as typed.
    Input(Vec<u8>),
    /// Text, bracketed when the program asked for it.
    Paste(String),
    /// A request the end that keeps the screen arbitrates.
    Resize(Size),
    /// Leave; the process keeps running.
    Detach,
}

#[derive(Serialize, Deserialize)]
struct Elsewhere {
    elsewhere: bool,
}

/// A frame of either direction.
pub trait Frame: Sized {
    /// Its type byte and payload.
    fn encode(&self) -> (u8, Vec<u8>);
    /// The frame a type byte and payload carry.
    fn decode(kind: u8, payload: Vec<u8>) -> io::Result<Self>;
}

fn to_json(value: &impl Serialize) -> Vec<u8> {
    // These types serialize infallibly: plain structs of numbers and strings.
    serde_json::to_vec(value).unwrap_or_default()
}

fn from_json<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> io::Result<T> {
    serde_json::from_slice(payload).map_err(|e| invalid(format!("frame payload: {e}")))
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

impl Frame for Down {
    fn encode(&self) -> (u8, Vec<u8>) {
        match self {
            Down::Hello(h) => (0x01, to_json(h)),
            Down::Snapshot(s) => (0x02, to_json(s)),
            Down::Output(b) => (0x03, b.clone()),
            Down::Resized(s) => (0x04, to_json(s)),
            Down::SizedElsewhere(e) => (0x05, to_json(&Elsewhere { elsewhere: *e })),
            Down::Resync(s) => (0x06, to_json(s)),
            Down::State(b) => (0x07, b.clone()),
            Down::Exit(e) => (0x08, to_json(e)),
        }
    }

    fn decode(kind: u8, payload: Vec<u8>) -> io::Result<Down> {
        Ok(match kind {
            0x01 => Down::Hello(from_json(&payload)?),
            0x02 => Down::Snapshot(from_json(&payload)?),
            0x03 => Down::Output(payload),
            0x04 => Down::Resized(from_json(&payload)?),
            0x05 => Down::SizedElsewhere(from_json::<Elsewhere>(&payload)?.elsewhere),
            0x06 => Down::Resync(from_json(&payload)?),
            0x07 => Down::State(payload),
            0x08 => Down::Exit(from_json(&payload)?),
            other => return Err(invalid(format!("unknown down frame {other:#04x}"))),
        })
    }
}

impl Frame for Up {
    fn encode(&self) -> (u8, Vec<u8>) {
        match self {
            Up::Input(b) => (0x81, b.clone()),
            Up::Paste(t) => (0x82, t.as_bytes().to_vec()),
            Up::Resize(s) => (0x83, to_json(s)),
            Up::Detach => (0x84, Vec::new()),
        }
    }

    fn decode(kind: u8, payload: Vec<u8>) -> io::Result<Up> {
        Ok(match kind {
            0x81 => Up::Input(payload),
            0x82 => {
                Up::Paste(String::from_utf8(payload).map_err(|e| invalid(format!("paste: {e}")))?)
            }
            0x83 => Up::Resize(from_json(&payload)?),
            0x84 => Up::Detach,
            other => return Err(invalid(format!("unknown up frame {other:#04x}"))),
        })
    }
}

/// `frame`'s bytes on the wire.
pub fn to_bytes(frame: &impl Frame) -> Vec<u8> {
    let (kind, payload) = frame.encode();
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(&payload);
    out
}

/// Write one frame and flush, so it reaches the other end now.
pub fn write(w: &mut impl Write, frame: &impl Frame) -> io::Result<()> {
    w.write_all(&to_bytes(frame))?;
    w.flush()
}

/// Read one frame: `None` when the stream ended between two frames, an
/// error when it ended inside one or carried one this version does not know.
pub fn read<F: Frame>(r: &mut impl Read) -> io::Result<Option<F>> {
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(invalid(format!("a {len}-byte frame is over {MAX_PAYLOAD}")));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    F::decode(head[0], payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<F: Frame + PartialEq + std::fmt::Debug>(frame: F) {
        let bytes = to_bytes(&frame);
        let back: F = read(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn every_frame_survives_the_wire() {
        let snapshot = Snapshot {
            cols: 80,
            rows: 2,
            cursor: (3, 1),
            normal: vec!["history".into(), "row 1".into(), "\u{1b}[1mrow 2".into()],
            alternate: None,
        };
        for frame in [
            Down::Hello(Hello {
                version: VERSION,
                capabilities: vec![Capability::Snapshot, Capability::Resize],
            }),
            Down::Snapshot(snapshot.clone()),
            Down::Output(b"\x1b[31mred\r\n".to_vec()),
            Down::Resized(Size {
                rows: 40,
                cols: 120,
            }),
            Down::SizedElsewhere(true),
            Down::Resync(snapshot),
            Down::State(b"\x1b]0;title\x07".to_vec()),
            Down::Exit(Exit::Known { code: Some(3) }),
            Down::Exit(Exit::Unknown { why: "gone".into() }),
        ] {
            round_trip(frame);
        }
        for frame in [
            Up::Input(vec![0, 255, 3]),
            Up::Paste("two\nlines".into()),
            Up::Resize(Size { rows: 1, cols: 1 }),
            Up::Detach,
        ] {
            round_trip(frame);
        }
    }

    #[test]
    fn the_end_of_a_stream_is_clean_only_between_frames() {
        assert!(read::<Down>(&mut [].as_slice()).unwrap().is_none());
        let bytes = to_bytes(&Down::Output(b"abc".to_vec()));
        let cut = &bytes[..bytes.len() - 1];
        assert!(read::<Down>(&mut &cut[..]).is_err());
    }

    #[test]
    fn an_unknown_type_or_an_oversized_frame_is_refused() {
        assert!(read::<Down>(&mut [0x7f, 0, 0, 0, 0].as_slice()).is_err());
        assert!(read::<Up>(&mut [0x01, 0, 0, 0, 0].as_slice()).is_err());
        assert!(read::<Down>(&mut [0x03, 0xff, 0xff, 0xff, 0xff].as_slice()).is_err());
    }
}
