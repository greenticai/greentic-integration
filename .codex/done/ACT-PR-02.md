# ACT-PR-02 — greentic-integration-tester: JSON Capture + JSONPath Assertions + Normalization

## Goal
Enable robust assertions against component/runner outputs without brittle string matching.

## Scope
### Add directives
- `#CAPTURE_STDOUT > <path>` — capture last stdout to file
- `#CAPTURE_JSON > <path>` — capture last stdout and validate it is JSON
- `#EXPECT_JSONPATH <file> <jsonpath> <op> <value>`
  - `op`: `equals`, `contains`, `exists`, `not_exists`, `matches`
- `#NORMALIZE_JSON <in> > <out>`
  - stable key ordering
  - remove volatile fields (configurable list)
- `#DIFF_JSON <a> <b>`
  - prints a friendly diff on mismatch

### JSONPath implementation
- Prefer a small, well-maintained Rust crate that supports common JSONPath expressions.
- If JSONPath libs are limited, implement a constrained “dot path” evaluator as fallback (`a.b[0].c`).

### Normalization rules
Default normalize config:
- sort object keys recursively
- remove fields:
  - `meta.trace_id`
  - `meta.timestamp`
  - `envelope.trace_id`
  - any `*.duration_ms`

Allow overriding via `--normalize-config <path>`.

## Implementation details
### New modules
- `src/json/assert.rs`
- `src/json/normalize.rs`
- `src/json/diff.rs`

### CLI
- `--normalize-config <path>`

## Acceptance criteria
- README tests can assert:
  - flow validate JSON includes the expected node
  - component output JSON includes `rendered_card.version == "1.6"`
- Normalization produces stable diffs across runs.

## Test plan
- Add `tests/gtests/smoke/02_json_assert.gtest` exercising all new directives.

## Notes
This is a key enabler for matrix and negative testing.
