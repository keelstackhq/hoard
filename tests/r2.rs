//! Store checks on the R2 backend (S3 API), run against MinIO when `HOARD_TEST_S3_ENDPOINT`
//! is set. The store is built through `build_operator`, the same path the server uses.

mod common;

use aws_sdk_s3::types::{
    BucketLifecycleConfiguration, ExpirationStatus, LifecycleExpiration, LifecycleRule,
    LifecycleRuleFilter,
};
use chrono::{DateTime, TimeZone, Utc};
use common::{
    ChildGuard, MinioBucket, SpawnGuarded, minio_bucket, r2_config, s3_runtime as rt,
    store_checks as c,
};
use hoard::config::LifecycleMode;
use hoard::r2::lifecycle::{
    LifecycleError, LifecycleStatus, ensure_lifecycle, fix_command, r2_client, rule_id,
};
use hoard::r2::prune::{marker_key, prune};
use hoard::r2::refresh::{Refresher, refresh_object};
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

fn output(child: ChildGuard) -> (String, String) {
    let out = child.wait_with_output();
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
        let mut child = r2_cmd("serve", &b, dir.path(), &key, &secret).spawn_guarded();
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
        let mut child = r2_cmd("serve", &b, dir.path(), &b.key_id, &b.secret).spawn_guarded();
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
        let mut child = r2_cmd("serve", &b, dir.path(), "k", "s").spawn_guarded();
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
        let mut child = r2_cmd("serve", &b, dir.path(), "k", "s").spawn_guarded();
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
            .spawn_guarded();
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
            let child = r2_cmd("doctor", &b, dir.path(), &b.key_id, &b.secret).spawn_guarded();
            let out = child.wait_with_output();
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

/// A GET hit costs exactly one storage request (one streaming read; header and body come from
/// the same response), for an artifact smaller than the header read and for a larger one.
/// HEAD stays a single bounded read too.
#[test]
fn get_hit_is_one_storage_read() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let (pb, _, log) = common::proxied_with_log(&b).await;
        let dir = tempfile::tempdir().unwrap();
        let t =
            common::spawn_test_server_with(bucket_store(&pb), hoard::config::Mode::ReadWrite, dir)
                .await;
        let http = reqwest::Client::new();
        for (hash, size) in [("aa", 100usize), ("bb", 10_000)] {
            let body: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let url = format!("{}/v8/artifacts/{hash}", t.base_url);
            let put = http
                .put(&url)
                .bearer_auth(&t.token)
                .body(body.clone())
                .send()
                .await
                .unwrap();
            assert_eq!(put.status(), 200);
            assert_eq!(t.stats.snapshot().put_errors, 0);
            log.take();

            let got = http.get(&url).bearer_auth(&t.token).send().await.unwrap();
            assert_eq!(got.status(), 200);
            assert_eq!(got.bytes().await.unwrap().as_ref(), body.as_slice());
            let key = format!("/{}/cache/turbo/v1/{hash}", b.bucket);
            assert_eq!(log.take(), vec![format!("GET {key}")], "{size} B artifact");

            let head = http.head(&url).bearer_auth(&t.token).send().await.unwrap();
            assert_eq!(head.status(), 200);
            assert_eq!(log.take(), vec![format!("GET {key}")], "{size} B HEAD");
        }
    })
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
            .spawn_guarded();
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
            .spawn_guarded();
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

/// `mode=read` never writes the lifecycle rule: `auto` only verifies, and a missing rule or
/// AccessDenied is a warning, never a failed start.
#[test]
fn serve_read_mode_only_verifies_lifecycle() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let raw = b.clone();
        let b = common::proxied(&b).await;
        let run = |b: MinioBucket| async move {
            let dir = tempfile::tempdir().unwrap();
            let mut child = r2_cmd("serve", &b, dir.path(), &b.key_id, &b.secret)
                .args(["--mode", "read"])
                .spawn_guarded();
            shutdown(dir.path()).await;
            let status = wait_exit(&mut child);
            let (_, stderr) = output(child);
            assert!(status.success(), "{stderr}");
            std::fs::read_to_string(dir.path().join("summary.md")).unwrap()
        };
        // Rule missing: not created, and the summary says why it was not checked.
        let summary = run(b.clone()).await;
        assert!(
            summary.contains("Lifecycle: not checked (read-only)"),
            "{summary}"
        );
        assert!(
            rules(&b).await.is_empty(),
            "read mode must not write the rule"
        );
        // A key without lifecycle access (AccessDenied, straight to MinIO) still starts.
        let (key_id, secret) = limited_creds();
        let summary = run(MinioBucket {
            key_id,
            secret,
            ..raw
        })
        .await;
        assert!(
            summary.contains("Lifecycle: not checked (read-only)"),
            "{summary}"
        );
        // Rule present: verified.
        ensure_lifecycle(&client(&b), &b.bucket, "cache/", 7, LifecycleMode::Auto)
            .await
            .unwrap();
        let summary = run(b.clone()).await;
        assert!(summary.contains("Lifecycle: 7 days ✓"), "{summary}");
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
            .spawn_guarded();
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
            .spawn_guarded();
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
            .spawn_guarded();
        let out = child.wait_with_output();
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

