# hoard

One-line CI cache for TypeScript monorepos (Turborepo and Vite+), stored in the GitHub Actions
cache or your own Cloudflare R2 bucket. Part of [keelstack](https://github.com/keelstack): tools
for bootstrapping projects.

> Status: under construction. See `docs/superpowers/specs/2026-10-14-hoard-design.md`.

```yaml
- uses: keelstack/hoard@v1
- run: pnpm turbo run build test lint
```

With Cloudflare R2 (no size or rate limits, shared across branches and laptops; the bucket is
kept bounded by a lifecycle rule and a size cap):

```yaml
- uses: keelstack/hoard@v1
  with:
    storage: r2
    r2-account-id: ${{ secrets.R2_ACCOUNT_ID }}
    r2-access-key-id: ${{ secrets.R2_ACCESS_KEY_ID }}
    r2-secret-access-key: ${{ secrets.R2_SECRET_ACCESS_KEY }}
    r2-bucket: ${{ secrets.R2_BUCKET }}
```

License: MIT.
