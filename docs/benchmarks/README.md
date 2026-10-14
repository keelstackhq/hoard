# Benchmarks

## GitHub cache upload rate limits

Question (spec §6.4, §12): does a cold build with hundreds of packages hit GitHub's
200 uploads/min limit, so that artifacts must be packed (one pack + index per job)?

### Run it

1. Actions tab, workflow `bench`, **Run workflow**.
2. Set `packages` (run 100, 400 and 1000, one run each) and `write-behind` (`auto` is the
   default behaviour for the GitHub cache; `off` shows the synchronous case).
3. When the run ends, open its summary. The `report` job prints one table row.

What happens: the `cold` job generates the packages (`scripts/bench/generate-monorepo.mjs`:
independent packages, a ~1 KB artifact each), builds them with `--concurrency=32` against an
empty cache (`cache-version: bench-<run_id>`) and reads `/_hoard/stats`. The `warm` job rebuilds
the same repo on a fresh runner and records hits. The run uses the GitHub cache of this
repository, so it leaves up to N entries behind; they expire under GitHub's normal eviction.

Reading the numbers:

- `rate_limited` counts PUTs the server skipped during a 429 pause (synchronous uploads).
- With write-behind, a 429 pauses the queue and retries the upload; it is not counted in
  `rate_limited`. It shows up as a long `drain` (time the uploads kept running after the build)
  and, if the 120 s shutdown drain runs out, as `put_errors`. Judge write-behind runs by `drain`
  and `put_errors` as well as `rate_limited`.
- `hits` on the warm run should equal `packages`.

### Record the results

Copy the `report` row of each run into `docs/benchmarks/2026-10-rate-limit.md` (create it from
the template below) and add the run URL and date. Do not round or estimate.

```markdown
# Rate-limit benchmark, October 2026

| packages | write-behind | cold wall (s) | drain (s) | puts | rate_limited | put_errors | warm wall (s) | hits |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
|  |  |  |  |  |  |  |  |  |

Runs: <links>
Decision: packing needed / not needed (see spec §12).
```

### Local check of the generator

```bash
node --test "scripts/bench/*.test.mjs"
node scripts/bench/generate-monorepo.mjs /tmp/bench 50
```
