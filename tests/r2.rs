//! Store checks on the R2 backend (S3 API), run against MinIO when `HOARD_TEST_S3_ENDPOINT`
//! is set. The store is built through `build_operator`, the same path the server uses.

mod common;

use aws_sdk_s3::types::{
    BucketLifecycleConfiguration, ExpirationStatus, LifecycleExpiration, LifecycleRule,
    LifecycleRuleFilter,
};
use chrono::{DateTime, TimeZone, Utc};
use common::{MinioBucket, minio_bucket, r2_config, s3_runtime as rt, store_checks as c};
use hoard::config::LifecycleMode;
use hoard::r2::lifecycle::{
    LifecycleError, LifecycleStatus, ensure_lifecycle, fix_command, r2_client, rule_id,
};
use hoard::r2::prune::{marker_key, prune};
use hoard::store::{Store, build_operator};
use std::process::{Command, Stdio};

struct S3Store {
    store: Store,
    _bucket: MinioBucket,
    _dir: tempfile::TempDir,
}

async fn s3_store() -> Option<S3Store> {
    let bucket = minio_bucket().await?;
    let dir = tempfile::tempdir().unwrap();
    let op = build_operator(&r2_config(dir.path(), &bucket)).unwrap();
    Some(S3Store {
        store: Store::new(op),
        _bucket: bucket,
        _dir: dir,
    })
}

macro_rules! s3_check {
    ($name:ident, $check:path) => {
        #[test]
        fn $name() {
            rt().block_on(async {
                let Some(s) = s3_store().await else { return };
                $check(&s.store).await;
            })
        }
    };
}

s3_check!(s3_put_get_roundtrip, c::check_put_get_roundtrip);
s3_check!(
    s3_put_existing_keeps_original,
    c::check_put_existing_keeps_original
);
s3_check!(
    s3_put_existing_large_keeps_original,
    c::check_put_existing_large_keeps_original
);
s3_check!(s3_missing_and_corrupt, c::check_missing_and_corrupt);
s3_check!(s3_head_does_not_read_body, c::check_head_does_not_read_body);
s3_check!(s3_list_and_delete, c::check_list_and_delete);
s3_check!(
    s3_failed_body_leaves_no_object,
    c::check_failed_body_leaves_no_object
);
s3_check!(
    s3_failed_large_body_leaves_no_object,
    c::check_failed_large_body_leaves_no_object
);
s3_check!(
    s3_large_body_in_small_chunks,
    c::check_large_body_in_small_chunks
);
s3_check!(
    s3_list_keeps_hoard_tmp_prefix,
    c::check_list_keeps_hoard_tmp_prefix
);

/// `put` takes the conditional-write path on S3 (no stat-then-write race), and MinIO
/// rejects a second if-not-exists write instead of overwriting.
#[test]
fn s3_reports_and_honours_conditional_write() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        let op = &s.store.op;
        assert!(op.info().capability().write_with_if_not_exists);
        op.write_with("c/aa", b"one".to_vec())
            .if_not_exists(true)
            .await
            .unwrap();
        let err = op
            .write_with("c/aa", b"two".to_vec())
            .if_not_exists(true)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), opendal::ErrorKind::ConditionNotMatch, "{err}");
        assert_eq!(op.read("c/aa").await.unwrap().to_vec(), b"one");
    })
}

/// Runs `check`, then asserts the bucket holds no unfinished multipart upload.
async fn assert_no_pending_uploads(s: &S3Store) {
    let b = &s._bucket;
    let out = common::s3_client(&b.endpoint, &b.key_id, &b.secret)
        .list_multipart_uploads()
        .bucket(&b.bucket)
        .send()
        .await
        .unwrap();
    assert!(
        out.uploads().is_empty(),
        "pending multipart uploads: {:?}",
        out.uploads()
    );
}

/// A failed multi-part body aborts the upload; no parts stay behind.
#[test]
fn s3_failed_large_body_aborts_multipart() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        c::check_failed_large_body_leaves_no_object(&s.store).await;
        assert_no_pending_uploads(&s).await;
    })
}

/// A multi-part write rejected because the key exists aborts the upload.
#[test]
fn s3_rejected_large_put_aborts_multipart() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        c::check_put_existing_large_keeps_original(&s.store).await;
        assert_no_pending_uploads(&s).await;
    })
}

