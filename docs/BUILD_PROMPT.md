# Build prompt

Paste one of the two prompts below into Claude Code at the root of this repo. The superpowers
plugin must be installed.

## A. Self-paced loop (recommended)

```
/loop Build hoard from the plans in this repo, one task per iteration, until every plan is done.

Each iteration:
1. Read CLAUDE.md, docs/PROGRESS.md and the spec docs/superpowers/specs/2026-10-14-hoard-design.md
   (only the sections the next task touches).
2. Pick the next unchecked task in plan order: 01-core-r2, then 02-github-action, then
   03-vite-plus-throughput (docs/superpowers/plans/). Skip tasks listed under "Blocked" in
   docs/PROGRESS.md unless the blocker is resolved.
3. Execute exactly that task with superpowers:subagent-driven-development: a fresh implementer
   subagent following the task's steps test-first, then a fresh reviewer subagent checking it
   against the task, the spec and the rules in CLAUDE.md. Fix what the reviewer finds.
4. Verify with superpowers:verification-before-completion: run cargo fmt --check, cargo clippy
   --all-targets -- -D warnings, cargo test (MinIO up via docker compose), and for action/ changes
   pnpm --dir action lint/test/build. Paste the real output into your reasoning; never claim a
   pass you did not see.
5. Tick the task's checkboxes in the plan, add one line to docs/PROGRESS.md (date, task, commit,
   test summary), commit with the message from the plan, and push if a remote exists.
6. If a step needs something only a human can provide (GitHub org/repo settings, secrets, a real
   R2 bucket, running a workflow on GitHub, a release), do every part you can locally, write the
   exact remaining action under "Blocked" in docs/PROGRESS.md, and move on to the next task that
   does not depend on it. Never fake a result, skip or weaken a test, or mark a blocked task done.
7. When a milestone's tasks are all done, run superpowers:requesting-code-review on the whole
   milestone, fix the findings, and note "Milestone N reviewed" in docs/PROGRESS.md.

Stop the loop when all three plans are fully checked (or only blocked tasks remain) and every
milestone review is recorded. Then write a final "Status" section in docs/PROGRESS.md: what
works, how to try it (commands), and the list of blocked items for me.
```

## B. One long session (no /loop)

```
Build hoard by executing the three plans in docs/superpowers/plans/ in order (01, 02, 03) with
superpowers:subagent-driven-development. Follow CLAUDE.md and the spec. One commit per task,
progress tracked in docs/PROGRESS.md, every claim backed by real command output
(superpowers:verification-before-completion). Anything that needs a human (secrets, GitHub
settings, real R2, releases) goes under "Blocked" in docs/PROGRESS.md; continue with independent
tasks. Request a code review at the end of each milestone. Keep going until all tasks are done or
only blocked ones remain, then summarise status and blocked items.
```

## What you will need to provide (expected blockers)

- The GitHub org `keelstack` and repo `keelstack/hoard` (for the release and self-test workflows).
- Repository secrets `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET`
  (an R2 S3 token with Object Read & Write on the bucket). Changing lifecycle rules needs a broader
  permission (Cloudflare lists "Workers R2 Storage Write"); with a narrower token set
  `r2-lifecycle: verify` and add the rule once with the `wrangler` command hoard prints.
- One manual check against a real R2 bucket (milestone 3, refresh-on-hit).
- Triggering the release workflow for the first version (`v0.1.0`).
