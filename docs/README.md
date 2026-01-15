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

`.gtest` scripts are plain text files with one command or directive per line. Directives allow future flow control or environment setup without executing a command.

The `greentic-integration-validator` binary is intended to be invoked from `.gtest` scripts for assertions (for example, `file exists path/to/file`).

Examples live under `tests/gtests`, with shared fixtures under `tests/fixtures`.

### Directive reference

| Directive | Meaning |
| --- | --- |
| `@set KEY=VALUE` | Define a test variable for substitutions. |
| `@env KEY=VALUE` | Override environment variables for commands. |
| `@cd PATH` | Change working directory for subsequent commands. |
| `@timeout DURATION` | Set default timeout for commands (e.g. `500ms`, `2s`). |
| `@expect exit=0` / `@expect exit!=0` | Override the next command's exit expectation. |
| `@capture NAME` | Capture stdout/stderr for the next command. |
| `@print NAME` | Print a named capture to stdout. |
| `@skip REASON` | Skip the entire test with a reason. |

### Substitution rules

Substitutions use the `${VAR}` syntax. Precedence is:
1. `@set` variables
2. `@env` variables
3. Process environment variables
4. Built-ins: `WORK_DIR`, `TEST_DIR`, `REPO_ROOT`, `TMP_DIR`

Missing variables cause the test to fail with a line-numbered error.

### Using local binaries

To point the tester at locally built tools, prepend their target directory:

```
cargo run -p greentic-integration-tester -- \\
  --prepend-path ../greentic-runner/target/debug \\
  --test tests/gtests/00_smoke_validator.gtest
```

### Validator examples

```
greentic-integration-validator json path tests/fixtures/sample.json a.b[0].c --eq 3
greentic-integration-validator cbor find tests/fixtures/sample.cbor --string "hello"
```
