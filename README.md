# Greentic Integration

This repository hosts the integration harness for the Greentic demo stack. It provides
infrastructure scaffolding, golden fixtures, and automated test targets that exercise the
full local environment (runner, packs, providers, and WebChat UI).

## Project Roadmap

The integration effort follows the PR-INT series (PR-INT-01 through PR-INT-15). Each step
adds a focused capability—from bootstrapping the repository, to provider simulators,
Playwright end-to-end tests, and documentation for adding new scenarios. Refer to
`docs/` for the detailed implementation plan.

## Getting Started

1. Install the required tooling (Docker, Rust, Node.js 18+, and Make).
2. Clone this repository.
3. Run `make help` to list the available developer commands.

## Repository Layout

```
compose/    Local infrastructure definitions (Docker Compose stacks)
harness/    Rust crates/binaries for simulators and smoke tests
packs/      Demo pack fixtures and golden snapshots
scripts/    Utility scripts (golden updates, repo pinning, etc.)
docs/       Project documentation and onboarding guides
```

> **Note:** Most directories are scaffolds as of PR-INT-01. Subsequent PR-INT tasks will
> populate them with runnable code, fixtures, and tests.

## Local Infrastructure Stack

Run `make stack-up` to start the deterministic local dependencies defined under
`compose/stack.yml`. The stack currently includes:

- NATS with JetStream + monitoring endpoint (ports 4222/8222)
- Redis 7 (port 6379)
- An ingress stub (port 8080) that surfaces a `/healthz` probe

You can verify the ingress health check with:

```bash
curl -fsS http://localhost:8080/healthz
```

Stop and clean up the containers with `make stack-down`.

## Pack Fixtures

Pack definitions live under `packs/`. Each pack exposes a manifest (`pack.json`), scenario
definitions, and golden transcripts. Run `make packs.test` to ensure manifests stay
well-formed. Set `GREENTIC_PACK_VALIDATE=1` to opt-in to the real `greentic-dev` /
`greentic-pack` CLI checks once those binaries are available locally.

### Greentic-dev E2E (PR-13)

The greentic-dev workflow test (`pr13_greentic_dev_e2e`) scaffolds a component, builds it
to Wasm, wires it into a pack, and runs/validates the pack. To run locally:

- Install `greentic-dev` and `packc` on your PATH.
- Install the Rust target: `rustup target add wasm32-wasip2`.
- Set strict mode to fail on missing tools: `GREENTIC_DEV_E2E_STRICT=1 cargo test -p greentic-integration pr13_greentic_dev_e2e -- --nocapture`.
- The test isolates HOME/XDG into a temp dir and uses fixture config at
  `tests/fixtures/greentic-dev/profiles/default.toml` plus the fixture public key
  `tests/fixtures/keys/ed25519_test_pub.pem`. No real secrets are required; pack verification
  uses `packc` with `--allow-unsigned` when supported, otherwise it signs with a temp key.

The negative suite (`e2e_greentic_dev_negative`) exercises failure paths (bad component build,
missing components, invalid add-step, invalid flow) and expects clear error messages without
producing artifacts; it shares the same environment setup as above.

The offline/local-store suite (`e2e_greentic_dev_offline`) proves greentic-dev can build and
run using a filesystem store without network:
- Uses isolated HOME/XDG + fixture profile, sets `CARGO_NET_OFFLINE=1` and
  `GREENTIC_COMPONENT_STORE=<tmp>/local-store`.
- Builds a component, installs it into the local store, validates/builds a pack with `--offline`,
  and attempts a pack run expecting `OFFLINE::WORLD` output.
- In non-strict mode, the test will skip if required tooling or cached crates are missing; set
  `GREENTIC_DEV_E2E_STRICT=1` to fail fast.

The snapshot stability suite (`e2e_greentic_dev_snapshot`) generates a pack/flow and asserts
their normalized YAML remains stable via `insta` snapshots. Run it with the same environment as
above; update snapshots intentionally via `cargo insta review` when schema changes are expected.

Regression harness (`e2e_regression`) runs the greentic-dev E2E scenarios (PR-13–PR-17: workflow,
negative, offline, snapshot, multi-pack). It shells out to `cargo test` for each scenario and
fails fast; set `E2E_REGRESSION_CHILD=1` to avoid recursion when invoking tests directly.

