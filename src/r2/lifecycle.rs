//! The R2 lifecycle rule that keeps the bucket bounded (spec §7.1).
//!
//! Rule `hoard-<prefix with / replaced by ->`: objects under the prefix expire after
//! `max-age-days`, unfinished multipart uploads after 1 day. Other rules in the bucket are
//! written back as read: every field the S3 lifecycle model defines is kept (elements unknown
//! to aws-sdk-s3 would be lost on our Put).

use std::time::Duration;

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::{
    BehaviorVersion, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation,
};
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::types::{
    AbortIncompleteMultipartUpload, BucketLifecycleConfiguration, ExpirationStatus,
    LifecycleExpiration, LifecycleRule, LifecycleRuleFilter,
};

use crate::config::LifecycleMode;

/// Lifecycle rule ID for `prefix`: `cache/` → `hoard-cache-`.
pub fn rule_id(prefix: &str) -> String {
    format!("hoard-{}", prefix.replace('/', "-"))
}

/// The wrangler command that sets the rule by hand.
pub fn fix_command(bucket: &str, prefix: &str, max_age_days: u32) -> String {
    format!(
        "npx wrangler r2 bucket lifecycle add {bucket} {} {prefix} --expire-days {max_age_days} --abort-multipart-days 1",
        rule_id(prefix)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStatus {
    AlreadyOk,
    Created,
    Updated,
    /// `r2-lifecycle=manual`: nothing was checked.
    Skipped,
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// `verify` mode found the rule missing (`differs: false`) or different (`differs: true`).
    #[error(
        "the R2 lifecycle rule that keeps the bucket bounded {}. Ask a bucket admin to run:\n{fix}\n\
         or use --r2-lifecycle auto to let hoard set it, or --r2-lifecycle manual to skip the check",
        if *.differs { "differs from what hoard needs" } else { "is missing" }
    )]
    Missing { differs: bool, fix: String },
    #[error(
        "hoard is not allowed to read or set the R2 lifecycle rule that keeps the bucket bounded \
         (AccessDenied). Ask a bucket admin to run:\n{fix}\n\
         or use an R2 token with Admin Read & Write, or --r2-lifecycle manual to skip the check"
    )]
    AccessDenied { fix: String },
    /// Transient or unexpected (network, timeout, 5xx). `error`'s outermost context is a
    /// short reason such as `timed out` (see [`LifecycleError::reason`]).
    #[error(
        "{}\nIf the rule is missing, a bucket admin can add it with:\n{fix}",
        crate::redact::redacted(.error)
    )]
    Other { error: anyhow::Error, fix: String },
}

impl LifecycleError {
    /// A few words for the summary line `Lifecycle: not checked (<reason>)`.
    pub fn reason(&self) -> String {
        match self {
            Self::Missing { differs: true, .. } => "rule differs".into(),
            Self::Missing { differs: false, .. } => "rule missing".into(),
            Self::AccessDenied { .. } => "AccessDenied".into(),
            Self::Other { error, .. } => error.to_string(),
        }
    }
}

/// An S3 client for R2 with only the given static credentials (no env, profile or IMDS
/// sources), region `auto`, path-style addressing (MinIO needs it, R2 accepts it), rustls,
/// and bounded request time so a hung endpoint cannot stall startup.
pub fn r2_client(endpoint: &str, key_id: &str, secret: &str) -> aws_sdk_s3::Client {
    r2_client_with(
        endpoint,
        key_id,
        secret,
        Duration::from_secs(30),
        Duration::from_secs(10),
    )
}

fn r2_client_with(
    endpoint: &str,
    key_id: &str,
    secret: &str,
    operation: Duration,
    connect: Duration,
) -> aws_sdk_s3::Client {
    let timeouts = TimeoutConfig::builder()
        .operation_timeout(operation)
        .connect_timeout(connect)
        .build();
    let conf = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .endpoint_url(endpoint)
        .region(Region::new(super::R2_REGION))
        .credentials_provider(Credentials::new(key_id, secret, None, None, "hoard"))
        .force_path_style(true)
        .timeout_config(timeouts)
        // R2 supports only the checksums the API requires.
        .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
        .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
        .build();
    aws_sdk_s3::Client::from_conf(conf)
}

