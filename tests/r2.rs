//! Store checks on the R2 backend (S3 API), run against MinIO when `HOARD_TEST_S3_ENDPOINT`
//! is set. The store is built through `build_operator`, the same path the server uses.

mod common;

use common::{MinioBucket, minio_bucket, r2_config, s3_runtime as rt, store_checks as c};
use hoard::store::{Store, build_operator};

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

use aws_sdk_s3::types::{
    BucketLifecycleConfiguration, ExpirationStatus, LifecycleExpiration, LifecycleRule,
    LifecycleRuleFilter,
};
use hoard::config::LifecycleMode;
use hoard::r2::lifecycle::{
    LifecycleError, LifecycleStatus, ensure_lifecycle, fix_command, r2_client, rule_id,
};

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

use std::process::{Command, Stdio};

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