// ---- refresh-on-hit (spec §7.3) ----

fn hrd1(body: &[u8]) -> Vec<u8> {
    let h = hoard::envelope::Header {
        v: 1,
        duration_ms: Some(5),
        tag: None,
        size: body.len() as u64,
    };
    let mut out = hoard::envelope::encode_header(&h);
    out.extend_from_slice(body);
    out
}

async fn last_modified(c: &aws_sdk_s3::Client, bucket: &str, key: &str) -> std::time::SystemTime {
    let head = c
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .unwrap();
    std::time::UNIX_EPOCH
        + std::time::Duration::from_millis(head.last_modified().unwrap().to_millis().unwrap() as u64)
}

/// A self-copy resets the object's age and leaves the body (a full envelope) untouched.
#[test]
fn refresh_object_resets_age_and_keeps_body() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let c = client(&b);
        let key = "cache/turbo/v1/aa bb+c";
        let blob = hrd1(b"artifact body");
        c.put_object()
            .bucket(&b.bucket)
            .key(key)
            .body(blob.clone().into())
            .send()
            .await
            .unwrap();
        let before = last_modified(&c, &b.bucket, key).await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        refresh_object(&c, &b.bucket, key).await.unwrap();
        let after = last_modified(&c, &b.bucket, key).await;
        assert!(after > before, "{after:?} should be newer than {before:?}");
        let got = c
            .get_object()
            .bucket(&b.bucket)
            .key(key)
            .send()
            .await
            .unwrap();
        assert_eq!(got.body.collect().await.unwrap().to_vec(), blob);
        // And through the store: the envelope still decodes.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(build_operator(&r2_config(dir.path(), &b)).unwrap());
        match store.get(key).await.unwrap() {
            hoard::store::GetOutcome::Hit(h, _) => assert_eq!(h.size, 13),
            _ => panic!("expected a hit after the refresh"),
        }
    })
}

#[test]
fn refresh_object_missing_key_is_an_error() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let err = refresh_object(&client(&b), &b.bucket, "cache/turbo/v1/nope")
            .await
            .unwrap_err();
        assert!(!format!("{err:#}").is_empty());
    })
}

struct RefreshFixture {
    t: common::TestServer,
    log: common::lifecycle_proxy::RequestLog,
    b: MinioBucket,
    http: reqwest::Client,
}

impl RefreshFixture {
    /// A server over the proxied bucket whose refresher treats objects older than `threshold`
    /// as old; `None` = refresh off.
    async fn new(b: &MinioBucket, threshold: Option<std::time::Duration>) -> Self {
        let (pb, _, log) = common::proxied_with_log(b).await;
        let stats = std::sync::Arc::new(hoard::stats::Stats::default());
        let refresher = threshold.map(|th| {
            std::sync::Arc::new(Refresher::new(
                client(&pb),
                pb.bucket.clone(),
                th,
                stats.clone(),
            ))
        });
        let t = common::spawn_refresh_server(
            bucket_store(&pb),
            refresher,
            stats,
            tempfile::tempdir().unwrap(),
        )
        .await;
        Self {
            t,
            log,
            b: b.clone(),
            http: reqwest::Client::new(),
        }
    }

    async fn put(&self, hash: &str) {
        let url = format!("{}/v8/artifacts/{hash}", self.t.base_url);
        let r = self
            .http
            .put(&url)
            .bearer_auth(&self.t.token)
            .body("data")
            .send()
            .await;
        assert_eq!(r.unwrap().status(), 200);
        self.log.take();
    }

    async fn get(&self, hash: &str) -> reqwest::StatusCode {
        let url = format!("{}/v8/artifacts/{hash}", self.t.base_url);
        let r = self
            .http
            .get(&url)
            .bearer_auth(&self.t.token)
            .send()
            .await
            .unwrap();
        let status = r.status();
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"data");
        status
    }

    /// Requests seen since the last call: polls until `done` accepts them (10 s limit), then
    /// allows a short grace period so unexpected extra requests still show up.
    async fn settled_log(&self, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            seen.extend(self.log.take());
            if done(&seen) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for requests, saw {seen:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        seen.extend(self.log.take());
        seen
    }

    fn key(&self, hash: &str) -> String {
        format!("/{}/cache/turbo/v1/{hash}", self.b.bucket)
    }
}

