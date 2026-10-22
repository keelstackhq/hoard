# Rate-limit benchmark, October 2026

| packages | write-behind | cold wall (s) | drain (s) | puts | rate_limited | throttled | put_errors | warm wall (s) | hits |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | auto | 36.4 | 2 | 100 | 0 | 0 | 0 | 3.8 | 100 |
| 400 | auto | 103.5 | 2 | 378 | 22 | 1 | 0 | 6.0 | 378 |
| 1000 | auto | 411.9 | 2 | 1000 | 0 | 0 | 0 | 4.1 | 1000 |

Runs (2026-10-21, `ubuntu-latest`, main at ed8074a, one run at a time so they do not share the
repository's upload limit):

- 100 packages: https://github.com/keelstackhq/hoard/actions/runs/39659795765
- 400 packages: https://github.com/keelstackhq/hoard/actions/runs/39662254202
- 1000 packages: https://github.com/keelstackhq/hoard/actions/runs/39664353368

Decision: **packing needed** (see spec §12). The rule is `throttled > 0` at 400 packages or
fewer, and the 400-package run got a `429`.

Notes:

- At 400 packages the cold build ran at about 233 uploads/min (400 tasks in 103.5 s), above the
  200/min limit. After the one `429`, hoard paused uploads, and the 22 PUTs that came in during the
  pause were skipped (`rate_limited` 22, as spec §6.4 says). Those 22 tasks were misses on the warm
  run (378 of 400 hits).
- At 1000 packages the build was slower per task (1000 tasks in 411.9 s, about 146 uploads/min),
  so it stayed under the limit and got no `429`. The limit is about uploads per minute, not about
  the package count, so a faster runner or cheaper tasks can hit it at any size.

## With packing (spec §6.5), October 2026

| packages | write-behind | pack | cold wall (s) | drain (s) | puts | packs | rate_limited | throttled | put_errors | warm wall (s) | hits |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 400 | auto | auto | 83.7 | 2 | 400 | 1 | 0 | 0 | 0 | 1.3 | 400 |
| 1000 | auto | auto | 260.8 | 2 | 1000 | 1 | 0 | 0 | 0 | 3.0 | 1000 |

Runs (2026-10-22, `ubuntu-latest`, PR keelstackhq/hoard#9 at f1e1f41; `pack: auto` = on for the
GitHub cache; `drain` is the shutdown time, which uploads the packs and the index):

- 400 packages: https://github.com/keelstackhq/hoard/actions/runs/39755912769
- 1000 packages: https://github.com/keelstackhq/hoard/actions/runs/39756606113

Result: **goal met**. Each cold job made 2 uploads (1 pack + 1 index) instead of 400 or 1000, so
`throttled` and `rate_limited` are 0 and no artifact was lost; every warm task hit (400/400 and
1000/1000, before: 378/400). Warm builds got faster too (400: 6.0 → 1.3 s; 1000: 4.1 → 3.0 s),
since HEAD answers from the index and each hit is one Range read.
