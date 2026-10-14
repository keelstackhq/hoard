//! Refresh-on-hit (spec §7.3): R2's lifecycle rule expires objects by age since the last
//! write, so an artifact that is read every day would still vanish after `max-age-days`.
//! When a GET hits an object older than half of `max-age-days`, it is copied onto itself,
//! which resets its age. This runs in the background, once per object per run; it never
//! delays or fails a response.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, anyhow};
use aws_sdk_s3::error::DisplayErrorContext;
use aws_sdk_s3::types::MetadataDirective;
use tracing::{debug, warn};

use crate::redact::redacted;
use crate::stats::Stats;

/// Refreshes running at once; a hit that finds them all busy is skipped, not queued.
const MAX_CONCURRENT: usize = 4;

/// `value` percent-encoded for the `x-amz-copy-source` header: everything except unreserved
/// characters and `/`.
fn encode_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Copies `key` onto itself. S3 only accepts a self-copy that replaces the metadata
/// (`MetadataDirective::Replace`); hoard objects carry no user metadata, so nothing changes
/// but the object's last-modified time.
pub async fn refresh_object(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> anyhow::Result<()> {
    client
        .copy_object()
        .bucket(bucket)
        .key(key)
        .copy_source(encode_path(&format!("{bucket}/{key}")))
        .metadata_directive(MetadataDirective::Replace)
        .send()
        .await
        .map_err(|e| anyhow!("CopyObject failed: {}", DisplayErrorContext(&e)))?;
    Ok(())
}

/// Decides, per GET hit, whether to refresh the object in the background.
pub struct Refresher {
    client: aws_sdk_s3::Client,
    bucket: String,
    /// Objects older than this are refreshed (`max-age-days / 2`; injectable for tests).
    threshold: Duration,
    stats: Arc<Stats>,
    /// Keys already considered in this run (refreshed, or found young).
    seen: Mutex<HashSet<String>>,
    permits: Arc<tokio::sync::Semaphore>,
}

impl Refresher {
    pub fn new(
        client: aws_sdk_s3::Client,
        bucket: String,
        threshold: Duration,
        stats: Arc<Stats>,
    ) -> Self {
        Self {
            client,
            bucket,
            threshold,
            stats,
            seen: Mutex::new(HashSet::new()),
            permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT)),
        }
    }

    /// The age at which `max_age_days` objects are refreshed: half of it.
    pub fn threshold_for(max_age_days: u32) -> Duration {
        Duration::from_secs(u64::from(max_age_days) * 86_400 / 2)
    }

    /// Called on a GET hit for `key`. Returns at once; the first call per key spawns a task
    /// that checks the object's age (one HEAD) and refreshes it when old. When all
    /// [`MAX_CONCURRENT`] slots are busy the hit is skipped and may try again later.
    pub fn on_hit(self: &Arc<Self>, key: &str) {
        if !self
            .seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_owned())
        {
            return;
        }
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            self.seen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(key);
            return;
        };
        let this = self.clone();
        let key = key.to_owned();
        tokio::spawn(async move {
            let _permit = permit;
            match this.check_and_refresh(&key).await {
                Ok(true) => {
                    this.stats
                        .refreshed
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    debug!(%key, "refreshed R2 object");
                }
                Ok(false) => {}
                Err(e) => {
                    warn!(%key, error = %redacted(&e), "R2 refresh failed; the object keeps its age")
                }
            }
        });
    }

    /// `true` when the object was old and has been refreshed.
    async fn check_and_refresh(&self, key: &str) -> anyhow::Result<bool> {
        let head = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| anyhow!("HeadObject failed: {}", DisplayErrorContext(&e)))?;
        let secs = head
            .last_modified()
            .context("HeadObject returned no last-modified time")?
            .secs();
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(secs.max(0).unsigned_abs());
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO);
        if age < self.threshold {
            return Ok(false);
        }
        refresh_object(&self.client, &self.bucket, key).await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_source_is_percent_encoded() {
        assert_eq!(
            encode_path("b/cache/turbo/v1/ab12"),
            "b/cache/turbo/v1/ab12"
        );
        assert_eq!(encode_path("b/a b+c%é"), "b/a%20b%2Bc%25%C3%A9");
    }

    #[test]
    fn threshold_is_half_of_max_age() {
        assert_eq!(Refresher::threshold_for(7), Duration::from_secs(302_400));
        assert_eq!(Refresher::threshold_for(1), Duration::from_secs(43_200));
    }
}
