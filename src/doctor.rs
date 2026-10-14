//! `hoard doctor`: checks the configured storage and prints one line per check.

use anyhow::bail;
use bytes::Bytes;

use crate::config::{Config, LifecycleMode, Storage};
use crate::envelope::Header;
use crate::r2::lifecycle::{ensure_lifecycle, r2_client, rule_id};
use crate::serve::storage_layout;
use crate::store::{GetOutcome, PutOutcome, Store, build_operator};

const BODY: &[u8] = b"hoard doctor round-trip";

/// Runs every check, printing `✓ ...` or `✗ ...` (with the fix) to stdout; fails if any
/// check failed. The lifecycle rule is only verified, never written.
pub async fn run_doctor(cfg: &Config) -> anyhow::Result<()> {
    let (prefix, _) = storage_layout(cfg)?;
    let mut failed = 0;
    if let (Storage::R2, Some(r2)) = (cfg.storage, &cfg.r2) {
        let id = rule_id(&r2.prefix);
        let client = r2_client(&r2.endpoint(), &r2.access_key_id, &r2.secret_access_key);
        let days = cfg.max_age_days;
        match ensure_lifecycle(&client, &r2.bucket, &r2.prefix, days, LifecycleMode::Verify).await {
            Ok(_) => println!("✓ lifecycle rule {id} ({days} days)"),
            Err(e) => {
                failed += 1;
                println!("✗ lifecycle rule {id}: {e}");
            }
        }
    }
    let key = format!("{prefix}_hoard/doctor");
    match round_trip(cfg, &key).await {
        Ok(()) => println!("✓ bucket read/write"),
        Err(e) => {
            failed += 1;
            println!("✗ bucket read/write ({key}): {e:#}");
        }
    }
    if failed > 0 {
        bail!("{failed} doctor check(s) failed; see above");
    }
    Ok(())
}

/// Puts a small envelope at `key`, reads it back, compares, and deletes it.
async fn round_trip(cfg: &Config, key: &str) -> anyhow::Result<()> {
    let store = Store::new(build_operator(cfg)?);
    // A previous run may have left the object behind; keys are immutable, so start clean.
    store.delete(key).await?;
    let header = Header {
        v: 1,
        duration_ms: None,
        tag: None,
        size: BODY.len() as u64,
    };
    let body = futures::stream::iter([Ok(Bytes::from_static(BODY))]);
    if store.put(key, &header, body).await? != PutOutcome::Created {
        bail!("write was not stored (another doctor running?)");
    }
    let got = match store.get(key).await? {
        GetOutcome::Hit(_, reader, off) => reader.read(off..).await?.to_vec(),
        GetOutcome::Miss => bail!("object missing right after the write"),
        GetOutcome::Corrupt => bail!("object corrupt right after the write"),
    };
    store.delete(key).await?;
    if got != BODY {
        bail!(
            "read back {} bytes that differ from what was written",
            got.len()
        );
    }
    Ok(())
}
