---
name: hoard
description: Use when adding or fixing a CI cache for a Turborepo or Vite+ monorepo with the keelstackhq/hoard GitHub Action (or hoard on CircleCI/laptop), or when a hoard step warns, misses or fails.
---

# hoard

One step makes `turbo` (and `vp run`) caching work in CI. A localhost server speaks the Turborepo
remote-cache API and stores artifacts in the GitHub Actions cache, Cloudflare R2 or disk.

## Add it

Put the step **after** dependency install, **before** the turbo/vp command. No inputs needed:

```yaml
- uses: keelstackhq/hoard@v0   # floating major tag of the latest 0.x release; never @main
- run: pnpm turbo run build test lint
```

Order: checkout, setup-node, install, hoard, turbo. The step exports `TURBO_API`, `TURBO_TOKEN`
and `TURBO_TEAM` to every later step (no `eval "$(hoard env)"` in Actions). With turbo
`envMode: strict`, add those three to `globalPassThroughEnv`. Needs no `permissions:` block and
any turbo that reads those variables. Keep other turbo cache steps off; one `turbo` job per matrix
leg is fine.

Keep `pull_request` and also run the workflow on `push` to the default branch (`main` below): GitHub cache entries are branch-scoped, so pull
requests only start warm from what the default branch wrote. No `turbo.json` change; do not set
`"remoteCache": {"enabled": false}`.

## Pick storage (`storage:`)

- `github` (default): zero config. 10 GB/repo, about 200 uploads/min (packing keeps it under).
- `r2`: big monorepos, shared by all branches/laptops. Needs `r2-account-id`, `r2-access-key-id`,
  `r2-secret-access-key`, `r2-bucket` from secrets (setup steps: README "Use Cloudflare R2").
  Missing inputs (fork PRs get no secrets) fall back to `github` with a warning. Example:
  `with: {storage: r2, r2-account-id: "${{ secrets.R2_ACCOUNT_ID }}", ...}`.
- `fs`: self-hosted runner with a persistent disk, or tests.

## Check it works

Job summary has the hoard table (Hits/Misses/Uploaded). First run all misses is normal; the second
run should say `>>> FULL TURBO`. A re-run of the same commit counts as a second run. Outputs `hits`,
`misses`, `time-saved-ms` exist only after the post step, so read them from a later job
(`needs.<job>.outputs`; give the hoard step an `id` and map it in `jobs.<job>.outputs`). All inputs and defaults: `action.yml` (README "Action inputs").

## Fix it

| Symptom | Do |
|---|---|
| All misses every run | `Upload errors: N` in summary: read the log warning. Else `storage: github` on a PR branch with no `main` push: add the `push` trigger. Check `outputs` in `turbo.json`. |
| `uploads failed` / `throttled` | Build still passed. Throttle: use `storage: r2`. |
| `could not be downloaded … continuing without the cache` | Runner cannot reach github.com release assets. Not fatal. |
| Setup error (step fails) | The message names the fix: bad input, or R2 lifecycle rule missing with `r2-lifecycle: verify`. Run `hoard doctor` with `HOARD_R2_*` set. |
| `storage: r2` but summary says `github` | R2 inputs empty: fork PR or secret name typo. |
| Cargo-only repo | hoard does not cache cargo; use `Swatinem/rust-cache@v2`. |

## Other CI, local

`scripts/install.sh` installs a verified release; `hoard serve --detach`, `eval "$(hoard env)"`,
run turbo, `hoard stop` (files in this repo, not in your project). CircleCI: `circleci/hoard.yml`, `docs/circleci.md`; pass `restore: false`
on the default branch (CircleCI parameter only; Actions has no such input).

## Rules

The cache never fails a build except setup errors. Never put R2 keys in `run:` args or logs; pass
them through `with:` from secrets.
