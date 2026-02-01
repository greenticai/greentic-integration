# ACT-PR-03 — greentic-integration-tester: Artifacts Convention + Trace Attachment + Replay Hint

## Goal
Make failures instantly diagnosable by standardizing artifact output and printing a one-line replay instruction.

## Scope
### Artifacts convention
For each scenario, always create:
- `artifacts/` folder containing:
  - `trace.json` (if produced by runner/component)
  - `inputs.json` (captured inputs where applicable)
  - `output.json` (captured output where applicable)
  - `step-*.stdout.log`, `step-*.stderr.log`, `step-*.meta.json`

### Add directives
- `#SAVE_ARTIFACT <path>` — copy file into scenario artifacts dir
- `#TRY_SAVE_TRACE <path>` — if file exists, copy to `artifacts/trace.json`

### Replay hint
On failure, print (to console and junit failure message) something like:
- `Replay: greentic-runner replay <artifacts-dir>/artifacts/trace.json`
- If trace missing, print: `No trace.json found; enable runner tracing with --trace-out`.

## Implementation details
- Add a small helper that attempts to discover trace output locations:
  - env var `GREENTIC_TRACE_OUT`
  - common runner arg `--trace-out`
  - known default locations in artifacts dir

## Acceptance criteria
- A failing `.gtest` run uploads a single folder with everything needed.
- Console output includes a deterministic replay command.

## Test plan
- Add a scenario that deliberately fails and writes a dummy trace file; ensure it is copied.

## Notes
This PR does not implement failure injection. That’s a later PR (or done in runner).
