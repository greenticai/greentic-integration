# Pack Audit (GHCR `greentic-packs/*`)

This audit downloads published packs from GHCR, decodes their `.gtpack` manifests, and writes a summary to `target/pack-audit/pack_index.json` (plus a human summary at `target/pack-audit/pack_index.md`).

## Running locally

```bash
GITHUB_TOKEN=ghp_xxx \
GITHUB_ORG=greenticai \ # or GITHUB_USER=your-user
GT_PACKS_MODE=latest \ # or all
GT_PACKS_LIMIT=10 \    # optional
RUST_LOG=info \
cargo run -p greentic-integration --bin pack_audit

cargo test -p greentic-integration --test pack_audit_oci -- --nocapture
```

Optional filters:
- `GT_PACKS_INCLUDE_REGEX` / `GT_PACKS_EXCLUDE_REGEX`
- `GT_PACKS_ORG` (default `greenticai`)
- `GT_PACK_AUDIT_DIR` to change output directory

## What the tests enforce
- Each audited entry must decode `manifest.cbor`.
- Provider packs must expose the canonical `greentic.provider-extension.v1` extension with populated provider declarations and runtime bindings.
- Packs are classified into messaging/events/secrets/other; categories must not be empty.