// ---- §7.1 lifecycle rule ----------------------------------------------------------------

/// Credentials of the MinIO user without `s3:PutLifecycleConfiguration` (docker-compose.yml).
fn limited_creds() -> (String, String) {
    let var = |k: &str, d: &str| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| d.into())
    };
    (
        var("HOARD_TEST_S3_LIMITED_KEY", "hoard-limited"),
        var("HOARD_TEST_S3_LIMITED_SECRET", "hoard-limited-secret"),
    )
}

fn client(b: &MinioBucket) -> aws_sdk_s3::Client {
    r2_client(&b.endpoint, &b.key_id, &b.secret)
}

async fn rules(b: &MinioBucket) -> Vec<LifecycleRule> {
    match client(b)
        .get_bucket_lifecycle_configuration()
        .bucket(&b.bucket)
        .send()
        .await
    {
        Ok(out) => out.rules().to_vec(),
        Err(e) => {
            use aws_sdk_s3::error::ProvideErrorMetadata;
            assert_eq!(e.code(), Some("NoSuchLifecycleConfiguration"), "{e:?}");
            Vec::new()
        }
    }
}

fn our_rule<'a>(rules: &'a [LifecycleRule], prefix: &str) -> Option<&'a LifecycleRule> {
    rules
        .iter()
        .find(|r| r.id() == Some(rule_id(prefix).as_str()))
}

#[test]
fn rule_id_replaces_slashes() {
    assert_eq!(rule_id("cache/"), "hoard-cache-");
    assert_eq!(rule_id("team/cache/"), "hoard-team-cache-");
}

#[test]
fn fix_command_is_exact() {
    assert_eq!(
        fix_command("team-cache", "cache/", 7),
        "npx wrangler r2 bucket lifecycle add team-cache hoard-cache- cache/ --expire-days 7 --abort-multipart-days 1"
    );
}

/// Against MinIO itself: the rule is accepted (MinIO drops the abort clause, so the
/// round-trip checks below run through `common::proxied`).
#[test]
fn auto_creates_rule_on_minio() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let got = ensure_lifecycle(&client(&b), &b.bucket, "cache/", 7, LifecycleMode::Auto)
            .await
            .unwrap();
        assert_eq!(got, LifecycleStatus::Created);
        let r = rules(&b).await;
        let ours = our_rule(&r, "cache/").expect("rule created");
        assert_eq!(ours.status(), &ExpirationStatus::Enabled);
        assert_eq!(ours.filter().and_then(|f| f.prefix()), Some("cache/"));
        assert_eq!(ours.expiration().and_then(|e| e.days()), Some(7));
    })
}

#[test]
fn auto_creates_rule_then_is_idempotent() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let b = common::proxied(&b).await;
        let c = client(&b);
        let ensure = |days, mode| ensure_lifecycle(&c, &b.bucket, "cache/", days, mode);
        assert_eq!(
            ensure(7, LifecycleMode::Auto).await.unwrap(),
            LifecycleStatus::Created
        );
        let r = rules(&b).await;
        let ours = our_rule(&r, "cache/").expect("rule created");
        assert_eq!(ours.status(), &ExpirationStatus::Enabled);
        assert_eq!(ours.filter().and_then(|f| f.prefix()), Some("cache/"));
        assert_eq!(ours.expiration().and_then(|e| e.days()), Some(7));
        assert_eq!(
            ours.abort_incomplete_multipart_upload()
                .and_then(|a| a.days_after_initiation()),
            Some(1)
        );
        assert_eq!(
            ensure(7, LifecycleMode::Auto).await.unwrap(),
            LifecycleStatus::AlreadyOk
        );
        assert_eq!(
            ensure(7, LifecycleMode::Verify).await.unwrap(),
            LifecycleStatus::AlreadyOk
        );
        assert_eq!(
            ensure(14, LifecycleMode::Auto).await.unwrap(),
            LifecycleStatus::Updated
        );
        let r = rules(&b).await;
        assert_eq!(r.len(), 1, "replaced, not appended: {r:?}");
        assert_eq!(r[0].expiration().and_then(|e| e.days()), Some(14));
        assert_eq!(
            ensure(14, LifecycleMode::Verify).await.unwrap(),
            LifecycleStatus::AlreadyOk
        );
    })
}

