//! Maintenance-VM control frames.
//!
//! A frame is one ASCII line, `VIBE1 <nonce> <kind> [args...]\n`, at most 1024
//! bytes including the newline. The nonce ties a frame to one maintenance run.
//! It is not an authenticator. This parser checks syntax only; step order is
//! enforced by the maintenance driver.
//!
//! TODO(T21): the maintenance VM reads these frames from hvc1.

#![allow(dead_code)]

use std::{
    fs::File,
    io::{self, Read},
};

pub(crate) const MAX_FRAME_LEN: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    Fail,
}

// The Prep prefix matches the wire kinds PREP-READY, PREP-STEP, and PREP-RESULT.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Frame {
    PrepReady,
    PrepStep {
        name: String,
    },
    PrepResult {
        status: Status,
        code: u32,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FrameError {
    BadNonce,
    LineTooLong,
    NonAscii,
    Malformed,
    UnknownKind,
    BadStatus,
    Truncated,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            FrameError::BadNonce => "control frame nonce does not match",
            FrameError::LineTooLong => "control frame is longer than 1024 bytes",
            FrameError::NonAscii => "control frame is not ASCII",
            FrameError::Malformed => "control frame is malformed",
            FrameError::UnknownKind => "control frame kind is unknown",
            FrameError::BadStatus => "control frame status is not ok or fail",
            FrameError::Truncated => "control frame was cut off",
        };
        write!(f, "{text}")
    }
}

impl std::error::Error for FrameError {}

pub(crate) struct FrameReader {
    buf: Vec<u8>,
    nonce: String,
    max_len: usize,
    discarding: bool,
}

impl FrameReader {
    pub(crate) fn new(nonce: impl Into<String>) -> Self {
        Self::with_max_len(nonce, MAX_FRAME_LEN)
    }

    pub(crate) fn with_max_len(nonce: impl Into<String>, max_len: usize) -> Self {
        Self {
            buf: Vec::new(),
            nonce: nonce.into(),
            max_len: max_len.max(1),
            discarding: false,
        }
    }

    /// Feed the next read. One read may contain several frames or a fragment.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Result<Frame, FrameError>> {
        let mut out = Vec::new();
        for &byte in bytes {
            if self.discarding {
                if byte == b'\n' {
                    self.discarding = false;
                    self.buf.clear();
                }
                continue;
            }
            if byte == b'\n' {
                out.push(self.parse_line());
                self.buf.clear();
                continue;
            }
            if !byte.is_ascii() {
                out.push(Err(FrameError::NonAscii));
                self.discarding = true;
                self.buf.clear();
                continue;
            }
            // `max_len` counts the trailing newline, so the payload stops one
            // byte earlier. The extra byte is discarded with the rest of the line.
            if self.buf.len() + 1 >= self.max_len {
                out.push(Err(FrameError::LineTooLong));
                self.discarding = true;
                self.buf.clear();
                continue;
            }
            self.buf.push(byte);
        }
        out
    }

    /// Report EOF. A partial line, or a line already rejected and not yet
    /// terminated, is `Truncated`.
    pub(crate) fn finish(&mut self) -> Result<(), FrameError> {
        let pending = self.discarding || !self.buf.is_empty();
        self.discarding = false;
        self.buf.clear();
        if pending {
            Err(FrameError::Truncated)
        } else {
            Ok(())
        }
    }

    fn parse_line(&self) -> Result<Frame, FrameError> {
        let Ok(line) = std::str::from_utf8(&self.buf) else {
            return Err(FrameError::NonAscii);
        };
        if line.is_empty() {
            return Err(FrameError::Malformed);
        }
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(FrameError::Malformed);
        }
        if parts.len() < 3 || parts[0] != "VIBE1" {
            return Err(FrameError::Malformed);
        }
        if !parts.iter().all(|part| field_ok(part)) {
            return Err(FrameError::Malformed);
        }
        if parts[1] != self.nonce {
            return Err(FrameError::BadNonce);
        }
        match parts[2] {
            "PREP-READY" => {
                if parts.len() != 3 {
                    return Err(FrameError::Malformed);
                }
                Ok(Frame::PrepReady)
            }
            "PREP-STEP" => {
                if parts.len() != 4 {
                    return Err(FrameError::Malformed);
                }
                Ok(Frame::PrepStep {
                    name: parts[3].to_string(),
                })
            }
            "PREP-RESULT" => {
                if parts.len() != 6 {
                    return Err(FrameError::Malformed);
                }
                let status = match parts[3] {
                    "ok" => Status::Ok,
                    "fail" => Status::Fail,
                    _ => return Err(FrameError::BadStatus),
                };
                let code: u32 = match parts[4].parse() {
                    Ok(code) if !parts[4].starts_with('+') => code,
                    _ => return Err(FrameError::Malformed),
                };
                Ok(Frame::PrepResult {
                    status,
                    code,
                    reason: parts[5].to_string(),
                })
            }
            _ => Err(FrameError::UnknownKind),
        }
    }
}

