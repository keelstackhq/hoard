//! HRD1 blob format: magic, big-endian header length, JSON header, raw artifact body.

use serde::{Deserialize, Serialize};

/// Blob magic.
pub const MAGIC: &[u8; 4] = b"HRD1";
/// Length of the fixed prefix (magic + u32 length).
pub const PREFIX_LEN: usize = 8;
/// Largest accepted JSON header length.
pub const MAX_HEADER_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub v: u8,
    pub duration_ms: Option<u64>,
    pub tag: Option<String>,
    pub size: u64,
    /// Artifact hash; only written into envelopes inside a pack (spec §6.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("bad magic")]
    BadMagic,
    #[error("header too long")]
    TooLong,
    /// Invalid JSON, or a header whose `v` is not 1.
    #[error("invalid header: {0}")]
    Json(String),
    #[error("incomplete header")]
    Incomplete,
}

/// Magic + length + JSON header. The body follows.
pub fn encode_header(h: &Header) -> Vec<u8> {
    let json = serde_json::to_vec(h).expect("header serializes");
    let mut out = Vec::with_capacity(PREFIX_LEN + json.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out
}

/// Parses the header from the start of a blob; returns it and the body offset.
pub fn decode_header(prefix: &[u8]) -> Result<(Header, usize), EnvelopeError> {
    let n = prefix.len().min(4);
    if prefix[..n] != MAGIC[..n] {
        return Err(EnvelopeError::BadMagic);
    }
    if prefix.len() < PREFIX_LEN {
        return Err(EnvelopeError::Incomplete);
    }
    let len = u32::from_be_bytes([prefix[4], prefix[5], prefix[6], prefix[7]]) as usize;
    if len > MAX_HEADER_LEN {
        return Err(EnvelopeError::TooLong);
    }
    let end = PREFIX_LEN + len;
    if prefix.len() < end {
        return Err(EnvelopeError::Incomplete);
    }
    let h: Header = serde_json::from_slice(&prefix[PREFIX_LEN..end])
        .map_err(|e| EnvelopeError::Json(e.to_string()))?;
    if h.v != 1 {
        return Err(EnvelopeError::Json(format!("unsupported version {}", h.v)));
    }
    Ok((h, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(json: &str) -> Vec<u8> {
        let mut b = b"HRD1".to_vec();
        b.extend_from_slice(&(json.len() as u32).to_be_bytes());
        b.extend_from_slice(json.as_bytes());
        b
    }

    #[test]
    fn envelope_roundtrip() {
        let h = Header {
            v: 1,
            duration_ms: Some(1234),
            tag: Some("c2lnbg==".into()),
            size: 10,
            hash: None,
        };
        let mut blob = encode_header(&h);
        blob.extend_from_slice(b"0123456789");
        let (back, off) = decode_header(&blob).unwrap();
        assert_eq!(back, h);
        assert_eq!(&blob[off..], b"0123456789");
    }

    #[test]
    fn envelope_rejects_garbage() {
        assert!(matches!(
            decode_header(b"PK\x03\x04rest"),
            Err(EnvelopeError::BadMagic)
        ));
        let mut too_long = b"HRD1".to_vec();
        too_long.extend_from_slice(&5000u32.to_be_bytes());
        assert!(matches!(
            decode_header(&too_long),
            Err(EnvelopeError::TooLong)
        ));
        assert!(matches!(
            decode_header(b"HRD1\x00"),
            Err(EnvelopeError::Incomplete)
        ));
    }

    #[test]
    fn header_json_shape_keeps_nulls() {
        let h = Header {
            v: 1,
            duration_ms: None,
            tag: None,
            size: 3,
            hash: None,
        };
        let b = encode_header(&h);
        assert_eq!(
            &b[8..],
            br#"{"v":1,"duration_ms":null,"tag":null,"size":3}"#
        );
        let (back, _) = decode_header(&b).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn hash_roundtrips_and_is_optional() {
        let h = Header {
            v: 1,
            duration_ms: None,
            tag: None,
            size: 3,
            hash: Some("abc123".into()),
        };
        let b = encode_header(&h);
        assert_eq!(
            &b[8..],
            br#"{"v":1,"duration_ms":null,"tag":null,"size":3,"hash":"abc123"}"#
        );
        assert_eq!(decode_header(&b).unwrap().0, h);

        let old = blob(r#"{"v":1,"duration_ms":null,"tag":null,"size":3}"#);
        assert_eq!(decode_header(&old).unwrap().0.hash, None);
    }

    #[test]
    fn short_inputs() {
        assert!(matches!(decode_header(b""), Err(EnvelopeError::Incomplete)));
        assert!(matches!(
            decode_header(b"HR"),
            Err(EnvelopeError::Incomplete)
        ));
        assert!(matches!(decode_header(b"XY"), Err(EnvelopeError::BadMagic)));
        // header body truncated
        let b = blob(r#"{"v":1,"duration_ms":null,"tag":null,"size":3}"#);
        assert!(matches!(
            decode_header(&b[..b.len() - 1]),
            Err(EnvelopeError::Incomplete)
        ));
    }

    #[test]
    fn invalid_json_and_version() {
        assert!(matches!(
            decode_header(&blob("{nope")),
            Err(EnvelopeError::Json(_))
        ));
        assert!(matches!(
            decode_header(&blob(r#"{"v":2,"duration_ms":null,"tag":null,"size":3}"#)),
            Err(EnvelopeError::Json(_))
        ));
    }
}