/// Review Focus 3: other rules survive our Put field for field, on create and on update.
#[test]
fn keeps_foreign_rules() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let foreign = LifecycleRule::builder()
            .id("logs-expire")
            .status(ExpirationStatus::Enabled)
            .filter(LifecycleRuleFilter::builder().prefix("logs/").build())
            .expiration(LifecycleExpiration::builder().days(30).build())
            .build()
            .unwrap();
        let disabled = LifecycleRule::builder()
            .id("tmp-off")
            .status(ExpirationStatus::Disabled)
            .filter(LifecycleRuleFilter::builder().prefix("tmp/").build())
            .expiration(LifecycleExpiration::builder().days(2).build())
            .build()
            .unwrap();
        client(&b)
            .put_bucket_lifecycle_configuration()
            .bucket(&b.bucket)
            .lifecycle_configuration(
                BucketLifecycleConfiguration::builder()
                    .rules(foreign)
                    .rules(disabled)
                    .build()
                    .unwrap(),
            )
            .send()
            .await
            .unwrap();
        let before = rules(&b).await;
        assert_eq!(before.len(), 2);

        let c = client(&b);
        for (days, want) in [(7, LifecycleStatus::Created), (9, LifecycleStatus::Updated)] {
            let got = ensure_lifecycle(&c, &b.bucket, "cache/", days, LifecycleMode::Auto)
                .await
                .unwrap();
            assert_eq!(got, want);
            let after = rules(&b).await;
            assert_eq!(after.len(), 3, "{after:?}");
            for old in &before {
                let new = after.iter().find(|r| r.id() == old.id()).unwrap();
                assert_eq!(new, old, "foreign rule changed");
            }
            let ours = our_rule(&after, "cache/").unwrap();
            assert_eq!(ours.expiration().and_then(|e| e.days()), Some(days as i32));
        }
    })
}

/// The `<Rule>` element whose `<ID>` is `id` in a lifecycle configuration.
fn rule_xml<'a>(xml: &'a str, id: &str) -> &'a str {
    xml.split("<Rule>")
        .find(|r| r.contains(&format!("<ID>{id}</ID>")))
        .and_then(|r| r.split("</Rule>").next())
        .unwrap_or_else(|| panic!("no rule {id} in {xml}"))
}

/// Foreign rules keep every S3 lifecycle field through our Get, merge and Put (raw XML in,
/// raw XML out; MinIO is bypassed for the lifecycle calls).
#[test]
fn foreign_rules_survive_as_xml() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let (b, store) = common::proxied_with_store(&b).await;
        store.set(
            &b.bucket,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<LifecycleConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Rule>
    <ID>archive-web</ID>
    <Filter><And><Prefix>web/</Prefix><Tag><Key>team</Key><Value>web</Value></Tag><ObjectSizeGreaterThan>1024</ObjectSizeGreaterThan></And></Filter>
    <Status>Enabled</Status>
    <Transition><Days>30</Days><StorageClass>GLACIER</StorageClass></Transition>
    <NoncurrentVersionExpiration><NoncurrentDays>5</NoncurrentDays><NewerNoncurrentVersions>2</NewerNoncurrentVersions></NoncurrentVersionExpiration>
  </Rule>
  <Rule>
    <ID>markers</ID>
    <Filter><Prefix>old/</Prefix></Filter>
    <Status>Disabled</Status>
    <Expiration><ExpiredObjectDeleteMarker>true</ExpiredObjectDeleteMarker></Expiration>
  </Rule>
</LifecycleConfiguration>"#,
        );
        let got = ensure_lifecycle(&client(&b), &b.bucket, "cache/", 7, LifecycleMode::Auto)
            .await
            .unwrap();
        assert_eq!(got, LifecycleStatus::Created);
        let xml = store.get(&b.bucket).unwrap();
        let web = rule_xml(&xml, "archive-web");
        for want in [
            "<Prefix>web/</Prefix>",
            "<Key>team</Key>",
            "<Value>web</Value>",
            "<ObjectSizeGreaterThan>1024</ObjectSizeGreaterThan>",
            "<Status>Enabled</Status>",
            "<Days>30</Days>",
            "<StorageClass>GLACIER</StorageClass>",
            "<NoncurrentDays>5</NoncurrentDays>",
            "<NewerNoncurrentVersions>2</NewerNoncurrentVersions>",
        ] {
            assert!(web.contains(want), "{want} lost from {web}");
        }
        assert!(web.contains("<And>") && web.contains("<Transition>"), "{web}");
        assert!(!web.contains("<Expiration>"), "{web}");
        let markers = rule_xml(&xml, "markers");
        for want in [
            "<Prefix>old/</Prefix>",
            "<Status>Disabled</Status>",
            "<ExpiredObjectDeleteMarker>true</ExpiredObjectDeleteMarker>",
        ] {
            assert!(markers.contains(want), "{want} lost from {markers}");
        }
        let ours = rule_xml(&xml, "hoard-cache-");
        assert!(ours.contains("<DaysAfterInitiation>1</DaysAfterInitiation>"), "{ours}");
    })
}

