# Flake triage mode

Flake triage mode reruns failing `.gtest` scripts, collects diagnostics, and attempts
to minimize the failing repro into a smaller test.

## How it works
- When a test fails and `--triage-flakes` is enabled, the runner reruns the same test
  `N` times (default `3`).
- If any rerun passes, the test is marked as flaky in the transcript summary.
- Reruns enable verbose logging and expanded timeouts, and attempt to add runner
  trace output when supported.
- Best-effort minimization attempts to find the smallest failing prefix of the test.

Artifacts are written to:
`target/flake-artifacts/<test>/<timestamp>/`

Artifacts include:
- `original.gtest`
- `minimized.gtest` (if found)
- `run-XX.log` and `run-XX.json` for each rerun
- `env.txt`
- `summary.json`

## Run locally
```bash
cargo run -p greentic-integration-tester -- \
  --test tests/gtests/00_smoke_validator.gtest \
  --triage-flakes \
  --triage-runs 3
```

## Promote a minimized repro
1) Copy `minimized.gtest` into `tests/gtests/` with a new filename.
2) Add it to CI or local smoke runs.
3) Update expectations in the script so it fails deterministically.