## Renderer Snapshots

Provider simulators live under `harness/providers-sim`. Run `make render.snapshot` to execute
the snapshot test suite, which compares renderer metrics against
`harness/providers-sim/golden/render_reports.json`. When intentionally updating packs or the
renderer logic, refresh the golden file via:

```bash
UPDATE_GOLDEN=1 make render.snapshot
```

## Runner Smoke Harness

`make runner.smoke` executes the deterministic runner harness housed in
`harness/runner-smoke`. It replays canned dev-mode traces to verify session continuity,
tenant isolation, state write expectations, and once-only effect log semantics before hooking
into the real Runner binary. The effect log contract lives in
`harness/runner-smoke/effect_log.schema.json`.

## Demo Payload Replays

- `make demo.replay.build` / `make demo.replay.chat` replay the sample EventEnvelope/ChannelMessageEnvelope payloads through the runner emit proxy.
- CI starts the `greentic-integration` server and runs these targets with `USE_SERVER=1` so they POST to `http://localhost:8080/runner/emit`. Locally, the make targets default to the in-process stub unless you set `USE_SERVER=1`.

## Dev Mode Check

`make dev.min` now runs `scripts/dev-check/check.sh`, which verifies essential environment
variables (`DEV_API_KEY`, `DEV_TENANT_ID`), ensures telemetry is disabled for local runs,
checks Docker/Docker Compose availability, and looks for the hot-reload token under
`.dev/reload.token`. Logs land in `.logs/dev-check.log`.

## WebChat Contract Tests

`make webchat.contract` hits the Direct Line-compatible backend endpoints
(`/tokens/generate`, `/conversations`, `/activities`). By default it runs against an
in-process stub server so the suite works offline. Point it at a real backend by exporting
`WEBCHAT_BASE_URL=https://your-service.example.com`.

## WebChat Playwright E2E

`make webchat.e2e` executes the Playwright UI suite located under `webchat-e2e/`. Install the
Node dependencies (`npm install`) when network access is available. The harness automatically
tries to download the browser binaries locally (`PLAYWRIGHT_BROWSERS_PATH=0`); if that fails,
the target will log a warning and skip execution. Review `.logs/webchat-e2e.log` for details.
The default run targets Chromium; set `PLAYWRIGHT_PROJECT=firefox` (or any other configured
project) to run a different browser locally.

## Golden Snapshot Management

Golden reports (renderer outputs, etc.) should only change when intentionally refreshed. Run:

```bash
UPDATE_GOLDEN=1 make golden.update
```

The script enforces a clean working tree, regenerates snapshots (currently via
`make render.snapshot`), and writes logs to `.logs/golden-update.log`. Commit the resulting
changes to keep CI green; any drift detected by CI indicates the golden refresh step was
skipped.

## Continuous Integration

`.github/workflows/integration.yml` runs on pushes/PRs and nightly at 05:00 UTC. It fans out
into four jobs (lint, packs, harness, and webchat). The webchat job runs Chromium in the fast
path and expands to Chromium + Firefox on the nightly schedule. Cargo and npm/Playwright
artifacts are cached to keep the workflow fast.

## Local CI

Run `./ci/local_check.sh` before pushing to ensure the same suite passes locally (fmt, clippy,
workspace tests, and all Make targets including packs, harnesses, and WebChat checks).

## Cross-Repo Pinning

Use `./scripts/pin_repo.sh <org/repo> <sha>` to write a `[patch]` override into
`.cargo/config.toml`. This lets you test unreleased dependencies (e.g.
`./scripts/pin_repo.sh greentic-ai/greentic-messaging deadbeef`). Remove the generated section
from the config (or run the script again with a new SHA) to unpin.

## Contributor Docs

- `docs/ADDING_A_SCENARIO.md` – walkthrough for creating new packs/scenarios and refreshing
  golden data.
- `docs/ADDING_A_PROVIDER_SIM.md` – process for extending provider simulators and updating
  capability parity checks.

## End-to-End Harness

The E2E harness lives in `crates/app/src/harness`. Run the smoke test with:

