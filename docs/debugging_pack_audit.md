# Debugging the pack audit (GHCR)

The pack audit downloads `ghcr.io/greentic-ai/greentic-packs/*` with `crane`, decodes `manifest.cbor`, and writes `target/pack-audit/pack_index.json`.

## Authenticate to GHCR

```bash
export GITHUB_TOKEN=ghp_xxx      # or GHCR_TOKEN if packs are private
export GITHUB_ACTOR=your-user    # defaults to oauth2 if unset
echo "$GITHUB_TOKEN" | crane auth login ghcr.io -u "${GITHUB_ACTOR:-oauth2}" --password-stdin
```

Required GitHub Actions permissions: `packages: read` (and `contents: read`).

## Run the audit

```bash
# optional: GT_CRANE_LOGIN=1 will make the binary attempt login with the token above
GT_CRANE_LOGIN=1 \
GITHUB_TOKEN=$GITHUB_TOKEN \
cargo run -p greentic-integration --bin pack_audit

cargo test -p greentic-integration --test pack_audit_oci -- --nocapture
```

Useful envs:
- `GT_PACKS_MODE=latest|all` (default `latest`)
- `GT_PACKS_INCLUDE_REGEX` / `GT_PACKS_EXCLUDE_REGEX`
- `GT_PACKS_LIMIT` to cap how many packs are processed
- `GT_PACK_AUDIT_DIR` to override `target/pack-audit`
- `CRANE_BIN` to point to a specific crane binary

If you see `crane is not authenticated...`, rerun the login command above and ensure your token has `packages:read`.
