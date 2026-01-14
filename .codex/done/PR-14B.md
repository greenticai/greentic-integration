# PR-14B.md (greentic-integration)
# Title: E2E tests using GHCR packs (messaging + secrets) with provider-core-only gate

## Goal
Run end-to-end tests against the published GHCR packs, using the generated OCI index.json.

## CI invariants
- Always pin packs by digest (via PR-14A scripts).
- Always run with provider-core-only enabled:
  - `GREENTIC_PROVIDER_CORE_ONLY=1`

## Deliverables
### 1) GitHub Actions workflow job
Add `.github/workflows/e2e_packs_oci.yml`:
Steps:
1) Checkout greentic-integration
2) Login to GHCR (use `GITHUB_TOKEN`)
3) Install `skopeo` or `crane` + `jq`
4) Run:
   - `scripts/oci_resolve_digests.sh tests/packs/manifest.txt`
   - `scripts/make_pack_index.sh` with:
     - `TENANT_NAME=integration`
     - `MAIN_PACK_NAME=greentic-packs/messaging-dummy`
     - `OVERLAY_PACK_NAMES=greentic-packs/secrets-k8s` (and optionally webchat)
5) Export:
   - `PACK_INDEX_URL=file://$PWD/target/index.json`
   - `GREENTIC_PROVIDER_CORE_ONLY=1`
6) Run E2E tests.

### 2) Runner/harness integration
Implement a test harness helper `tests/support/runner_harness.rs` that:
- starts the embedded host/runner in-process (preferred) OR shells out to your runner binary
- loads packs using PACK_INDEX_URL and tenant `integration`
- calls one flow per domain in the simplest deterministic way

If runner is available as a crate dependency, use embedded host APIs; do not spin full HTTP server unless needed.

### 3) E2E tests (minimal but complete)
Add:
- `tests/e2e_messaging_dummy_send.rs`
  - ensures messaging-dummy pack can be invoked end-to-end
  - if your runner exposes ingress adapters, call the minimal “send” flow entrypoint (or invoke provider op through a flow)
  - assert response includes `message_id`
- `tests/e2e_secrets_k8s_smoke.rs`
  - load secrets-k8s pack
  - run a minimal secrets op (or a known flow in that pack)
  - if no flow exists, implement a minimal “provider.invoke op=healthcheck” flow fixture just for test (in integration repo)
  - assert OK

Important: if real secrets providers require cloud credentials, then use only:
- `secrets-k8s` (can run against a local k8s mock) OR
- rely on a `secrets-providers` bundle pack that includes an in-memory provider
If neither exists, change the baseline to a deterministic secrets pack once published.

### 4) Fail-fast when events packs missing (but do not block PR-14B)
Do NOT add events tests here. Just:
- add a test that checks if any `greentic-packs/events-*` lines exist in manifest and are resolvable;
- if not, print a clear skipped message.

## Acceptance criteria
- CI job passes using published messaging packs and at least one secrets pack.
- provider-core-only flag is enabled in CI.
- Packs are pinned by digest.
- Tests are deterministic (no external APIs).

## Notes for Codex
- Keep the E2E tests small: one test per domain.
- If the runner expects tenant selection via env, set it explicitly.
- Do not use “latest” tags.