/// Makes sure the bucket holds our rule (`auto`), or only checks it (`verify`), or does
/// nothing (`manual`). Every other rule is kept unchanged.
pub async fn ensure_lifecycle(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: &str,
    max_age_days: u32,
    mode: LifecycleMode,
) -> Result<LifecycleStatus, LifecycleError> {
    if mode == LifecycleMode::Manual {
        return Ok(LifecycleStatus::Skipped);
    }
    let fix = || fix_command(bucket, prefix, max_age_days);
    let days = i32::try_from(max_age_days).map_err(|_| LifecycleError::Other {
        error: anyhow::anyhow!("max-age-days {max_age_days} is too large"),
        fix: fix(),
    })?;
    let id = rule_id(prefix);

    let mut rules = match client
        .get_bucket_lifecycle_configuration()
        .bucket(bucket)
        .send()
        .await
    {
        Ok(out) => out.rules().to_vec(),
        Err(e) if e.code() == Some("NoSuchLifecycleConfiguration") => Vec::new(),
        Err(e) => return Err(classify(e, fix(), "cannot read the R2 lifecycle rules")),
    };

    let existing = rules.iter().position(|r| r.id() == Some(id.as_str()));
    if let Some(i) = existing
        && rule_matches(&rules[i], prefix, days)
    {
        return Ok(LifecycleStatus::AlreadyOk);
    }
    if mode == LifecycleMode::Verify {
        return Err(LifecycleError::Missing {
            differs: existing.is_some(),
            fix: fix(),
        });
    }

    let ours = our_rule(&id, prefix, days);
    let status = match existing {
        Some(i) => {
            rules[i] = ours;
            LifecycleStatus::Updated
        }
        None => {
            rules.push(ours);
            LifecycleStatus::Created
        }
    };
    let config = BucketLifecycleConfiguration::builder()
        .set_rules(Some(rules))
        .build()
        .map_err(|e| LifecycleError::Other {
            error: anyhow::Error::new(e).context("invalid lifecycle configuration"),
            fix: fix(),
        })?;
    client
        .put_bucket_lifecycle_configuration()
        .bucket(bucket)
        .lifecycle_configuration(config)
        .send()
        .await
        .map_err(|e| classify(e, fix(), "cannot set the R2 lifecycle rule"))?;
    Ok(status)
}

fn our_rule(id: &str, prefix: &str, days: i32) -> LifecycleRule {
    LifecycleRule::builder()
        .id(id)
        .status(ExpirationStatus::Enabled)
        .filter(LifecycleRuleFilter::builder().prefix(prefix).build())
        .expiration(LifecycleExpiration::builder().days(days).build())
        .abort_incomplete_multipart_upload(
            AbortIncompleteMultipartUpload::builder()
                .days_after_initiation(1)
                .build(),
        )
        .build()
        .expect("status is set")
}

/// The single prefix a rule applies to, or `None` when it has other conditions. Accepts
/// `Filter.Prefix`, `Filter.And.Prefix` with nothing else, and the legacy top-level `Prefix`.
fn rule_prefix(r: &LifecycleRule) -> Option<&str> {
    let Some(f) = r.filter() else {
        return legacy_prefix(r);
    };
    if f.tag().is_some()
        || f.object_size_greater_than().is_some()
        || f.object_size_less_than().is_some()
    {
        return None;
    }
    match (f.prefix(), f.and()) {
        (Some(p), None) => Some(p),
        (None, Some(a)) => (a.tags().is_empty()
            && a.object_size_greater_than().is_none()
            && a.object_size_less_than().is_none())
        .then(|| a.prefix())
        .flatten(),
        (None, None) => legacy_prefix(r),
        (Some(_), Some(_)) => None,
    }
}

/// The deprecated top-level `Prefix`; some servers still return rules in that shape.
#[allow(deprecated)]
fn legacy_prefix(r: &LifecycleRule) -> Option<&str> {
    r.prefix()
}

fn rule_matches(r: &LifecycleRule, prefix: &str, days: i32) -> bool {
    r.status() == &ExpirationStatus::Enabled
        && rule_prefix(r) == Some(prefix)
        && r.expiration().and_then(|e| e.days()) == Some(days)
        && r.abort_incomplete_multipart_upload()
            .and_then(|a| a.days_after_initiation())
            == Some(1)
}

