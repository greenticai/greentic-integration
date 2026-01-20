# Greentic Integration Examples

This repo now includes two runnable integration examples that wire events, messaging, and the Repo Assistant worker using mock/local components:

- Build status notifications: `flows/events_to_message/build_status_notifications.ygtc`
- Chat-driven Repo Assistant: `flows/chat_driven/repo_assistant.ygtc`

Use the per-example guides for details:

- [events_to_message_example.md](events_to_message_example.md)
- [chat_driven_repo_assistant.md](chat_driven_repo_assistant.md)
- [payload_samples.md](payload_samples.md)

Pack fixture:

- `packs/integration-demos/pack.json` pairs the flows with simple scenarios and golden transcripts.

Helper scripts:

- `scripts/run_build_status_demo.sh` – run the build-status notification flow with the demo config.
- `scripts/run_repo_assistant_demo.sh` – run the chat-driven Repo Assistant flow with the demo config.

Demo config:

- `configs/demo_local.yaml` – mock/local provider bindings for the example flows. Override via `CONFIG=/path/to/your.yaml` when running the scripts.

## Greentic Integration Tester/Validator

`.gtest` scripts are plain text files with one command or directive per line. The MVP runner uses `#` directives and explicit `#RUN` commands.

The `greentic-integration-validator` binary is intended to be invoked from `.gtest` scripts for assertions (for example, `file exists path/to/file`).

Examples live under `tests/gtests`, with shared fixtures under `tests/fixtures`.

### Directive reference

| Directive | Meaning |
| --- | --- |
| `#SET KEY=VALUE` | Define a test variable for substitutions. |
| `#ENV KEY=VALUE` | Override environment variables for commands. |
| `#RUN <command...>` | Run a shell command. |
| `#CAPTURE_STDOUT > <path>` | Write the last command stdout to a file. |
| `#CAPTURE_JSON > <path>` | Validate last stdout as JSON and write it to a file. |
| `#EXPECT_EXIT <code>` | Assert the last command's exit code. |
| `#EXPECT_STDOUT_CONTAINS <string>` | Assert the last command's stdout contains the string. |
| `#EXPECT_STDERR_CONTAINS <string>` | Assert the last command's stderr contains the string. |
| `#EXPECT_JSONPATH <file> <jsonpath> <op> <value>` | Assert JSONPath with `equals`, `contains`, `exists`, `not_exists`, `matches`. |
| `#WORKDIR <path>` | Change working directory (relative to the test root). |
| `#MKDIR <path>` | Create a directory. |
| `#WRITE <path> <<<EOF ... EOF` | Write a file using a simple heredoc. |
| `#NORMALIZE_JSON <in> > <out>` | Normalize JSON by removing volatile fields and sorting keys. |
| `#DIFF_JSON <a> <b>` | Diff two JSON files and fail on mismatch. |
| `#SAVE_ARTIFACT <path>` | Copy a file into the scenario artifacts folder. |
| `#TRY_SAVE_TRACE <path>` | Copy a trace file into `artifacts/trace.json` if it exists. |
| `#FAIL: drop_state_write` | Drop state writes for subsequent commands. |
| `#FAIL: delay_state_read <ms>` | Delay state reads by a fixed millisecond count. |
| `#FAIL: asset_transient_failure <n>/<m>` | Inject transient asset failures (ratio). |
| `#FAIL: duplicate_interaction` | Replay the next interaction once. |

### Substitution rules

Substitutions use the `${VAR}` syntax. Precedence is:
1. `#SET` variables
2. `#ENV` variables
3. Process environment variables
4. Built-ins: `WORK_DIR`, `TEST_DIR`, `REPO_ROOT`, `TMP_DIR`, `ARTIFACTS_DIR`

Missing variables cause the test to fail with a line-numbered error.

### Artifacts and replay

When `--artifacts-dir` is provided, each scenario writes step logs into an `artifacts/` subfolder and emits replay hints on failure (using `artifacts/trace.json` when available).

### Failure injection

Use `#FAIL:` directives to set `GREENTIC_FAIL_*` environment variables for downstream runner tooling. Pass `--seed` to set `GREENTIC_FAIL_SEED` for deterministic injection.

### JSON normalization

Use `#NORMALIZE_JSON` to apply stable key ordering and remove volatile fields. Override the default list with `--normalize-config <path>` (JSON file with a `remove` array).

Example normalize config:

```
{
  "remove": ["meta.trace_id", "meta.timestamp", "envelope.trace_id"]
}
```

### Using local binaries

To point the tester at locally built tools, prepend their target directory:

```
cargo run -p greentic-integration-tester -- \\
  run --gtest tests/gtests/smoke/01_basic.gtest --artifacts-dir /tmp/gtest-artifacts --seed 42
```

### Validator examples

```
greentic-integration-validator json path tests/fixtures/sample.json a.b[0].c --eq 3
greentic-integration-validator cbor find tests/fixtures/sample.cbor --string "hello"
```
