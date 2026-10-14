//! Cloudflare R2 over the S3 API.

use opendal::{Operator, services};

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
    Ok(Operator::new(s3)?)
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

    #[test]
    fn rejects_empty_credentials() {
        assert!(s3_operator("http://127.0.0.1:9", R2_REGION, "b", "", "s").is_err());
        assert!(s3_operator("http://127.0.0.1:9", R2_REGION, "b", "k", "").is_err());
    }
}
