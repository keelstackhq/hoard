//! Cloudflare R2 over the S3 API.

use std::time::Duration;

use opendal::layers::TimeoutLayer;
use opendal::{Operator, services};

pub mod lifecycle;

/// Region used for R2 (R2 ignores the region but S3 signing needs one).
pub const R2_REGION: &str = "auto";

/// An S3 operator with only the given static credentials. Every implicit source (AWS_* env
/// vars, `~/.aws`, EC2/IMDS) is disabled so a runner's own AWS setup can never be used.
/// Path-style addressing: MinIO needs it and R2 accepts it.
pub fn s3_operator(
    endpoint: &str,
    region: &str,
    bucket: &str,
    key_id: &str,
    secret: &str,
) -> anyhow::Result<Operator> {
    s3_operator_with(endpoint, region, bucket, key_id, secret, Timeouts::DEFAULT)
}

/// Request time bounds; a hung connection becomes an error (a miss), never a stalled build.
#[derive(Debug, Clone, Copy)]
struct Timeouts {
    /// Whole non-IO operation (stat, delete, list page, ...).
    op: Duration,
    /// Each IO step of a read or write (one chunk, one part).
    io: Duration,
}

impl Timeouts {
    // 60 s per operation, 15 s per IO step: generous for R2, short enough not to stall CI.
    const DEFAULT: Self = Self {
        op: Duration::from_secs(60),
        io: Duration::from_secs(15),
    };
}

fn s3_operator_with(
    endpoint: &str,
    region: &str,
    bucket: &str,
    key_id: &str,
    secret: &str,
    t: Timeouts,
) -> anyhow::Result<Operator> {
    if key_id.is_empty() || secret.is_empty() {
        anyhow::bail!("R2 access key id and secret access key must not be empty");
    }
    // Default HTTP transport (reqwest + rustls); idempotent.
    opendal::install_default();
    let s3 = services::S3::default()
        .endpoint(endpoint)
        .region(region)
        .bucket(bucket)
        .access_key_id(key_id)
        .secret_access_key(secret)
        .disable_config_load()
        .disable_ec2_metadata();
    let timeout = TimeoutLayer::new().with_timeout(t.op).with_io_timeout(t.io);
    Ok(Operator::new(s3)?.layer(timeout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_with_static_credentials() {
        let op = s3_operator("http://127.0.0.1:9", R2_REGION, "b", "k", "s").unwrap();
        assert_eq!(op.info().scheme(), "s3");
        assert!(op.info().capability().write_with_if_not_exists);
    }

    /// A server that accepts connections and never answers: requests fail within the bound
    /// instead of hanging (a stalled build is a failed build).
    // The only lib test that sends S3 requests: OpenDAL's HTTP client is process-wide and its
    // pooled connections die with this test's runtime (see tests/common::s3_runtime).
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
        let t = Timeouts {
            op: Duration::from_secs(1),
            io: Duration::from_secs(1),
        };
        let op = s3_operator_with(&format!("http://{addr}"), R2_REGION, "b", "k", "s", t).unwrap();
        let store = crate::store::Store::new(op);
        let start = std::time::Instant::now();
        assert!(store.get("cache/aa").await.is_err());
        let body = futures::stream::iter(vec![Ok(bytes::Bytes::from_static(b"x"))]);
        let h = crate::envelope::Header {
            v: 1,
            duration_ms: None,
            tag: None,
            size: 1,
        };
        assert!(store.put("cache/aa", &h, body).await.is_err());
        assert!(
            start.elapsed() < Duration::from_secs(6),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn rejects_empty_credentials() {
        assert!(s3_operator("http://127.0.0.1:9", R2_REGION, "b", "", "s").is_err());
        assert!(s3_operator("http://127.0.0.1:9", R2_REGION, "b", "k", "").is_err());
    }
}
