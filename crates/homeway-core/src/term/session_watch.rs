//! Bounded, uncompressed session-metadata snapshots for SUBSCRIBE/SESSION_LIST.
//!
//! Each payload is `[revision:u64 LE][flags:u8][JSON chunk]`; bit 0 means more.
//! Revisions are service-local. Reconnect starts a fresh assembler and baseline.

use std::io::Write;
use std::time::Duration;

pub const MAX_JSON_BYTES: usize = 1 << 20;
pub const CHUNK_BYTES: usize = 32 << 10;
pub const MAX_SUBSCRIPTIONS: usize = 16;
pub const HEARTBEAT: Duration = Duration::from_secs(5);
pub const STALE_TIMEOUT: Duration = Duration::from_secs(15);
const HEADER_BYTES: usize = 9;
const MORE: u8 = 1;
const MAX_PARTS: usize = MAX_JSON_BYTES / CHUNK_BYTES;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("session metadata exceeds {MAX_JSON_BYTES} bytes")]
    Limit,
    #[error("invalid session metadata fragment header")]
    Header,
    #[error("session metadata revision is zero, regressed, or changed within a group")]
    Revision,
    #[error("session metadata fragment flags are unsupported")]
    Flags,
    #[error("session metadata fragment count exceeds {MAX_PARTS}")]
    Parts,
    #[error("session metadata JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn encode(revision: u64, json: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    if revision == 0 {
        return Err(Error::Revision);
    }
    if json.len() > MAX_JSON_BYTES {
        return Err(Error::Limit);
    }
    let mut out = Vec::new();
    let chunks = json.len().max(1).div_ceil(CHUNK_BYTES);
    for index in 0..chunks {
        let start = index * CHUNK_BYTES;
        let end = (start + CHUNK_BYTES).min(json.len());
        let mut payload = Vec::with_capacity(HEADER_BYTES + end - start);
        payload.extend_from_slice(&revision.to_le_bytes());
        payload.push(if index + 1 < chunks { MORE } else { 0 });
        payload.extend_from_slice(&json[start..end]);
        out.push(payload);
    }
    Ok(out)
}

#[derive(Default)]
pub struct Assembler {
    revision: Option<u64>,
    last_revision: u64,
    parts: usize,
    bytes: Vec<u8>,
}

impl Assembler {
    pub fn push(&mut self, payload: &[u8]) -> Result<Option<(u64, Vec<u8>)>, Error> {
        let result = self.push_inner(payload);
        if result.is_err() {
            self.revision = None;
            self.parts = 0;
            self.bytes = Vec::new();
        }
        result
    }

