//! Pack and index key names (spec §6.5): `{prefix}turbo-pack/{v}/{run_key}-{n}` and
//! `{prefix}turbo-index/{v}/{ref}/{run_key}-{m}`.

/// Longest key segment built from a branch name.
const MAX_REF_LEN: usize = 100;

/// `raw` as one key segment: every character outside `[A-Za-z0-9._-]` becomes `_`, at most
/// 100 characters. An empty result, `.` and `..` become `_`, so a segment never escapes its
/// directory.
pub fn ref_segment(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_REF_LEN)
        .collect();
    if s.is_empty() || s == "." || s == ".." {
        "_".into()
    } else {
        s
    }
}

#[derive(Debug, Clone)]
pub struct PackNames {
    pub prefix: String,
    pub cache_version: String,
    pub run_key: String,
    /// This job's ref as a key segment.
    pub ref_: String,
    /// The pull request's base ref as a key segment.
    pub base: Option<String>,
}

impl PackNames {
    /// `ref` is `GITHUB_HEAD_REF`, else `GITHUB_REF_NAME`, else `local`; `base` is
    /// `GITHUB_BASE_REF` when set. Empty variables count as unset.
    pub fn from_env(
        prefix: &str,
        cache_version: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
        let ref_ = var("GITHUB_HEAD_REF")
            .or_else(|| var("GITHUB_REF_NAME"))
            .unwrap_or_else(|| "local".into());
        Self {
            prefix: prefix.into(),
            cache_version: cache_version.into(),
            run_key: crate::vp::default_run_key(&lookup),
            ref_: ref_segment(&ref_),
            base: var("GITHUB_BASE_REF").map(|b| ref_segment(&b)),
        }
    }

    /// Pack number `n` of this server.
    pub fn pack_key(&self, n: u64) -> String {
        format!(
            "{}turbo-pack/{}/{}-{n}",
            self.prefix, self.cache_version, self.run_key
        )
    }

    /// Index number `m` of this server.
    pub fn index_key(&self, m: u64) -> String {
        format!("{}{}-{m}", self.ref_index_prefix(&self.ref_), self.run_key)
    }

    fn ref_index_prefix(&self, r: &str) -> String {
        format!("{}turbo-index/{}/{r}/", self.prefix, self.cache_version)
    }

    /// Where to look for the newest index: this ref, the base ref, and (when neither has one)
    /// every ref. All end with `/`.
    pub fn index_prefixes(&self) -> (String, Option<String>, String) {
        (
            self.ref_index_prefix(&self.ref_),
            self.base.as_deref().map(|b| self.ref_index_prefix(b)),
            format!("{}turbo-index/{}/", self.prefix, self.cache_version),
        )
    }
}
