//! CLI flags, `HOARD_*` environment variables and defaults. Precedence: flag > env > default.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use clap::{Args, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Storage {
    Github,
    R2,
    Fs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    ReadWrite,
    Read,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LifecycleMode {
    Auto,
    Verify,
    Manual,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid size {0:?}: use a number with an optional KB, MB, GB or TB suffix, e.g. 5GB")]
    BadSize(String),
    #[error("storage=r2 needs {}; set the flag or its HOARD_* env var", .0.join(", "))]
    MissingR2(Vec<String>),
    #[error(
        "cache-version {0:?} must be one segment of A-Z a-z 0-9 . _ - (not . or .., no /); \
         set --cache-version or HOARD_CACHE_VERSION (default v1)"
    )]
    BadCacheVersion(String),
    #[error(
        "r2-prefix {0:?} must be segments of A-Z a-z 0-9 . _ - joined by / and must end with / \
         (no leading /, //, spaces, . or .. segments); for example cache/"
    )]
    BadPrefix(String),
    #[error(
        "r2-endpoint {0:?} must start with http:// or https:// (for example http://127.0.0.1:9000)"
    )]
    BadEndpoint(String),
    #[error(
        "max-age-days {0} must be between 1 and 36500; set --max-age-days or HOARD_MAX_AGE_DAYS"
    )]
    BadMaxAge(u32),
}

/// Parse a size such as `5GB`, `750MB` or `1024` (powers of 1000).
pub fn parse_size(s: &str) -> Result<u64, ConfigError> {
    let bad = || ConfigError::BadSize(s.to_string());
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (digits, unit) = t.split_at(split);
    let n: u64 = digits.parse().map_err(|_| bad())?;
    let mult: u64 = match unit.to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        _ => return Err(bad()),
    };
    n.checked_mul(mult).ok_or_else(bad)
}

/// An r2 prefix is one or more segments of `[A-Za-z0-9._-]` (not `.` or `..`), each followed
/// by `/`. Anything else could be rewritten by OpenDAL's path normalisation (leading `/`,
/// `//`, whitespace) and reach keys outside the prefix, e.g. `/` = the whole bucket.
fn valid_prefix(p: &str) -> bool {
    let Some(body) = p.strip_suffix('/') else {
        return false;
    };
    body.split('/').all(valid_segment)
}

/// One key segment: `[A-Za-z0-9._-]+`, not `.` or `..`. Used for r2-prefix segments and for
/// cache-version, which become key path segments (on fs, `..` would leave fs-root).
fn valid_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg != "."
        && seg != ".."
        && seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn runner_temp_or(file: &str, fallback: &str) -> PathBuf {
    match std::env::var_os("RUNNER_TEMP") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join(file),
        _ => PathBuf::from(fallback),
    }
}

pub fn default_port_file() -> PathBuf {
    runner_temp_or("hoard.port", "./.hoard/port")
}

pub fn default_token_file() -> PathBuf {
    runner_temp_or("hoard.token", "./.hoard/token")
}

/// Port and token file locations; enough for `hoard env` to find a running server.
#[derive(Args, Debug, Clone)]
pub struct TokenPaths {
    /// File holding the actual listening port.
    #[arg(long, env = "HOARD_PORT_FILE")]
    pub port_file: Option<PathBuf>,
    /// File holding the bearer token.
    #[arg(long, env = "HOARD_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
}