#[test]
fn verify_mode_reports_missing_with_fix() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let c = client(&b);
        let err = ensure_lifecycle(&c, &b.bucket, "cache/", 7, LifecycleMode::Verify)
            .await
            .unwrap_err();
        assert!(matches!(err, LifecycleError::Missing { .. }), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("missing"), "{msg}");
        let fix = fix_command(&b.bucket, "cache/", 7);
        assert!(msg.lines().any(|l| l == fix), "{msg}");
        assert!(rules(&b).await.is_empty(), "verify must not write");

        // A rule with our id but other values is reported as different.
        ensure_lifecycle(&c, &b.bucket, "cache/", 3, LifecycleMode::Auto)
            .await
            .unwrap();
        let msg = ensure_lifecycle(&c, &b.bucket, "cache/", 7, LifecycleMode::Verify)
            .await
            .unwrap_err()
            .to_string();
        assert!(msg.contains("differs"), "{msg}");
        assert!(msg.lines().any(|l| l == fix), "{msg}");
    })
}

#[test]
fn manual_mode_skips() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let got = ensure_lifecycle(&client(&b), &b.bucket, "cache/", 7, LifecycleMode::Manual)
            .await
            .unwrap();
        assert_eq!(got, LifecycleStatus::Skipped);
        assert!(rules(&b).await.is_empty());
    })
}

/// An unreachable endpoint is a transient error, not a setup error.
#[test]
fn unreachable_endpoint_is_other() {
    rt().block_on(async {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let c = r2_client(&format!("http://127.0.0.1:{port}"), "k", "s");
        let err = ensure_lifecycle(&c, "b", "cache/", 7, LifecycleMode::Auto)
            .await
            .unwrap_err();
        assert!(matches!(err, LifecycleError::Other { .. }), "{err:?}");
        let fix = fix_command("b", "cache/", 7);
        assert!(err.to_string().lines().any(|l| l == fix), "{err}");
    })
}

/// Arguments for `serve` / `doctor` on `b`; credentials go through the environment.
fn r2_cmd(cmd: &str, b: &MinioBucket, dir: &std::path::Path, key: &str, secret: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hoard"));
    c.arg(cmd)
        .args(["--storage", "r2", "--r2-endpoint", &b.endpoint])
        .args(["--r2-bucket", &b.bucket])
        .arg("--port-file")
        .arg(dir.join("port"))
        .arg("--token-file")
        .arg(dir.join("token"))
        .arg("--summary-file")
        .arg(dir.join("summary.md"))
        .env_remove("HOARD_R2_LIFECYCLE")
        .env("HOARD_R2_ACCESS_KEY_ID", key)
        .env("HOARD_R2_SECRET_ACCESS_KEY", secret)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// Waits up to 30 s for the child to exit.
fn wait_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    for _ in 0..600 {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    panic!("process did not exit");
}

fn output(child: std::process::Child) -> (String, String) {
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Review Focus 4: a key that may not set lifecycle rules gets AccessDenied with the fix, and
/// `serve` exits non-zero with the fix on stderr (and no secret).
#[test]
fn access_denied_prints_fix() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let (key, secret) = limited_creds();
        let limited = r2_client(&b.endpoint, &key, &secret);
        let err = ensure_lifecycle(&limited, &b.bucket, "cache/", 7, LifecycleMode::Auto)
            .await
            .unwrap_err();
        let fix = fix_command(&b.bucket, "cache/", 7);
        match &err {
            LifecycleError::AccessDenied { fix: f } => assert_eq!(f, &fix),
            e => panic!("want AccessDenied, got {e:?}"),
        }
        assert!(err.to_string().lines().any(|l| l == fix), "{err}");

        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), &key, &secret)
            .spawn()
            .unwrap();
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(!status.success(), "{stderr}");
        assert!(
            stderr.contains("npx wrangler r2 bucket lifecycle add"),
            "{stderr}"
        );
        assert!(stderr.lines().any(|l| l == fix), "{stderr}");
        assert!(!stderr.contains(&secret), "secret leaked: {stderr}");
        assert!(!dir.path().join("port").exists());
    })
}

