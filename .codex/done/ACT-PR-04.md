# ACT-PR-04 — greentic-integration-tester: Failure Injection Directives (State + Asset + Interaction)

## Goal
Multiply scenario coverage by running the same `.gtest` scripts under controlled fault conditions.

## Scope
### Add directives
- `#FAIL: drop_state_write`
- `#FAIL: delay_state_read <ms>`
- `#FAIL: asset_transient_failure <n>/<m>` (e.g. 1/10)
- `#FAIL: duplicate_interaction` (replay same interaction once)

### Determinism
- Add `--seed <u64>` to drive deterministic fault injection.

## Implementation approach
Two options (choose based on current architecture):
1) **Harness-level**: wrap calls to runner/CLI tools with env vars that enable injection in runner.
2) **Runner-level** (preferred): these directives set env vars consumed by greentic-runner decorators.

This PR implements the directive parsing + env propagation. The actual injection behavior is implemented in greentic-runner ACT-PR-04.

## Env vars to standardize
- `GREENTIC_FAIL_DROP_STATE_WRITE=1`
- `GREENTIC_FAIL_DELAY_STATE_READ_MS=250`
- `GREENTIC_FAIL_ASSET_TRANSIENT=1/10`
- `GREENTIC_FAIL_DUPLICATE_INTERACTION=1`
- `GREENTIC_FAIL_SEED=<seed>`

## Acceptance criteria
- Same scenario run with and without injection produces different behavior deterministically.
- Seed makes runs reproducible.

## Test plan
- Add one scenario that reads/writes state and demonstrates injected drop/delay.

## Notes
Keep injection off by default; only enabled when directives are present.
