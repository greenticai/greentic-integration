# Repository Overview

## 1. High-Level Purpose
- Integration harness for the Greentic stack: Rust CLI/HTTP app plus Docker-backed E2E harness that exercises packs, runner flows, and provider simulators.
- Supplies deterministic fixtures, simulators, and tiered E2E/CI tooling (Compose for NATS/Postgres, scenario DSL, and local scripts/workflows) to validate end-to-end behavior and developer workflows.

## 2. Main Components and Functionality
- **Path:** `crates/app`  
  - **Role:** Main integration crate/CLI (`greentic-integration`) and E2E harness library.  
  - **Key functionality:** Loads config defaults, indexes packs, serves pack/session/runner HTTP endpoints, CLI helpers for pack list/reload/plan and session maintenance; session store supports memory, file, or Redis; runner proxy records events and can forward to an external runner URL; pack watch reloads index on file changes; `harness::TestEnv` spins Compose NATS+Postgres with health probes, captures logs/artifacts under `target/e2e/<test>/`, and can boot local Greentic binaries; pack helpers build/verify/install via greentic binaries when present (fallback with optional strict mode); config/secret layer merge utilities; fixture loader/normalizer; scenario DSL/runner for NATS publish/await, HTTP POST, and JSON assertions; greentic-dev E2E harness isolates HOME/XDG config, writes fixture profiles to both XDG and HOME, and verifies packs with `packc` (allow-unsigned if supported, otherwise temp signing).
- **Path:** `crates/deploy-plan-component`  
  - **Role:** Minimal deploy-plan component for WASM/guest bindings.  
  - **Key functionality:** Reads deployment plan via host runtime trait and writes pretty JSON to `/iac/plan.json`, with injectable runtime for tests.
- **Path:** `harness/providers-sim`  
  - **Role:** Deterministic provider simulator and renderer snapshot suite.  
  - **Key functionality:** Capability parity checks (`capabilities/providers.yaml`), golden comparisons in `golden/render_reports.json`, and snapshot update flow (`make render.snapshot`).
- **Path:** `harness/runner-smoke`  
  - **Role:** Runner smoke harness validating traces/cases before hitting real runner.  
  - **Key functionality:** Replays cases from `cases/`, checks tenant isolation, ordered effects, state writes, and once-only trace IDs; runnable via `make runner.smoke`.
- **Path:** `tests/` (+ `tests/compose/compose.e2e.yml`)  
  - **Role:** Tiered E2E suite.  
  - **Key functionality:** Compose NATS/Postgres infra test (`e2e_infra`), smoke harness (`e2e_smoke`), scenario DSL round-trip (`e2e_scenario_smoke`), retry/backoff flaky tool (`e2e_retry_backoff`), config+secrets precedence (`e2e_config_precedence`), pack lifecycle stub/build (`e2e_pack_lifecycle`), stack boot when binaries exist (`e2e_stack_boot`), multi-tenant isolation over NATS (`e2e_multi_tenant_isolation`), greentic-dev workflow test (`pr13_greentic_dev_e2e`), greentic-dev negative validation suite (`e2e_greentic_dev_negative`), offline/local-store workflow (`e2e_greentic_dev_offline`), greentic-dev snapshot stability (`e2e_greentic_dev_snapshot`), multi-pack shared-component test (`e2e_greentic_dev_multi_pack`), and regression runner (`e2e_regression`) that shells out to run the greentic-dev suites. Tests use isolated HOME/XDG roots, fixture distributor profile/key, and skip when greentic-dev/packc or wasm targets are unavailable. Artifacts/logs land under `target/e2e/`.
- **Path:** `crates/app/tests/e2e_ingress_control.rs` + `scripts/build_e2e_fixtures.sh`  
  - **Role:** Local-first ingress-control E2E scaffold for operator+runner+pack hook integration.  
  - **Key functionality:** Ignored integration test stages local fixture packs (`control-chain`, `chat2flow`, `marker-pack`) into a temporary demo bundle, starts a local runner process, invokes `greentic-operator demo ingress` for `"refund please"` and `"hello"` paths, and asserts MARKER behavior using response-first + log fallback checks; companion script builds/copies local `.gtpack` fixtures into `fixtures/packs/` (gitignored) with env-path overrides.