async fn shutdown(dir: &std::path::Path) {
    let port = common::wait_for_file(&dir.join("port")).await;
    let token = std::fs::read_to_string(dir.join("token")).unwrap();
    let res = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/_hoard/shutdown", port.trim()))
        .bearer_auth(token.trim())
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());
}

/// `serve` with root credentials starts, creates the rule and reports it in the summary.
#[test]
fn serve_creates_rule_on_startup() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), &b.key_id, &b.secret)
            .spawn()
            .unwrap();
        common::wait_for_file(&dir.path().join("port")).await;
        let r = rules(&b).await;
        let ours = our_rule(&r, "cache/").expect("serve created the rule");
        assert_eq!(ours.expiration().and_then(|e| e.days()), Some(7));
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(summary.contains("Lifecycle: 7 days ✓"), "{summary}");
    })
}

/// A transient error (R2 unreachable) does not stop `serve`; the summary says so.
#[test]
fn serve_continues_when_lifecycle_unreachable() {
    rt().block_on(async {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let b = MinioBucket {
            endpoint: format!("http://127.0.0.1:{port}"),
            bucket: "nowhere".into(),
            key_id: "k".into(),
            secret: "s".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), "k", "s").spawn().unwrap();
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(summary.contains("Lifecycle: not checked ("), "{summary}");
    })
}

/// A lifecycle endpoint that accepts connections and never answers: `serve` is ready (port
/// file written) well inside the action's 10 s health budget, and the summary says why the
/// rule was not checked.
#[test]
fn serve_ready_quickly_when_lifecycle_hangs() {
    rt().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        let b = MinioBucket {
            endpoint: format!("http://{addr}"),
            bucket: "nowhere".into(),
            key_id: "k".into(),
            secret: "s".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let mut child = r2_cmd("serve", &b, dir.path(), "k", "s").spawn().unwrap();
        common::wait_for_file(&dir.path().join("port")).await;
        let ready = start.elapsed();
        assert!(
            ready < std::time::Duration::from_secs(8),
            "ready after {ready:?}"
        );
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(
            summary.contains("Lifecycle: not checked (timed out)"),
            "{summary}"
        );
    })
}

/// `serve --r2-lifecycle manual` touches no rule and says so.
#[test]
fn serve_manual_mode_warns() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), &b.key_id, &b.secret)
            .args(["--r2-lifecycle", "manual"])
            .spawn()
            .unwrap();
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        assert!(
            stderr.contains("r2-lifecycle=manual: the bucket can grow until the size cap runs"),
            "{stderr}"
        );
        assert!(rules(&b).await.is_empty());
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(summary.contains("Lifecycle: manual"), "{summary}");
    })
}

/// `doctor` fails with the fix while the rule is missing and passes once it exists; the
/// round-trip object is gone afterwards.
#[test]
fn doctor_checks_rule_and_bucket() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let b = common::proxied(&b).await;
        let dir = tempfile::tempdir().unwrap();
        let run = || {
            let child = r2_cmd("doctor", &b, dir.path(), &b.key_id, &b.secret)
                .spawn()
                .unwrap();
            let out = child.wait_with_output().unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };
        let (ok, stdout, stderr) = run();
        assert!(!ok, "{stdout}{stderr}");
        assert!(stdout.contains("✓ bucket read/write"), "{stdout}");
        assert!(stdout.contains("✗ lifecycle rule hoard-cache-"), "{stdout}");
        assert!(
            stdout
                .lines()
                .any(|l| l == fix_command(&b.bucket, "cache/", 7)),
            "{stdout}"
        );
        assert!(rules(&b).await.is_empty(), "doctor must not write the rule");

        ensure_lifecycle(&client(&b), &b.bucket, "cache/", 7, LifecycleMode::Auto)
            .await
            .unwrap();
        let (ok, stdout, stderr) = run();
        assert!(ok, "{stdout}{stderr}");
        assert!(
            stdout.contains("✓ lifecycle rule hoard-cache- (7 days)"),
            "{stdout}"
        );
        assert!(stdout.contains("✓ bucket read/write"), "{stdout}");
        let left = client(&b)
            .list_objects_v2()
            .bucket(&b.bucket)
            .send()
            .await
            .unwrap();
        assert!(left.contents().is_empty(), "{:?}", left.contents());
    })
}