/// Three GET hits on an old object refresh it exactly once (one HEAD, one copy).
#[test]
fn get_hit_refreshes_old_object_once_per_run() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let f = RefreshFixture::new(&b, Some(std::time::Duration::ZERO)).await;
        f.put("aa").await;
        for _ in 0..3 {
            assert_eq!(f.get("aa").await, 200);
        }
        let key = f.key("aa");
        let log = f.settled_log(|l| l.contains(&format!("PUT {key}"))).await;
        let count = |m: &str| log.iter().filter(|l| **l == format!("{m} {key}")).count();
        assert_eq!(count("HEAD"), 1, "{log:?}");
        assert_eq!(count("PUT"), 1, "{log:?}");
        assert_eq!(f.t.stats.snapshot().refreshed, 1);
        // A later hit in the same run does not refresh again.
        assert_eq!(f.get("aa").await, 200);
        let log = f.settled_log(|l| l.contains(&format!("GET {key}"))).await;
        assert!(log.iter().all(|l| !l.starts_with("PUT")), "{log:?}");
        assert_eq!(f.t.stats.snapshot().refreshed, 1);
    })
}

/// An object younger than the threshold is checked once and not copied.
#[test]
fn get_hit_leaves_young_object_alone() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let f = RefreshFixture::new(&b, Some(std::time::Duration::from_secs(3600))).await;
        f.put("aa").await;
        for _ in 0..3 {
            assert_eq!(f.get("aa").await, 200);
        }
        let key = f.key("aa");
        let log = f.settled_log(|l| l.contains(&format!("HEAD {key}"))).await;
        assert_eq!(
            log.iter().filter(|l| **l == format!("HEAD {key}")).count(),
            1,
            "{log:?}"
        );
        assert!(log.iter().all(|l| !l.starts_with("PUT")), "{log:?}");
        assert_eq!(f.t.stats.snapshot().refreshed, 0);
    })
}

/// With refresh off a hit costs no extra request.
#[test]
fn get_hit_without_refresh_makes_no_copy() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let f = RefreshFixture::new(&b, None).await;
        f.put("aa").await;
        assert_eq!(f.get("aa").await, 200);
        let key = f.key("aa");
        let want = format!("GET {key}");
        assert_eq!(f.settled_log(|l| l.contains(&want)).await, vec![want]);
        assert_eq!(f.t.stats.snapshot().refreshed, 0);
    })
}

/// The GET answers while the refresh is stuck on an endpoint that never replies, and a
/// failing refresh changes neither the response nor the counters.
#[test]
fn get_hit_does_not_wait_for_the_refresh() {
    rt().block_on(async {
        let Some(b) = minio_bucket().await else {
            return;
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hole = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        let stats = std::sync::Arc::new(hoard::stats::Stats::default());
        let refresher = std::sync::Arc::new(Refresher::new(
            r2_client(&hole, &b.key_id, &b.secret),
            b.bucket.clone(),
            std::time::Duration::ZERO,
            stats.clone(),
        ));
        let t = common::spawn_refresh_server(
            bucket_store(&b),
            Some(refresher),
            stats,
            tempfile::tempdir().unwrap(),
        )
        .await;
        let http = reqwest::Client::new();
        let url = format!("{}/v8/artifacts/aa", t.base_url);
        let put = http
            .put(&url)
            .bearer_auth(&t.token)
            .body("data")
            .send()
            .await
            .unwrap();
        assert_eq!(put.status(), 200);
        let start = std::time::Instant::now();
        let got = http.get(&url).bearer_auth(&t.token).send().await.unwrap();
        assert_eq!(got.status(), 200);
        assert_eq!(got.bytes().await.unwrap().as_ref(), b"data");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(t.stats.snapshot().refreshed, 0);
    })
}

/// By hand against a real R2 bucket (spec §7.3): does a self-copy reset the object's age?
/// `HOARD_TEST_R2_ENDPOINT=https://<account>.r2.cloudflarestorage.com HOARD_TEST_R2_KEY=...
/// HOARD_TEST_R2_SECRET=... HOARD_TEST_R2_BUCKET=... cargo test --test r2 -- --ignored refresh_real_r2`
#[test]
#[ignore = "needs a real R2 bucket"]
fn refresh_real_r2() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let (endpoint, key_id, secret, bucket) = (
        var("HOARD_TEST_R2_ENDPOINT"),
        var("HOARD_TEST_R2_KEY"),
        var("HOARD_TEST_R2_SECRET"),
        var("HOARD_TEST_R2_BUCKET"),
    );
    rt().block_on(async {
        let c = r2_client(&endpoint, &key_id, &secret);
        let key = format!("hoard-refresh-check/{}", rand::random::<u32>());
        let blob = hrd1(b"artifact body");
        c.put_object()
            .bucket(&bucket)
            .key(&key)
            .body(blob.clone().into())
            .send()
            .await
            .unwrap();
        let before = last_modified(&c, &bucket, &key).await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let copied = refresh_object(&c, &bucket, &key).await;
        let after = last_modified(&c, &bucket, &key).await;
        let got = c
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .unwrap();
        let body = got.body.collect().await.unwrap().to_vec();
        let _ = c.delete_object().bucket(&bucket).key(&key).send().await;
        copied.unwrap();
        eprintln!("last_modified before={before:?} after={after:?}");
        assert!(
            after > before,
            "R2 self-copy did not reset last_modified: re-PUT instead (spec §7.3)"
        );
        assert_eq!(body, blob);
    })
}
