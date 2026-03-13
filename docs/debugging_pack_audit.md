# Debugging the pack audit (GHCR)

The pack audit downloads `ghcr.io/greenticai/greentic-packs/*` with `crane`, decodes `manifest.cbor`, and writes `target/pack-audit/pack_index.json`.

## Required auth/env

```bash
export GITHUB_TOKEN=ghp_xxx      # token for GitHub API (packages list)
export GITHUB_ORG=greenticai    # or GITHUB_USER=your-user
```

Required GitHub Actions permissions: `packages: read` (and `contents: read`).

## Run the audit

```bash
GITHUB_TOKEN=$GITHUB_TOKEN \
GITHUB_ORG=$GITHUB_ORG \
cargo run -p greentic-integration --bin pack_audit

cargo test -p greentic-integration --test pack_audit_oci -- --nocapture
```

Useful envs:
- `GT_PACKS_MODE=latest|all` (default `latest`)
- `GT_PACKS_INCLUDE_REGEX` / `GT_PACKS_EXCLUDE_REGEX`
- `GT_PACKS_LIMIT` to cap how many packs are processed
- `GT_PACK_AUDIT_DIR` to override `target/pack-audit`

If you see `crane is not authenticated...`, rerun the login command above and ensure your token has `packages:read`.