    fn push_inner(&mut self, payload: &[u8]) -> Result<Option<(u64, Vec<u8>)>, Error> {
        if payload.len() < HEADER_BYTES || payload.len() > HEADER_BYTES + CHUNK_BYTES {
            return Err(Error::Header);
        }
        let revision = u64::from_le_bytes(payload[..8].try_into().expect("header checked"));
        if revision == 0
            || revision < self.last_revision
            || self.revision.is_some_and(|r| r != revision)
        {
            return Err(Error::Revision);
        }
        let flags = payload[8];
        if flags & !MORE != 0 {
            return Err(Error::Flags);
        }
        let chunk = &payload[HEADER_BYTES..];
        if flags == MORE && chunk.is_empty() {
            return Err(Error::Header);
        }
        self.parts += 1;
        if self.parts > MAX_PARTS {
            return Err(Error::Parts);
        }
        if self.bytes.len() + chunk.len() > MAX_JSON_BYTES {
            return Err(Error::Limit);
        }
        self.revision = Some(revision);
        self.bytes.extend_from_slice(chunk);
        if flags == MORE {
            return Ok(None);
        }
        self.last_revision = revision;
        self.revision = None;
        self.parts = 0;
        Ok(Some((revision, std::mem::take(&mut self.bytes))))
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

pub(crate) fn json_bytes(value: &impl serde::Serialize) -> Result<Vec<u8>, Error> {
    struct Bounded {
        bytes: Vec<u8>,
        full: bool,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len() + bytes.len() > MAX_JSON_BYTES {
                self.full = true;
                return Err(std::io::Error::other("session metadata limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded {
        bytes: Vec::new(),
        full: false,
    };
    let result = serde_json::to_writer(&mut out, value);
    if out.full {
        return Err(Error::Limit);
    }
    result?;
    Ok(out.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_budget_multiframe_and_same_revision_heartbeat() {
        let bytes = vec![b'x'; MAX_JSON_BYTES];
        let frames = encode(7, &bytes).unwrap();
        assert_eq!(frames.len(), MAX_PARTS);
        let mut assembler = Assembler::default();
        for _ in 0..2 {
            let mut complete = None;
            for frame in &frames {
                assert!(frame.len() <= super::super::frames::MAX_PAYLOAD);
                complete = assembler.push(frame).unwrap();
            }
            assert_eq!(complete.unwrap(), (7, bytes.clone()));
        }
        assert!(matches!(
            encode(7, &vec![0; MAX_JSON_BYTES + 1]),
            Err(Error::Limit)
        ));
    }

    #[test]
    fn rejects_bad_groups_and_reconnect_resets_revision() {
        let mut assembler = Assembler::default();
        let mut part = encode(7, b"hello").unwrap().remove(0);
        part[8] = MORE;
        assert!(assembler.push(&part).unwrap().is_none());
        assert!(matches!(
            assembler.push(&encode(8, b"x").unwrap()[0]),
            Err(Error::Revision)
        ));
        assert!(assembler
            .push(&encode(8, b"x").unwrap()[0])
            .unwrap()
            .is_some());
        assert!(matches!(
            assembler.push(&encode(7, b"x").unwrap()[0]),
            Err(Error::Revision)
        ));
        assembler.reset();
        assert!(assembler
            .push(&encode(1, b"x").unwrap()[0])
            .unwrap()
            .is_some());
        part[8] = 2;
        assert!(matches!(assembler.push(&part), Err(Error::Flags)));
        assert!(matches!(assembler.push(&[0; 8]), Err(Error::Header)));
        assert!(matches!(assembler.push(&[0; 9]), Err(Error::Revision)));
        assert!(matches!(
            assembler.push(&vec![0; HEADER_BYTES + CHUNK_BYTES + 1]),
            Err(Error::Header)
        ));
        part[8] = MORE;
        for _ in 0..MAX_PARTS {
            assembler.push(&part).unwrap();
        }
        assert!(matches!(assembler.push(&part), Err(Error::Parts)));
    }

    #[test]
    fn bounded_serialization_includes_escape_expansion() {
        assert!(matches!(
            json_bytes(&"\0".repeat(MAX_JSON_BYTES / 2)),
            Err(Error::Limit)
        ));
        assert_eq!(
            json_bytes(&serde_json::json!({"sessions":[]})).unwrap(),
            br#"{"sessions":[]}"#
        );
    }

    #[test]
    fn fixed_wire_vectors() {
        let raw = include_str!("../../../../fixtures/term/session-subscription.json");
        let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            fixture["subscribeOp"],
            super::super::frames::Op::SUBSCRIBE.0
        );
        assert_eq!(
            fixture["sessionListOp"],
            super::super::frames::Op::SESSION_LIST.0
        );
        for case in fixture["cases"].as_array().unwrap() {
            let body = case["json"].as_str().unwrap().as_bytes();
            let expected: Vec<u8> = case["payloadHex"]
                .as_str()
                .unwrap()
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            assert_eq!(
                encode(case["revision"].as_u64().unwrap(), body).unwrap(),
                vec![expected.clone()]
            );
            assert_eq!(
                Assembler::default().push(&expected).unwrap().unwrap().1,
                body
            );
        }
    }
}
