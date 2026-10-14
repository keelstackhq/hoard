//! Artifact key layout and hash validation.

/// True if `hash` matches `^[a-fA-F0-9]{1,128}$`.
pub fn validate_hash(hash: &str) -> bool {
    (1..=128).contains(&hash.len()) && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Builds `{prefix}{tool}/{version}/{hash}`. The caller must validate `hash` first.
pub fn artifact_key(prefix: &str, tool: &str, version: &str, hash: &str) -> String {
    format!("{prefix}{tool}/{version}/{hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_validation() {
        assert!(validate_hash("9f2cAB01"));
        assert!(!validate_hash(""));
        assert!(!validate_hash("../etc/passwd"));
        assert!(!validate_hash(&"a".repeat(129)));
    }

    #[test]
    fn hash_validation_edges() {
        assert!(validate_hash(&"a".repeat(128)));
        assert!(validate_hash("0"));
        assert!(!validate_hash("abc\n"));
        assert!(!validate_hash("abg"));
        assert!(!validate_hash("é"));
    }

    #[test]
    fn key_layout() {
        assert_eq!(
            artifact_key("cache/", "turbo", "v1", "abc"),
            "cache/turbo/v1/abc"
        );
        assert_eq!(artifact_key("", "vp", "v2", "ff"), "vp/v2/ff");
    }
}