#[derive(clap::Parser, Clone)]
pub struct ServeArgs {
    #[arg(long, env = "HOARD_STORAGE", value_enum, default_value = "github")]
    pub storage: Storage,
    #[arg(long, env = "HOARD_MODE", value_enum, default_value = "read-write")]
    pub mode: Mode,
    /// Every GET/HEAD is a miss (the store is never read); uploads still happen.
    #[arg(long, env = "HOARD_NO_READS", value_parser = clap::builder::BoolishValueParser::new())]
    pub no_reads: bool,
    /// Part of every key; bump to purge.
    #[arg(long, env = "HOARD_CACHE_VERSION", default_value = "v1")]
    pub cache_version: String,
    /// 0 picks a free port.
    #[arg(long, env = "HOARD_PORT", default_value_t = 0)]
    pub port: u16,
    #[command(flatten)]
    pub paths: TokenPaths,
    /// Bearer token; random when unset.
    #[arg(long, env = "HOARD_TOKEN")]
    pub token: Option<String>,
    /// Markdown summary written on shutdown.
    #[arg(long, env = "HOARD_SUMMARY_FILE")]
    pub summary_file: Option<PathBuf>,
    #[arg(long, env = "HOARD_FS_ROOT", default_value = "./.hoard/store")]
    pub fs_root: PathBuf,
    #[arg(long, env = "HOARD_R2_ACCOUNT_ID")]
    pub r2_account_id: Option<String>,
    /// S3 endpoint URL override (e.g. MinIO); when set, r2-account-id is not needed.
    #[arg(long, env = "HOARD_R2_ENDPOINT")]
    pub r2_endpoint: Option<String>,
    #[arg(long, env = "HOARD_R2_ACCESS_KEY_ID")]
    pub r2_access_key_id: Option<String>,
    #[arg(long, env = "HOARD_R2_SECRET_ACCESS_KEY")]
    pub r2_secret_access_key: Option<String>,
    #[arg(long, env = "HOARD_R2_BUCKET")]
    pub r2_bucket: Option<String>,
    #[arg(long, env = "HOARD_R2_PREFIX", default_value = "cache/")]
    pub r2_prefix: String,
    #[arg(long, env = "HOARD_R2_LIFECYCLE", value_enum, default_value = "auto")]
    pub r2_lifecycle: LifecycleMode,
    #[arg(long, env = "HOARD_MAX_AGE_DAYS", default_value_t = 7)]
    pub max_age_days: u32,
    /// R2 size cap: KB, MB, GB or TB (powers of 1000).
    #[arg(long, env = "HOARD_MAX_SIZE", default_value = "5GB")]
    pub max_size: String,
}

fn redact<T>(v: &Option<T>) -> Option<&'static str> {
    v.as_ref().map(|_| "<redacted>")
}

impl fmt::Debug for ServeArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServeArgs")
            .field("storage", &self.storage)
            .field("mode", &self.mode)
            .field("no_reads", &self.no_reads)
            .field("cache_version", &self.cache_version)
            .field("port", &self.port)
            .field("paths", &self.paths)
            .field("token", &redact(&self.token))
            .field("summary_file", &self.summary_file)
            .field("fs_root", &self.fs_root)
            .field("r2_account_id", &self.r2_account_id)
            .field("r2_endpoint", &self.r2_endpoint)
            .field("r2_access_key_id", &redact(&self.r2_access_key_id))
            .field("r2_secret_access_key", &redact(&self.r2_secret_access_key))
            .field("r2_bucket", &self.r2_bucket)
            .field("r2_prefix", &self.r2_prefix)
            .field("r2_lifecycle", &self.r2_lifecycle)
            .field("max_age_days", &self.max_age_days)
            .field("max_size", &self.max_size)
            .finish()
    }
}

/// Names of `HOARD_*` variables whose value is empty. CI systems export empty values for
/// unset secrets; those must behave as unset.
fn empty_hoard_keys<I: IntoIterator<Item = (OsString, OsString)>>(vars: I) -> Vec<OsString> {
    vars.into_iter()
        .filter(|(k, v)| v.is_empty() && k.to_string_lossy().starts_with("HOARD_"))
        .map(|(k, _)| k)
        .collect()
}

