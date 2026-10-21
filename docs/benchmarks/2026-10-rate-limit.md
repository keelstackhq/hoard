# Rate-limit benchmark, October 2026

| packages | write-behind | cold wall (s) | drain (s) | puts | rate_limited | throttled | put_errors | warm wall (s) | hits |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | auto | 36.4 | 2 | 100 | 0 | 0 | 0 | 3.8 | 100 |
| 400 | auto | 103.5 | 2 | 378 | 22 | 1 | 0 | 6.0 | 378 |

Runs (2026-10-21, `ubuntu-latest`, main at ed8074a, one run at a time so they do not share the
repository's upload limit):

- 100 packages: https://github.com/keelstackhq/hoard/actions/runs/39659795765
- 400 packages: https://github.com/keelstackhq/hoard/actions/runs/39662254202

Decision: pending.