/// `doctor` with a key that cannot write objects reports the failed round-trip.
#[test]
fn doctor_reports_bad_credentials() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let out = r2_cmd("doctor", &b, dir.path(), "nobody", "wrong-secret-123")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success());
        assert!(stdout.contains("✗ bucket read/write"), "{stdout}");
        assert!(!stdout.contains("wrong-secret-123") && !stderr.contains("wrong-secret-123"));
    })
}

// ---- §7.2 size cap ----------------------------------------------------------------------

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap()
}

/// `n` raw objects of `size` bytes, written in key order (S3 times may tie at one second;
/// the key tie-break then matches the write order).
async fn fill(s: &Store, prefix: &str, n: usize, size: usize) {
    for i in 0..n {
        s.op.write(&format!("{prefix}obj{i:03}"), vec![b'x'; size])
            .await
            .unwrap();
    }
}

async fn keys(s: &Store, prefix: &str) -> Vec<String> {
    let mut k: Vec<String> = s
        .list(prefix)
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.key)
        .collect();
    k.sort();
    k
}

#[test]
fn s3_prune_throttled_within_24h() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        let s = &s.store;
        fill(s, "cache/", 5, 100).await;
        let r1 = prune(s, "cache/", 250, t0(), false).await.unwrap();
        assert_eq!((r1.deleted, r1.remaining_bytes), (3, 200));
        assert_eq!(
            keys(s, "cache/obj").await,
            vec!["cache/obj003", "cache/obj004"]
        );
        let hours = chrono::Duration::hours;
        let r2 = prune(s, "cache/", 0, t0() + hours(23), false)
            .await
            .unwrap();
        assert!(r2.skipped_throttled);
        let r3 = prune(s, "cache/", 0, t0() + hours(23), true).await.unwrap();
        assert_eq!(r3.remaining_bytes, 0);
        assert!(s.op.exists(&marker_key("cache/")).await.unwrap());
    })
}

#[test]
fn s3_prune_keeps_hoard_objects() {
    rt().block_on(async {
        let Some(s) = s3_store().await else { return };
        let s = &s.store;
        fill(s, "cache/", 2, 100).await;
        s.op.write("cache/_hoard/doctor", vec![b'd'; 1000])
            .await
            .unwrap();
        let r = prune(s, "cache/", 0, t0(), false).await.unwrap();
        assert_eq!((r.listed, r.deleted, r.remaining_bytes), (2, 2, 0));
        assert!(s.op.exists("cache/_hoard/doctor").await.unwrap());
    })
}

/// A store on `b` (for seeding and inspecting what `serve` sees).
fn bucket_store(b: &MinioBucket) -> Store {
    let dir = tempfile::tempdir().unwrap();
    Store::new(build_operator(&r2_config(dir.path(), b)).unwrap())
}

/// `serve` with a tiny cap deletes the oldest objects on shutdown and reports it.
#[test]
fn serve_prunes_on_shutdown() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let s = bucket_store(&b);
        fill(&s, "cache/", 5, 100).await;
        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), &b.key_id, &b.secret)
            .args(["--max-size", "250"])
            .spawn()
            .unwrap();
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(summary.contains("Pruned: 3 objects, 300 B"), "{summary}");
        assert_eq!(
            keys(&s, "cache/obj").await,
            vec!["cache/obj003", "cache/obj004"]
        );
        assert!(s.op.exists(&marker_key("cache/")).await.unwrap());
    })
}