/// `AccessDenied` is a setup error with the fix; anything else is `Other` with a short reason.
fn classify<E>(e: SdkError<E, HttpResponse>, fix: String, what: &str) -> LifecycleError
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    if e.code() == Some("AccessDenied") {
        return LifecycleError::AccessDenied { fix };
    }
    let reason = match &e {
        SdkError::TimeoutError(_) => "timed out".to_string(),
        SdkError::DispatchFailure(d) if d.is_timeout() => "timed out".to_string(),
        SdkError::DispatchFailure(_) => "endpoint unreachable".to_string(),
        SdkError::ServiceError(s) => match e.code() {
            Some(code) => code.to_string(),
            None => format!("HTTP {}", s.raw().status().as_u16()),
        },
        SdkError::ResponseError(_) => "unreadable response".to_string(),
        _ => "request failed".to_string(),
    };
    let detail = aws_sdk_s3::error::DisplayErrorContext(&e).to_string();
    LifecycleError::Other {
        error: anyhow::anyhow!("{what}: {detail}").context(reason),
        fix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_s3::types::{LifecycleRuleAndOperator, Tag};

    fn base() -> aws_sdk_s3::types::builders::LifecycleRuleBuilder {
        LifecycleRule::builder()
            .id("hoard-cache-")
            .status(ExpirationStatus::Enabled)
            .expiration(LifecycleExpiration::builder().days(7).build())
            .abort_incomplete_multipart_upload(
                AbortIncompleteMultipartUpload::builder()
                    .days_after_initiation(1)
                    .build(),
            )
    }

    #[test]
    fn our_rule_matches_itself() {
        assert!(rule_matches(
            &our_rule("hoard-cache-", "cache/", 7),
            "cache/",
            7
        ));
        assert!(!rule_matches(
            &our_rule("hoard-cache-", "cache/", 7),
            "cache/",
            8
        ));
        assert!(!rule_matches(
            &our_rule("hoard-cache-", "other/", 7),
            "cache/",
            7
        ));
    }

    #[test]
    fn accepts_and_prefix_and_legacy_prefix() {
        let and = base()
            .filter(
                LifecycleRuleFilter::builder()
                    .and(LifecycleRuleAndOperator::builder().prefix("cache/").build())
                    .build(),
            )
            .build()
            .unwrap();
        assert!(rule_matches(&and, "cache/", 7));
        #[allow(deprecated)]
        let legacy = base().prefix("cache/").build().unwrap();
        assert!(rule_matches(&legacy, "cache/", 7));
        #[allow(deprecated)]
        let empty_filter = base()
            .prefix("cache/")
            .filter(LifecycleRuleFilter::builder().build())
            .build()
            .unwrap();
        assert!(rule_matches(&empty_filter, "cache/", 7));
    }

    #[test]
    fn rejects_extra_conditions_and_other_values() {
        let tag = Tag::builder().key("k").value("v").build().unwrap();
        let and_tag = base()
            .filter(
                LifecycleRuleFilter::builder()
                    .and(
                        LifecycleRuleAndOperator::builder()
                            .prefix("cache/")
                            .tags(tag.clone())
                            .build(),
                    )
                    .build(),
            )
            .build()
            .unwrap();
        assert!(!rule_matches(&and_tag, "cache/", 7));
        #[allow(deprecated)]
        let filter_tag = base()
            .prefix("cache/")
            .filter(LifecycleRuleFilter::builder().tag(tag).build())
            .build()
            .unwrap();
        assert!(!rule_matches(&filter_tag, "cache/", 7));
        let sized = base()
            .filter(
                LifecycleRuleFilter::builder()
                    .prefix("cache/")
                    .object_size_greater_than(10)
                    .build(),
            )
            .build()
            .unwrap();
        assert!(!rule_matches(&sized, "cache/", 7));
        #[allow(deprecated)]
        let disabled = base()
            .status(ExpirationStatus::Disabled)
            .prefix("cache/")
            .build()
            .unwrap();
        assert!(!rule_matches(&disabled, "cache/", 7));
        #[allow(deprecated)]
        let no_abort = LifecycleRule::builder()
            .status(ExpirationStatus::Enabled)
            .prefix("cache/")
            .expiration(LifecycleExpiration::builder().days(7).build())
            .build()
            .unwrap();
        assert!(!rule_matches(&no_abort, "cache/", 7));
    }

    #[test]
    fn errors_show_the_fix_on_its_own_line() {
        let fix = fix_command("b", "cache/", 7);
        for e in [
            LifecycleError::Missing {
                differs: false,
                fix: fix.clone(),
            },
            LifecycleError::AccessDenied { fix: fix.clone() },
        ] {
            assert!(e.to_string().lines().any(|l| l == fix), "{e}");
        }
    }

    #[test]
    fn other_error_has_short_reason_and_the_fix() {
        let fix = fix_command("b", "cache/", 7);
        let e = LifecycleError::Other {
            error: anyhow::anyhow!("dispatch failure: connection refused").context("timed out"),
            fix: fix.clone(),
        };
        assert_eq!(e.reason(), "timed out");
        let msg = e.to_string();
        assert!(msg.lines().any(|l| l == fix), "{msg}");
        assert!(msg.contains("connection refused"), "{msg}");
        let missing = LifecycleError::Missing {
            differs: true,
            fix: fix.clone(),
        };
        assert_eq!(missing.reason(), "rule differs");
        assert_eq!(
            LifecycleError::AccessDenied { fix }.reason(),
            "AccessDenied"
        );
    }

    #[test]
    fn other_error_display_is_redacted() {
        let e = LifecycleError::Other {
            error: anyhow::anyhow!("GET https://h/b?lifecycle&X-Amz-Signature=abc123")
                .context("HTTP 500"),
            fix: fix_command("b", "cache/", 7),
        };
        let msg = e.to_string();
        assert!(msg.contains("X-Amz-Signature=REDACTED"), "{msg}");
        assert!(!msg.contains("abc123"), "{msg}");
    }

    /// A server that accepts connections and never answers: the check fails within the bound.
    #[tokio::test]
    async fn hung_endpoint_times_out() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        let c = r2_client_with(
            &format!("http://{addr}"),
            "k",
            "s",
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let start = std::time::Instant::now();
        let err = ensure_lifecycle(&c, "b", "cache/", 7, LifecycleMode::Auto)
            .await
            .unwrap_err();
        assert!(matches!(err, LifecycleError::Other { .. }), "{err:?}");
        assert_eq!(err.reason(), "timed out");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }
}
