# StatePR-05 — greentic-integration: Conformance tests for payload templating + WIT state-store

## Repo
`greentic-integration`

## Goal
Add black-box integration tests that prevent drift across repos by validating:
1) Payload wiring via node config templating works end-to-end (`entry/prev/node/state`).
2) Component state access via WIT `greentic:state/store@1.0.0` works end-to-end (read/write/delete) with capability gating.

## Non-goals
- Do not rely on runner-internal snapshot key conventions.
- Do not test greentic-dev.

---

## Test Plan

### A) Payload templating test (no state)
Create a flow fixture with:
- Node `start`: outputs JSON:
  `{ "user": { "id": 1, "name": "Ada" }, "status": "ready" }`
- Node `process`: its input config references:
  - `user_id: {{node.start.user.id}}`   (typed insertion should keep number)
  - `name: {{node.start.user.name}}`
  - `status: {{prev.status}}`
  - `message: {{entry.message}}`

Assert:
- `process` receives correctly typed `user_id` (number, not string).
- all fields are resolved as expected.

### B) State-store roundtrip test (WIT) with capability
- Component `writer` declares state-store write.
  - writes bytes/JSON to a known key (using runner’s standard prefix+key convention)
- Component `reader` declares state-store read.
  - reads the same key and returns it
Assert:
- roundtrip works, and delete works.

### C) Capability gating test
- Component without state-store capability attempts to call state-store.
Assert:
- runner prevents access (interface not linked or “capability denied”).

### D) Tenant scoping test
Execute similar roundtrip under two different TenantCtx values:
- tenant B must not see tenant A’s state.

---

## Execution harness
Run via the real `greentic-runner` pipeline (as close to production as possible):
- build the test components to wasm
- build a minimal pack/flow fixture if that is the standard execution mode
- execute and assert outputs

## Acceptance Criteria
- Tests are deterministic and run in CI.
- Any drift in templating context/typed insertion/state-store wiring/capabilities breaks CI.
