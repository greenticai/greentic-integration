PR-GI-01 - Replace the old chat2flow ingress-control plan with a real fast2flow routing E2E

Repo: greentic-integration
Date: 2026-03-06
Status: implemented and locally passing

1) Why the original PR note had to change

The previous draft targeted:
- `../greentic-chat2flow`
- `../greentic-marker-pack`
- `greentic-operator demo ingress` as the main assertion surface

That plan no longer matched the repos on disk:
- `../greentic-chat2flow` is not present
- `../greentic-marker-pack` is not present
- `../greentic-fast2flow` is present and its current contract is not "chat2flow renamed"; it is a routing-hook pack plus a mounted-index runtime
- `../greentic-control-chain` remains the stage-0 deterministic gate and `fast2flow` now composes with it through an explicit capability dependency

So the right rewrite is not a find/replace. The test has to follow the actual upstream contract.

2) What upstream changed

`../greentic-control-chain`
- Keeps deterministic ingress-control behavior: explicit-path validation plus rules/policy handling.
- Ships `dist/routing-ingress-control-chain.gtpack`.
- Is the capability dependency that routing extensions must require.

`../greentic-fast2flow`
- Adds a first-class index contract:
  - `greentic-fast2flow index build`
  - `greentic-fast2flow index inspect`
- Adds deterministic routing tests around indexed `FlowDoc` data.
- Adds mounted-runtime tests for the packaged host boundary (`greentic-fast2flow-routing-host` / `HostRuntime`).
- Builds `dist/fast2flow.gtpack`.
- Declares a pack dependency on `routing.ingress.control.chain` with required capability `greentic.cap.ingress.control.v1`.

That means the integration test should prove:
- local control-chain + fast2flow fixture packs are buildable and stageable
- demo app flow docs can be indexed locally
- mounted fast2flow runtime reads those indexes
- representative chats route to the expected flow targets

3) What this PR now adds

`crates/app/tests/e2e_ingress_control.rs`
- Enables the test by default.
- Stages two real fixture packs:
  - `control-chain.gtpack`
  - `fast2flow.gtpack`
- Verifies each `.gtpack` is structurally valid enough to contain `pack.cbor`.
- Builds a demo fast2flow index from local flow docs.
- Inspects the index and asserts it contains 3 demo flows.
- Routes four messages through `greentic-fast2flow-routing-host`:
  - `"refund please"` -> `demo-support/refund_flow`
  - `"shipping update"` -> `demo-ops/shipping_flow`
  - `"hello there"` -> `demo-assistant/welcome_flow`
  - `"abracadabra"` -> `continue`

`fixtures/fast2flow/demo_app_flows.json`
- Local demo flow docs used to build the mounted index.
- Represents "demo app packs" at the routing layer by assigning realistic pack ids and flow targets.

`scripts/build_e2e_fixtures.sh`
- Rewritten to resolve/build only the repos that actually exist:
  - `../greentic-control-chain`
  - `../greentic-fast2flow`
- Produces:
  - `fixtures/packs/control-chain.gtpack`
  - `fixtures/packs/fast2flow.gtpack`

`README.md`
- Updated to describe the new fast2flow-based E2E and its env overrides.
- Removed the stale `[patch.crates-io]` claim; there is no such patch block in the current workspace.

`scripts/fetch_fast2flow_release.sh`
- Downloads the latest private GitHub release binaries for:
  - `greentic-fast2flow`
  - `greentic-fast2flow-routing-host`
- Extracts them into `artifacts/fast2flow-release/<tag>/`
- Refreshes `artifacts/fast2flow-release/latest/`

4) Why this test shape is better

It validates the part that materially changed upstream:
- `fast2flow` is now about mounted indexes plus deterministic flow routing
- the strongest local assertion is the routed target, not a placeholder marker string

It also removes two broken assumptions from the old note:
- no dependency on missing repos
- no dependency on a messaging-provider path that `fast2flow` does not implement

This gives a deeper and more honest local E2E:
- fixture packs exist
- indexing works
- the runtime consumes the generated index
- the right chat is dispatched to the right flow

5) How to run it locally

Build fixture packs:

```bash
./scripts/build_e2e_fixtures.sh
```

Fetch the latest private fast2flow release binaries:

```bash
cd /projects/ai/greentic-ng/greentic-integration
./scripts/fetch_fast2flow_release.sh
```

Run the test:

```bash
cd /projects/ai/greentic-ng/greentic-integration
cargo test -p greentic-integration --test e2e_ingress_control -- --nocapture
```

Useful overrides:
- `GREENTIC_CONTROL_CHAIN_PATH`
- `GREENTIC_CONTROL_CHAIN_GTPACK`
- `GREENTIC_FAST2FLOW_GTPACK`
- `GREENTIC_FAST2FLOW_FLOWS_JSON`
- `GREENTIC_FAST2FLOW_CLI_BIN`
- `GREENTIC_FAST2FLOW_HOST_BIN`

6) Acceptance criteria

Local run passes when:
- fixture builder writes both `.gtpack` files
- the test confirms the two gtpacks contain `manifest.cbor`
- `greentic-fast2flow index build` emits 3 indexed demo flows
- `greentic-fast2flow index inspect` reports `scope=demo entries=3`
- mounted host dispatches:
  - refund chat to `demo-support/refund_flow`
  - shipping chat to `demo-ops/shipping_flow`
  - greeting chat to `demo-assistant/welcome_flow`
- unrelated text fails open to `continue`

7) Notes and limits

- This test intentionally does not use `greentic-operator demo ingress` anymore. That path expects a messaging provider pack, while `fast2flow` is a routing-hook runtime.
- The control-chain/runtime composition dependency itself is already asserted upstream in `../greentic-fast2flow/ci/test_gtpack_replay.sh`; this PR validates the local integration side by staging the real packs and exercising the routing runtime with indexed demo flows.
- The test depends on neighboring local repos for `.gtpack` fixtures and on the latest private GitHub release binaries for `greentic-fast2flow` and `greentic-fast2flow-routing-host`.