- **Path:** `fixtures/`, `packs/`, `flows/`, `samples/`  
  - **Role:** Shared data for tests and demos.  
  - **Key functionality:** Standard fixture layout (packs/config/secrets/inputs/expected), pack project/gtpack fallback (`fixtures/packs/hello`), flow definitions (`flows/chat_driven`, `flows/events_to_message`), greentic-dev fixture profile (`tests/fixtures/greentic-dev/profiles/default.toml`) and public key (`tests/fixtures/keys/ed25519_test_pub.pem`), and sample payloads consumed by tests and replay scripts.
- **Path:** `scripts/e2e.sh`, `scripts/fetch_greentic_binaries.sh`, `.github/workflows/e2e.yml`  
  - **Role:** Tiered E2E runners (local and CI) and binary bootstrap.  
  - **Key functionality:** `scripts/e2e.sh` wraps cargo tests by tier with focus filtering and artifact hints; `scripts/fetch_greentic_binaries.sh` pulls runner/deployer/store release assets with checksum verification into `tests/bin/linux-x86_64`; workflow runs L0/L1 on PR, L2 nightly/dispatch, installs `wasm32-wasip2`, fetches binaries, runs `pr13_greentic_dev_e2e` alongside other tiers, and uploads `target/e2e` artifacts.
- **Path:** `compose/stack.yml`, `configs/`, `docs/`, `webchat-e2e/`  
  - **Role:** Local infra/ingress stack, config samples, contributor docs, and Playwright UI harness.  
  - **Key functionality:** NATS/Redis/nginx compose stack, demo configs, scenario/provider how-tos, and webchat contract/UI suites (stubbed backend by default).

## 3. Work In Progress, TODOs, and Stubs
- **crates/app/src/main.rs:1370-1454** — Runner proxy still synthesizes events locally; external forwarding is best-effort HTTP only (no real runner API contract yet).
- **crates/app/src/harness/pack.rs:64-115** — Pack build/verify/install still fall back to fixtures/stubs when binaries are missing; strict mode available via `GREENTIC_PACK_STRICT`.
- **crates/app/tests/e2e_stack_boot.rs:1-40** — Test skips (or fails when `GREENTIC_STACK_STRICT=1`) if greentic binaries are absent; stack boot coverage depends on local binaries.
- **crates/app/tests/e2e_ingress_control.rs** — New test is `#[ignore]` by default and depends on local operator/runner binaries plus fixture packs from sibling repos; assertions/inputs are environment-tunable.
- **crates/app/tests/pr13_greentic_dev_e2e.rs** — Greentic-dev workflow test tolerates missing greentic-dev/packc binaries or `wasm32-wasip2` target by skipping steps; uses dual HOME/XDG fixture config to reduce profile lookup issues; still not a fully strict end-to-end verification without all tools installed.
- **crates/deploy-plan-component/src/lib.rs:15-36** — Guest runtime returns an error because deploy-plan host bindings are absent; component remains a placeholder.

## 4. Broken, Failing, or Conflicting Areas
- Greentic-dev flow add-step can still report “profile default not found” if the fixture config is not picked up; test currently skips unless strict mode forces failure. Ensure greentic-dev reads `$XDG_CONFIG_HOME/greentic-dev/config.toml`/`$HOME/.config/greentic-dev/config.toml`.
- Component scaffold/build relies on the `wasm32-wasip2` target; CI now installs it, but local runs may skip when missing.
- Binary-dependent tests rely on downloaded greentic runner/deployer/store assets; strict mode fails fast if the fetch script cannot resolve or verify checksums.

## 5. Notes for Future Work
- Firm up runner proxy with a real runner API contract and response handling; expand tests.
- Provide local greentic binaries (runner/deployer/store) and make stack boot/pack helpers run in strict mode in CI.
- Make greentic-dev workflow test fully strict by ensuring distributor profiles/keys and greentic-dev deps are available in CI or by vendoring minimal fixtures.
- Replace pack helper fallbacks with required binaries in high-confidence environments; consider feature gating stubs.
