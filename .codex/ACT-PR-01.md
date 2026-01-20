# ACT-PR-01 — greentic-integration-tester: .gtest Runner MVP (docs + matrix ready)

## Goal
Introduce a minimal but powerful `.gtest` scenario runner so we can execute:
- README-as-tests for `component-adaptive-card`
- Config matrix scenarios (pairwise for PRs; full for nightly)
- Negative cases with stable assertions

This PR delivers the **core runner** with variable substitution, step logging, and JUnit output.

## Scope
### Add
- `.gtest` parser (directives + shell-command lines)
- scenario execution engine
- per-step capture of stdout/stderr, exit code, and timing
- `--junit <path>` output for CI reporting
- `--artifacts-dir <path>` to collect logs consistently

### Directives (MVP)
- `#SET KEY=VALUE` — set variables
- `#ENV KEY=VALUE` — set environment variables for subsequent steps
- `#RUN <command...>` — run a shell command
- `#EXPECT_EXIT <code>` — assert last exit code
- `#EXPECT_STDOUT_CONTAINS <string>`
- `#EXPECT_STDERR_CONTAINS <string>`
- `#WORKDIR <path>` — change working directory (relative to test root)
- `#MKDIR <path>` — create directory
- `#WRITE <path> <<<EOF ... EOF` — write file content (simple heredoc syntax)

### Variable substitution
- `${VAR}` replaced in directive arguments and in `#RUN` commands.

## Implementation details
### New modules
- `src/gtest/mod.rs`
- `src/gtest/parser.rs`
- `src/gtest/executor.rs`
- `src/junit.rs`

### Output artifacts
Within `--artifacts-dir`, create:
- `step-001.stdout.log`, `step-001.stderr.log`
- `step-001.meta.json` (exit_code, duration_ms, command)
- `scenario.meta.json` (scenario name, start/end time, seed if any)

### CLI
Add a command:
- `greentic-integration-tester run --gtest <file> [--artifacts-dir ...] [--junit ...]`

### Error behavior
- First failing expectation stops the scenario (default).
- Add `--keep-going` flag to continue to collect more failures (optional; can be follow-up).

## Acceptance criteria
- Can run a trivial `.gtest` file that writes a temp file, runs `cat`, and asserts output.
- JUnit file is produced with one test case per scenario.
- Artifacts directory contains step logs and meta.

## Test plan
- Unit tests for:
  - directive parsing
  - variable substitution
  - JUnit generation
- Integration test scenario in `tests/gtests/smoke/01_basic.gtest`.

## Notes
This PR is intentionally minimal. JSON assertions and trace integration come next.
