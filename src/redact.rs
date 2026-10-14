//! Redaction of secrets in error text before it is logged or printed.
//!
//! Storage errors can carry signed URLs: an Azure SAS (`sig=`, used by the GitHub cache) or
//! an S3 presigned URL (`X-Amz-Signature=`), and URLs with `user:password@`. Every log or
//! print of a storage, OpenDAL or SDK error goes through [`redact`] or [`redacted`].

use std::fmt::Display;

/// Query parameters whose values are secret or part of a signature (compared ignoring case).
const SECRET_PARAMS: &[&str] = &[
    "sig",
    "se",
    "sp",
    "sv",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
];

const MASK: &[u8] = b"REDACTED";

/// Bytes that end a query value or a URL authority in free text.
fn ends_value(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'&' | b'#' | b'"' | b'\'' | b'<' | b'>' | b',' | b';' | b')' | b']' | b'}' | b'\\'
        )
}

/// `s` with the values of [`SECRET_PARAMS`] replaced by `REDACTED` and the userinfo
/// (`user:password@`) removed from every URL.
pub fn redact(s: &str) -> String {
    let s = strip_userinfo(s.as_bytes());
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        out.push(b);
        i += 1;
        if b != b'?' && b != b'&' {
            continue;
        }
        let rest = &s[i..];
        let Some(name) = SECRET_PARAMS
            .iter()
            .find(|p| rest.len() > p.len() && rest[..p.len()].eq_ignore_ascii_case(p.as_bytes()))
        else {
            continue;
        };
        if rest[name.len()] != b'=' {
            continue;
        }
        out.extend_from_slice(&rest[..=name.len()]);
        out.extend_from_slice(MASK);
        i += name.len() + 1;
        while i < s.len() && !ends_value(s[i]) {
            i += 1;
        }
    }
    // Only ASCII bytes were cut or inserted, so the text stays valid UTF-8.
    String::from_utf8(out).expect("redaction keeps UTF-8 intact")
}

/// Removes `user[:password]@` after every `://`.
fn strip_userinfo(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(b"://") {
            out.extend_from_slice(b"://");
            i += 3;
            let end = s[i..]
                .iter()
                .position(|&b| b == b'/' || b == b'?' || ends_value(b))
                .map_or(s.len(), |p| i + p);
            if let Some(at) = s[i..end].iter().rposition(|&b| b == b'@') {
                i += at + 1;
            }
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// An error (with its causes, `{:#}`) as redacted text, for logs and messages.
pub fn redacted(e: &(impl Display + ?Sized)) -> String {
    redact(&format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_sas_url() {
        let url = "https://acct.blob.core.windows.net/cache/hoard%2Fturbo?sv=2024-08-04&se=2026-10-14T12%3A00%3A00Z&sr=b&sp=r&sig=AbC%2Bdef%3D";
        let got = redact(&format!("Unexpected at read, context: {{ uri: {url} }}"));
        assert_eq!(
            got,
            "Unexpected at read, context: { uri: https://acct.blob.core.windows.net/cache/hoard%2Fturbo?sv=REDACTED&se=REDACTED&sr=b&sp=REDACTED&sig=REDACTED }"
        );
    }

    #[test]
    fn s3_presigned_url() {
        let url = "https://b.r2.cloudflarestorage.com/cache/x?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20261014%2Fauto%2Fs3%2Faws4_request&X-Amz-Date=20261014T000000Z&X-Amz-Expires=300&X-Amz-Security-Token=tok123&X-Amz-SignedHeaders=host&x-amz-signature=deadbeef";
        let got = redact(url);
        assert_eq!(
            got,
            "https://b.r2.cloudflarestorage.com/cache/x?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=REDACTED&X-Amz-Date=20261014T000000Z&X-Amz-Expires=300&X-Amz-Security-Token=REDACTED&X-Amz-SignedHeaders=host&x-amz-signature=REDACTED"
        );
        for secret in ["AKIDEXAMPLE", "tok123", "deadbeef"] {
            assert!(!got.contains(secret), "{got}");
        }
    }

    #[test]
    fn userinfo_is_stripped() {
        assert_eq!(
            redact(
                "dial http://user:pa55@127.0.0.1:9000/b?x=1 failed; see https://example.com/a@b"
            ),
            "dial http://127.0.0.1:9000/b?x=1 failed; see https://example.com/a@b"
        );
    }

    #[test]
    fn leaves_other_text_alone() {
        for s in [
            "",
            "plain text with sig and se= but no query",
            "https://h/p?signature=x&sex=1&spam=2",
            "ünïcode ?sig",
        ] {
            assert_eq!(redact(s), s);
        }
        assert_eq!(redact("a?sig=x\"b"), "a?sig=REDACTED\"b");
        assert_eq!(redact("?SIG=x"), "?SIG=REDACTED");
    }

    #[test]
    fn redacted_formats_the_cause_chain() {
        let e = anyhow::anyhow!("GET https://h/k?sig=s3cr3t").context("cannot read");
        let got = redacted(&e);
        assert_eq!(got, "cannot read: GET https://h/k?sig=REDACTED");
    }
}