/// Remove empty `HOARD_*` variables from the process environment. Call first thing in `main`,
/// before any thread or runtime exists.
pub fn drop_empty_hoard_env() {
    for key in empty_hoard_keys(std::env::vars_os()) {
        // SAFETY: called at the start of main, while the process is still single-threaded.
        unsafe { std::env::remove_var(key) };
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct R2Config {
    /// `None` only when `endpoint` overrides the R2 endpoint.
    pub account_id: Option<String>,
    /// Endpoint override (`r2-endpoint`), e.g. a MinIO URL.
    pub endpoint: Option<String>,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub prefix: String,
}

impl R2Config {
    /// The `r2-endpoint` override if set, else `https://<account_id>.r2.cloudflarestorage.com`.
    pub fn endpoint(&self) -> String {
        match (&self.endpoint, &self.account_id) {
            (Some(e), _) => e.trim_end_matches('/').to_string(),
            (None, a) => format!(
                "https://{}.r2.cloudflarestorage.com",
                a.as_deref().unwrap_or_default()
            ),
        }
    }
}

impl fmt::Debug for R2Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("R2Config")
            .field("account_id", &self.account_id)
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish()
    }
}

#[derive(Clone)]
pub struct Config {
    pub storage: Storage,
    pub mode: Mode,
    /// GET/HEAD and the batch query always miss without reading the store.
    pub no_reads: bool,
    pub cache_version: String,
    pub port: u16,
    pub port_file: PathBuf,
    pub token_file: PathBuf,
    pub token: Option<String>,
    pub summary_file: Option<PathBuf>,
    pub fs_root: PathBuf,
    pub r2: Option<R2Config>,
    pub r2_lifecycle: LifecycleMode,
    pub max_age_days: u32,
    pub max_size: u64,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("storage", &self.storage)
            .field("mode", &self.mode)
            .field("no_reads", &self.no_reads)
            .field("cache_version", &self.cache_version)
            .field("port", &self.port)
            .field("port_file", &self.port_file)
            .field("token_file", &self.token_file)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("summary_file", &self.summary_file)
            .field("fs_root", &self.fs_root)
            .field("r2", &self.r2)
            .field("r2_lifecycle", &self.r2_lifecycle)
            .field("max_age_days", &self.max_age_days)
            .field("max_size", &self.max_size)
            .finish()
    }
}

impl TryFrom<ServeArgs> for Config {
    type Error = ConfigError;