fn field_ok(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

/// 16 bytes from `/dev/urandom`, as 32 lowercase hex characters.
pub(crate) fn new_nonce() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    let mut file = File::open("/dev/urandom")?;
    file.read_exact(&mut bytes)?;
    let mut text = String::with_capacity(32);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    Ok(text)
}

pub(crate) fn encode(frame: &Frame, nonce: &str) -> String {
    match frame {
        Frame::PrepReady => format!("VIBE1 {nonce} PREP-READY\n"),
        Frame::PrepStep { name } => format!("VIBE1 {nonce} PREP-STEP {name}\n"),
        Frame::PrepResult {
            status,
            code,
            reason,
        } => {
            let status = match status {
                Status::Ok => "ok",
                Status::Fail => "fail",
            };
            format!("VIBE1 {nonce} PREP-RESULT {status} {code} {reason}\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn reader() -> FrameReader {
        FrameReader::new(NONCE)
    }

    #[test]
    fn split_reads_and_multiple_frames() {
        let step = Frame::PrepStep {
            name: "fsck".into(),
        };
        let ready = encode(&Frame::PrepReady, NONCE);
        let step_line = encode(&step, NONCE);
        let mut reader = reader();
        assert!(reader.push(&ready.as_bytes()[..8]).is_empty());
        let rest = ready.as_bytes()[8..]
            .iter()
            .chain(step_line.as_bytes())
            .copied()
            .collect::<Vec<_>>();
        let parsed = reader.push(&rest);
        assert_eq!(parsed, vec![Ok(Frame::PrepReady), Ok(step.clone())]);
        assert!(reader.finish().is_ok());
        let again = reader.push(encode(&step, NONCE).as_bytes());
        assert_eq!(again, vec![Ok(step)]);
    }

    #[test]
    fn round_trip_result_frame() {
        let frame = Frame::PrepResult {
            status: Status::Fail,
            code: 4,
            reason: "fsck".into(),
        };
        let mut reader = reader();
        let parsed = reader.push(encode(&frame, NONCE).as_bytes());
        assert_eq!(parsed, vec![Ok(frame)]);
    }

    #[test]
    fn rejects_bad_nonce_status_kind_and_shape() {
        let mut reader = reader();
        let wrong = format!("VIBE1 {} PREP-READY\n", "a".repeat(32));
        assert_eq!(
            reader.push(wrong.as_bytes()),
            vec![Err(FrameError::BadNonce)]
        );

        let status = format!("VIBE1 {NONCE} PREP-RESULT NOPE 1 fsck\n");
        assert_eq!(
            reader.push(status.as_bytes()),
            vec![Err(FrameError::BadStatus)]
        );

        let hello = format!("VIBE1 {NONCE} HELLO\n");
        assert_eq!(
            reader.push(hello.as_bytes()),
            vec![Err(FrameError::UnknownKind)]
        );

        let gap = format!("VIBE1  {NONCE} PREP-READY\n");
        assert_eq!(
            reader.push(gap.as_bytes()),
            vec![Err(FrameError::Malformed)]
        );

        let extra = format!("VIBE1 {NONCE} PREP-READY now\n");
        assert_eq!(
            reader.push(extra.as_bytes()),
            vec![Err(FrameError::Malformed)]
        );
        assert!(reader.finish().is_ok());
    }

    #[test]
    fn overlong_line_resynchronizes() {
        let mut reader = reader();
        let mut bytes = b"VIBE1 ".to_vec();
        bytes.extend(std::iter::repeat_n(b'A', MAX_FRAME_LEN));
        bytes.push(b'\n');
        bytes.extend(encode(&Frame::PrepReady, NONCE).as_bytes());
        let parsed = reader.push(&bytes);
        assert_eq!(
            parsed,
            vec![Err(FrameError::LineTooLong), Ok(Frame::PrepReady)]
        );
    }

    #[test]
    fn non_ascii_and_truncated_eof() {
        let mut reader = reader();
        let mut bytes = format!("VIBE1 {NONCE} PREP-ST").into_bytes();
        bytes.push(0xC3);
        bytes.push(b'\n');
        bytes.extend(encode(&Frame::PrepReady, NONCE).as_bytes());
        assert_eq!(
            reader.push(&bytes),
            vec![Err(FrameError::NonAscii), Ok(Frame::PrepReady)]
        );

        reader.push(b"VIBE1 ");
        assert_eq!(reader.finish(), Err(FrameError::Truncated));
        assert!(reader.finish().is_ok());
    }

    #[test]
    fn nonce_is_32_lowercase_hex() {
        let first = new_nonce().unwrap();
        let second = new_nonce().unwrap();
        assert_eq!(first.len(), 32);
        assert!(
            first
                .chars()
                .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
        );
        assert_ne!(first, second);
    }
}
