# PR-14C.md (greentic-integration)
# Title: Add events packs to OCI E2E once published (publish + timer/webhook baseline)

## Goal
Once `greentic-packs/events-*` are published on GHCR, extend the manifest + CI to run events E2E.

## Tasks
1) Update `tests/packs/manifest.txt` to include:
- `greentic-packs/events-dummy:<tag>` (preferred deterministic baseline)
- OR `greentic-packs/events-timer:<tag>`
- OR `greentic-packs/events-webhook:<tag>`
2) Update `scripts/make_pack_index.sh` overlay selection to include one events pack for tenant `integration`.
3) Add `tests/e2e_events_publish.rs`:
- run a publish flow or provider.invoke publish
- assert receipt_id returned OR state-store updated deterministically (depending on dummy provider behavior)
4) CI: enable this test unconditionally once packs exist.

## Acceptance criteria
- events publish passes deterministically in CI.
- no live external endpoints required.