```bash
cargo test -p greentic-integration e2e_smoke
```

`TestEnv` writes logs/artifacts under `target/e2e/<test-name>/`; set `E2E_TEST_NAME` to control
the folder name (defaults to a sanitized thread name or timestamp).

Infra-backed E2E (NATS + Postgres) uses Docker Compose in `tests/compose/compose.e2e.yml`:

```bash
cargo test -p greentic-integration e2e_infra
```

Logs are captured under `target/e2e/<test-name>/logs/compose.log` before teardown.

Pack lifecycle and scenario DSL tests:

```bash
cargo test -p greentic-integration e2e_pack_lifecycle
cargo test -p greentic-integration e2e_scenario_smoke
cargo test -p greentic-integration e2e_multi_tenant_isolation
```

Pack helpers look for binaries under `tests/bin/`, `target/{release,debug}/`, or PATH and stub when unavailable, writing artifacts to `target/e2e/<test>/artifacts/`.

Messaging/provider E2E (`e2e_messaging_provider`):
- Brings up the compose stack (NATS + Postgres), publishes inbound messages over NATS, captures outbound payloads via a stub HTTP provider sink, and asserts text/thread continuity plus AdaptiveCard preservation.
- Artifacts land under `target/e2e/<test>/artifacts/provider-e2e/<case>/outbound.json`.
- Skips locally when Docker is unavailable; set `E2E_REQUIRE_DOCKER=1` to fail instead of skipping (CI sets this).

Greentic stack boot (runner/deployer/store) uses locally available binaries (looked up under
`tests/bin/`, `target/{release,debug}/`, or PATH). The stack test will skip if binaries are
missing:

```bash
cargo test -p greentic-integration e2e_stack_boot
```

Local ingress-control E2E (`e2e_ingress_control`) stages local `control-chain` + `fast2flow`
`.gtpack` fixtures, builds a demo flow index, and routes sample chats through the local
`greentic-fast2flow-routing-host`.

```bash
./scripts/build_e2e_fixtures.sh
./scripts/fetch_fast2flow_release.sh
cargo test -p greentic-integration --test e2e_ingress_control -- --nocapture
```

Environment overrides:
- Fixture repos/files:
  `GREENTIC_CONTROL_CHAIN_PATH`,
  `GREENTIC_CONTROL_CHAIN_GTPACK`, `GREENTIC_FAST2FLOW_GTPACK`,
  `GREENTIC_FAST2FLOW_FLOWS_JSON`.
- Binary lookup:
  `GREENTIC_FAST2FLOW_CLI_BIN`, `GREENTIC_FAST2FLOW_HOST_BIN`.
  Default release cache:
  `artifacts/fast2flow-release/latest/greentic-fast2flow`
  `artifacts/fast2flow-release/latest/greentic-fast2flow-routing-host`
- Text fixtures:
  `GREENTIC_E2E_SCOPE`, `GREENTIC_E2E_REFUND_TEXT`, `GREENTIC_E2E_SHIPPING_TEXT`,
  `GREENTIC_E2E_HELLO_TEXT`, `GREENTIC_E2E_UNKNOWN_TEXT`.

## E2E Test Tiers (CI)

- **L0/L1 (PR)**: `e2e_smoke`, `e2e_scenario_smoke`, `e2e_retry_backoff_flaky_tool`, `e2e_config_precedence`, `e2e_pack_lifecycle`
- **L2 (nightly/dispatch)**: `e2e_infra`, `e2e_stack_boot`, `e2e_multi_tenant_isolation` plus L0/L1 set

On CI failure, `target/e2e/**` is uploaded for debugging (logs, observations, artifacts).

## Local E2E Runner

Use `./scripts/e2e.sh <tier>` for local runs:

```bash
./scripts/e2e.sh l1          # run L1 suite
./scripts/e2e.sh l2 --focus e2e_multi_tenant_isolation
```

Flags:
- `--focus <pattern>` – run a single test
- `E2E_KEEP=1` – retain `target/e2e` between runs
- `RUST_LOG=info` (default) can be overridden for verbose logs

On failure, the script prints the paths under `target/e2e` for quick inspection.