    fn try_from(a: ServeArgs) -> Result<Self, ConfigError> {
        if !valid_segment(&a.cache_version) {
            return Err(ConfigError::BadCacheVersion(a.cache_version));
        }
        let max_size = parse_size(&a.max_size)?;
        if !(1..=36_500).contains(&a.max_age_days) {
            return Err(ConfigError::BadMaxAge(a.max_age_days));
        }
        let r2 = if a.storage == Storage::R2 {
            let non_empty = |v: Option<String>| v.filter(|s| !s.is_empty());
            let endpoint = non_empty(a.r2_endpoint);
            let (account_id, access_key_id, secret_access_key, bucket) = (
                non_empty(a.r2_account_id),
                non_empty(a.r2_access_key_id),
                non_empty(a.r2_secret_access_key),
                non_empty(a.r2_bucket),
            );
            let account_ok = account_id.is_some() || endpoint.is_some();
            let (true, Some(access_key_id), Some(secret_access_key), Some(bucket)) = (
                account_ok,
                access_key_id.clone(),
                secret_access_key.clone(),
                bucket.clone(),
            ) else {
                let missing = [
                    ("r2-account-id (or r2-endpoint)", !account_ok),
                    ("r2-access-key-id", access_key_id.is_none()),
                    ("r2-secret-access-key", secret_access_key.is_none()),
                    ("r2-bucket", bucket.is_none()),
                ]
                .into_iter()
                .filter(|(_, m)| *m)
                .map(|(n, _)| n.to_string())
                .collect();
                return Err(ConfigError::MissingR2(missing));
            };
            if let Some(e) = &endpoint
                && !(e.starts_with("http://") || e.starts_with("https://"))
            {
                return Err(ConfigError::BadEndpoint(e.clone()));
            }
            if !valid_prefix(&a.r2_prefix) {
                return Err(ConfigError::BadPrefix(a.r2_prefix));
            }
            Some(R2Config {
                account_id,
                endpoint,
                access_key_id,
                secret_access_key,
                bucket,
                prefix: a.r2_prefix,
            })
        } else {
            None
        };
        Ok(Config {
            storage: a.storage,
            mode: a.mode,
            no_reads: a.no_reads,
            cache_version: a.cache_version,
            port: a.port,
            port_file: a.paths.port_file.unwrap_or_else(default_port_file),
            token_file: a.paths.token_file.unwrap_or_else(default_token_file),
            token: a.token,
            summary_file: a.summary_file,
            fs_root: a.fs_root,
            r2,
            r2_lifecycle: a.r2_lifecycle,
            max_age_days: a.max_age_days,
            max_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("5GB").unwrap(), 5_000_000_000);
        assert_eq!(parse_size("750MB").unwrap(), 750_000_000);
        assert_eq!(parse_size("1TB").unwrap(), 1_000_000_000_000);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert!(parse_size("5 parsecs").is_err());
    }
    #[test]
    fn r2_requires_all_fields() {
        let args = ServeArgs::parse_from(["serve", "--storage", "r2", "--r2-bucket", "b"]);
        let err = Config::try_from(args).unwrap_err().to_string();
        assert!(err.contains("r2-account-id"));
    }
    #[test]
    fn prefix_must_end_with_slash() {
        let args = ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-account-id",
            "a",
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "b",
            "--r2-prefix",
            "cache",
        ]);
        assert!(
            Config::try_from(args)
                .unwrap_err()
                .to_string()
                .contains("must end with /")
        );
    }
    fn r2_with_prefix(prefix: &str) -> Result<Config, ConfigError> {
        Config::try_from(ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-account-id",
            "a",
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "b",
            "--r2-prefix",
            prefix,
        ]))
    }
    #[test]
    fn prefix_must_be_plain_segments() {
        for bad in [
            "/", "//", "/cache/", "cache//", " cache/", "cache/ ", "a/../b/", "../", "./", "a/./",
            "ca che/", "ca\tche/", "c\u{7}/", "cache",
        ] {
            let err = r2_with_prefix(bad).unwrap_err().to_string();
            assert!(err.contains("r2-prefix"), "{bad:?}: {err}");
            assert!(err.contains("cache/"), "{bad:?}: {err}");
        }
        for good in ["cache/", "team-a/turbo_v1/", "a.b/c/", "X9/"] {
            assert_eq!(r2_with_prefix(good).unwrap().r2.unwrap().prefix, good);
        }
    }
    #[test]
    fn debug_redacts_secrets() {
        let args = ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-account-id",
            "a",
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "topsecret",
            "--r2-bucket",
            "b",
            "--token",
            "tok123",
        ]);
        let dbg = format!("{:?}", Config::try_from(args).unwrap());
        assert!(!dbg.contains("topsecret") && !dbg.contains("tok123"));
        assert!(!dbg.contains("\"k\""));
        let args = ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-access-key-id",
            "akid-xyz",
            "--r2-secret-access-key",
            "topsecret",
            "--token",
            "tok123",
        ]);
        let dbg = format!("{args:?}");
        for s in ["topsecret", "tok123", "akid-xyz"] {
            assert!(!dbg.contains(s), "{s} leaked");
        }
    }
    #[test]
    fn cache_version_must_be_one_plain_segment() {
        let parse = |v: &str| {
            Config::try_from(ServeArgs::parse_from([
                "serve",
                "--storage",
                "fs",
                "--cache-version",
                v,
            ]))
        };
        for ok in ["v1", "v2.1", "2026-10_a", "..v"] {
            assert_eq!(parse(ok).unwrap().cache_version, ok);
        }
        for bad in [
            "..", ".", "../../..", "a/b", "v1/", "/v1", "v 1", "v\\1", "v1\n",
        ] {
            let e = parse(bad).unwrap_err().to_string();
            assert!(e.contains("cache-version"), "{bad:?}: {e}");
            assert!(e.contains("A-Z a-z 0-9 . _ -"), "{bad:?}: {e}");
        }
    }

    #[test]
    fn empty_values_rejected() {
        let a = ServeArgs::parse_from(["serve", "--storage", "fs", "--cache-version", ""]);
        assert!(
            Config::try_from(a)
                .unwrap_err()
                .to_string()
                .contains("cache-version")
        );
        let a = ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-account-id",
            "a",
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "",
        ]);
        assert!(
            Config::try_from(a)
                .unwrap_err()
                .to_string()
                .contains("r2-bucket")
        );
    }
    #[test]
    fn finds_only_empty_hoard_vars() {
        let vars = [
            ("HOARD_PORT", ""),
            ("HOARD_TOKEN", "x"),
            ("OTHER", ""),
            ("HOARD_R2_BUCKET", ""),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        assert_eq!(
            empty_hoard_keys(vars),
            vec!["HOARD_PORT", "HOARD_R2_BUCKET"]
        );
    }
    #[test]
    fn r2_endpoint_defaults_to_account() {
        let c = Config::try_from(ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-account-id",
            "acc123",
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "b",
        ]))
        .unwrap();
        assert_eq!(
            c.r2.unwrap().endpoint(),
            "https://acc123.r2.cloudflarestorage.com"
        );
    }
    #[test]
    fn r2_endpoint_override_replaces_account_id() {
        let c = Config::try_from(ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-endpoint",
            "http://127.0.0.1:9000",
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "b",
        ]))
        .unwrap();
        let r2 = c.r2.unwrap();
        assert_eq!(r2.endpoint(), "http://127.0.0.1:9000");
        assert!(format!("{r2:?}").contains("http://127.0.0.1:9000"));
        // Without the override the account id is still required.
        let err = Config::try_from(ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-access-key-id",
            "k",
            "--r2-secret-access-key",
            "s",
            "--r2-bucket",
            "b",
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains("r2-account-id"), "{err}");
    }
    #[test]
    fn r2_endpoint_must_be_http_url() {
        for bad in ["127.0.0.1:9000", "ftp://x", "minio"] {
            let err = Config::try_from(ServeArgs::parse_from([
                "serve",
                "--storage",
                "r2",
                "--r2-endpoint",
                bad,
                "--r2-access-key-id",
                "k",
                "--r2-secret-access-key",
                "s",
                "--r2-bucket",
                "b",
            ]))
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("r2-endpoint") && err.contains("http://127.0.0.1:9000"),
                "{err}"
            );
        }
    }
    #[test]
    fn r2_endpoint_override_debug_redacts_secrets() {
        let c = Config::try_from(ServeArgs::parse_from([
            "serve",
            "--storage",
            "r2",
            "--r2-endpoint",
            "http://127.0.0.1:9000",
            "--r2-access-key-id",
            "akid-xyz",
            "--r2-secret-access-key",
            "topsecret",
            "--r2-bucket",
            "b",
        ]))
        .unwrap();
        let dbg = format!("{c:?}");
        for s in ["topsecret", "akid-xyz"] {
            assert!(!dbg.contains(s), "{s} leaked");
        }
    }
    #[test]
    fn max_age_days_must_be_in_range() {
        for bad in ["0", "36501"] {
            let a = ServeArgs::parse_from(["serve", "--storage", "fs", "--max-age-days", bad]);
            let err = Config::try_from(a).unwrap_err().to_string();
            assert!(
                err.contains("max-age-days") && err.contains("36500"),
                "{err}"
            );
        }
        for ok in ["1", "36500"] {
            let a = ServeArgs::parse_from(["serve", "--storage", "fs", "--max-age-days", ok]);
            assert!(Config::try_from(a).is_ok());
        }
    }
    #[test]
    fn no_reads_flag() {
        let parse = |args: &[&str]| {
            Config::try_from(ServeArgs::parse_from(args.iter().copied()))
                .unwrap()
                .no_reads
        };
        assert!(!parse(&["serve", "--storage", "fs"]));
        assert!(parse(&["serve", "--storage", "fs", "--no-reads"]));
    }
    #[test]
    fn defaults() {
        let c = Config::try_from(ServeArgs::parse_from(["serve", "--storage", "fs"])).unwrap();
        assert_eq!(
            (
                c.mode,
                c.cache_version.as_str(),
                c.port,
                c.max_age_days,
                c.max_size
            ),
            (Mode::ReadWrite, "v1", 0, 7, 5_000_000_000)
        );
    }
}
