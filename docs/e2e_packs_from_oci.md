# E2E packs from OCI (PR-14A)

This flow resolves GHCR tags to immutable digests and produces a runner-compatible `index.json`
so E2E runs consume OCI packs deterministically (no `latest` tags).

## Prereqs
- `docker login ghcr.io` (read access to the packs)
- `jq`
- One of: `crane` (preferred) or `skopeo`

## Steps (local)
1) Prepare/adjust `tests/packs/manifest.txt` (name:tag per line, no `latest`).
2) Resolve digests:
   ```bash
   PACKS_MANIFEST=tests/packs/manifest.txt ./scripts/oci_resolve_digests.sh
   # writes target/packs_resolved.json
   ```
3) Generate runner index:
   ```bash
   TENANT_NAME=integration \
   MAIN_PACK_NAME=greentic-packs/messaging-dummy \
   OVERLAY_PACK_NAMES="" \
   ./scripts/make_pack_index.sh
   # writes target/index.json
   ```
4) Point tests/runner to the index (example):
   ```bash
   PACK_INDEX_URL="file://$(pwd)/target/index.json" cargo test -p greentic-integration ...
   ```

## CI usage
- CI should run `oci_resolve_digests.sh` then `make_pack_index.sh` with the manifest checked into
  `tests/packs/manifest.txt`. Both scripts fail fast if required tools or tags are missing.
- Because tags are resolved to digests, reruns are deterministic; no manual edits needed.

## Notes
- Overlay packs: pass comma-separated names via `OVERLAY_PACK_NAMES=pack1,pack2` or `OVERLAY_PACK_NAMES=@/path/to/file`.
- Events packs: placeholder lines are commented out in `tests/packs/manifest.txt`; activate once events packs are published.
