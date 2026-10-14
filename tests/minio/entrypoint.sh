#!/bin/bash
# MinIO for the S3/R2 tests: runs the server and creates the limited user `hoard-limited`,
# which may read and write objects and read lifecycle rules but not set them (tests of the
# AccessDenied path). Restart-safe: every step overwrites what a previous start created.
set -euo pipefail

ready=/tmp/hoard-minio-ready
rm -f "$ready"

minio server /data &
server=$!
# PID 1 forwards the stop signal and exits only after the server has.
trap 'kill -TERM "$server"; wait "$server"; exit $?' TERM INT

until curl -sf http://127.0.0.1:9000/minio/health/live >/dev/null; do
	kill -0 "$server"
	sleep 0.5
done

mc alias set local http://127.0.0.1:9000 "$MINIO_ROOT_USER" "$MINIO_ROOT_PASSWORD" >/dev/null
cat >/tmp/hoard-limited.json <<'JSON'
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["s3:ListBucket", "s3:GetLifecycleConfiguration"],
      "Resource": ["arn:aws:s3:::*"]
    },
    {
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
      "Resource": ["arn:aws:s3:::*/*"]
    }
  ]
}
JSON
mc admin policy create local hoard-limited /tmp/hoard-limited.json >/dev/null
mc admin user add local hoard-limited hoard-limited-secret >/dev/null
mc admin policy attach local hoard-limited --user hoard-limited >/dev/null 2>&1 ||
	mc admin user info local hoard-limited | grep -q 'PolicyName: *hoard-limited'
touch "$ready"

wait "$server"