/// `mode=read` never prunes.
#[test]
fn serve_read_mode_does_not_prune() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let s = bucket_store(&b);
        fill(&s, "cache/", 2, 100).await;
        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), &b.key_id, &b.secret)
            .args(["--max-size", "0", "--mode", "read"])
            .spawn()
            .unwrap();
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(!summary.contains("Prune"), "{summary}");
        assert_eq!(keys(&s, "cache/obj").await.len(), 2);
        assert!(!s.op.exists(&marker_key("cache/")).await.unwrap());
    })
}

/// A failing prune (here: the bucket is gone) is a summary line, never a failed shutdown.
#[test]
fn serve_prune_failure_is_reported_not_fatal() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), &b.key_id, &b.secret)
            .args(["--max-size", "0"])
            .spawn()
            .unwrap();
        common::wait_for_file(&dir.path().join("port")).await;
        common::s3_client(&b.endpoint, &b.key_id, &b.secret)
            .delete_bucket()
            .bucket(&b.bucket)
            .send()
            .await
            .unwrap();
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(summary.contains("Prune: failed ("), "{summary}");
    })
}

/// Credentials of the MinIO user without `s3:DeleteObject` (tests/minio/entrypoint.sh).
fn nodelete_creds() -> (String, String) {
    let var = |k: &str, d: &str| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| d.into())
    };
    (
        var("HOARD_TEST_S3_NODELETE_KEY", "hoard-nodelete"),
        var("HOARD_TEST_S3_NODELETE_SECRET", "hoard-nodelete-secret"),
    )
}

/// `b` seen through the no-delete user.
fn nodelete_bucket(b: &MinioBucket) -> MinioBucket {
    let (key_id, secret) = nodelete_creds();
    MinioBucket {
        key_id,
        secret,
        ..b.clone()
    }
}

/// Each failed delete is counted, the others are still tried, the bytes stay counted, and
/// the marker is not written (the next run retries).
#[test]
fn s3_prune_counts_failed_deletes() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        fill(&bucket_store(&b), "cache/", 5, 100).await;
        let s = bucket_store(&nodelete_bucket(&b));
        let r = prune(&s, "cache/", 250, t0(), false).await.unwrap();
        assert_eq!((r.listed, r.deleted, r.delete_errors), (5, 0, 3), "{r:?}");
        assert_eq!((r.freed_bytes, r.remaining_bytes), (0, 500));
        assert!(r.last_error.is_some());
        assert_eq!(keys(&s, "cache/obj").await.len(), 5);
        assert!(!s.op.exists(&marker_key("cache/")).await.unwrap());
    })
}

/// On shutdown, failed deletes are a summary line; `serve` still exits 0.
#[test]
fn serve_reports_failed_deletes() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        fill(&bucket_store(&b), "cache/", 5, 100).await;
        let (key, secret) = nodelete_creds();
        let dir = tempfile::tempdir().unwrap();
        let mut child = r2_cmd("serve", &b, dir.path(), &key, &secret)
            .args(["--max-size", "250", "--r2-lifecycle", "manual"])
            .spawn()
            .unwrap();
        shutdown(dir.path()).await;
        let status = wait_exit(&mut child);
        let (_, stderr) = output(child);
        assert!(status.success(), "{stderr}");
        let summary = std::fs::read_to_string(dir.path().join("summary.md")).unwrap();
        assert!(
            summary.contains("Prune: failed (3 deletes failed)"),
            "{summary}"
        );
        assert!(!summary.contains("Pruned:"), "{summary}");
        assert!(!stderr.contains(&secret), "secret leaked: {stderr}");
    })
}

/// `hoard prune` prints its line, then exits non-zero when deletes failed.
#[test]
fn prune_command_fails_on_failed_deletes() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        fill(&bucket_store(&b), "cache/", 5, 100).await;
        let (key, secret) = nodelete_creds();
        let dir = tempfile::tempdir().unwrap();
        let child = r2_cmd("prune", &b, dir.path(), &key, &secret)
            .args(["--max-size", "250"])
            .spawn()
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{stdout}{stderr}");
        assert_eq!(
            stdout,
            "pruned 0 of 5 objects, freed 0 B, 500 B remain (max 250 B)\n"
        );
        assert!(stderr.contains("3 delete(s) failed"), "{stderr}");
        assert!(!stderr.contains(&secret), "secret leaked: {stderr}");
    })
}
