# PR-14A.md (greentic-integration)
# Title: OCI pack acquisition + deterministic index.json generator for E2E

## Goal
Add reusable tooling to:
1) resolve GHCR pack tags to immutable digests
2) generate a pack `index.json` in the runner’s format
3) make E2E tests consume packs from OCI deterministically

## Background / Constraints
- Runner resolves packs through a JSON index containing `tenants.<tenant>.main_pack` and optional `overlays`. :contentReference[oaicite:1]{index=1}
- Locators can be filesystem/HTTPS/OCI/etc and the watcher validates digest/signature. :contentReference[oaicite:2]{index=2}
- We must pin by digest to avoid flaky CI.
- No “latest” tags in CI.

## Deliverables
### 1) Script: resolve OCI digests
Add `scripts/oci_resolve_digests.sh`:
- Input: `PACKS_MANIFEST` file listing packs and tags, one per line:
  - `greentic-packs/messaging-dummy:0.4.10`
  - `greentic-packs/messaging-webex:0.4.10`
  - ...
- Output: `target/packs_resolved.json` with entries:
  - `{ "name": "greentic-packs/messaging-dummy", "tag": "0.4.10", "oci": "ghcr.io/greentic-ai/greentic-packs/messaging-dummy@sha256:..." , "digest": "sha256:..." }`
- Implementation:
  - Prefer `crane digest` if available; else use `skopeo inspect docker://... | jq -r .Digest`
  - Fail with clear error if neither tool exists
  - Do not require network except to GHCR

### 2) Script: generate runner index.json for a tenant
Add `scripts/make_pack_index.sh`:
- Inputs:
  - `TENANT_NAME` (default `integration`)
  - `RESOLVED_JSON` (default `target/packs_resolved.json`)
  - `MAIN_PACK_NAME` (default `greentic-packs/messaging-dummy` for baseline)
  - `OVERLAY_PACK_NAMES` (comma-separated or file)
- Output:
  - `target/index.json` with format matching runner docs:
    - `{"tenants": { "<tenant>": { "main_pack": {...}, "overlays": [...] }}}` :contentReference[oaicite:3]{index=3}
- Each pack entry must contain:
  - `reference.name` = pack name (string)
  - `reference.version` = tag (string)
  - `locator` = `oci://ghcr.io/greentic-ai/<name>@<digest>` (OCI locator)
  - `digest` = `sha256:...`
  - also include `path` for debugging (same as locator or omitted if your runner ignores it)
- IMPORTANT:
  - Keep overlays ordered.
  - Ensure output JSON stable (sorted keys or deterministic write).

### 3) Documentation
Add `docs/e2e_packs_from_oci.md`:
- How to run locally:
  - `docker login ghcr.io`
  - create packs manifest
  - run scripts
  - run tests with `PACK_INDEX_URL=file://.../target/index.json`
- How CI pins by digest.

## Default packs manifest (checked into repo)
Create `tests/packs/manifest.txt` with the packs you listed (messaging + secrets):
- greentic-packs/messaging-dummy
- greentic-packs/messaging-webchat
- greentic-packs/messaging-slack
- greentic-packs/messaging-webex
- greentic-packs/messaging-teams
- greentic-packs/messaging-whatsapp
- greentic-packs/messaging-telegram
- greentic-packs/messaging-email
- greentic-packs/secrets-k8s
- greentic-packs/secrets-aws-sm
- greentic-packs/secrets-azure-kv
- greentic-packs/secrets-gcp-sm
- greentic-packs/secrets-vault-kv
- greentic-packs/secrets-providers (if this is a bundle; keep optional)

Events packs:
- Add placeholder lines commented out:
  - `# greentic-packs/events-...:<tag>`
and document that PR-14C will activate once events packs are published.

## Tests
Add a fast test `tests/pack_index_format.rs`:
- validates `target/index.json` matches the expected schema structure (tenants/main_pack/overlays)
- does NOT require running the runner

## Acceptance criteria
- Running the scripts produces `target/index.json` deterministically with OCI locators + digests.
- Docs explain local + CI usage.
- CI can generate the index without manual edits.

## Notes for Codex
- Use only repo-local scripting (bash + jq).
- Do not add heavy dependencies; prefer `crane`/`skopeo` optional.
- If neither tool exists in CI image, install one in workflow (see PR-14B).
