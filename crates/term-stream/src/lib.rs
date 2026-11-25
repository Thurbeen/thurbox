use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Snapshot,
    Scrollback,
    Viewers,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    pub cursor: (u16, u16),
    pub normal: Vec<String>,
    pub alternate: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exit {
    // `None` when a signal ended the program: POSIX gives it no exit status.
    Known { code: Option<i64> },
    Unknown { why: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Down {
    Hello(Hello),
    Snapshot(Snapshot),
    Output(Vec<u8>),
    Resized(Size),
    SizedElsewhere(bool),
    Resync(Snapshot),
    State(Vec<u8>),
    Exit(Exit),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Up {
    Input(Vec<u8>),
    Paste(String),
    Resize(Size),
    Detach,
}

#[derive(Serialize, Deserialize)]
struct Elsewhere {
    elsewhere: bool,
}

pub trait Frame: Sized {
    fn encode(&self) -> (u8, Vec<u8>);
    fn decode(kind: u8, payload: Vec<u8>) -> io::Result<Self>;
}

fn to_json(value: &impl Serialize) -> Vec<u8> {
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

pub fn write(w: &mut impl Write, frame: &impl Frame) -> io::Result<()> {
    w.write_all(&to_bytes(frame))?;
    w.flush()
}

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
