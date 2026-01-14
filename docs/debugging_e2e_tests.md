# Debugging E2E Tests (greentic-integration)

List tests in a suite:

```bash
cargo test -p greentic-integration --test e2e_config_precedence -- --list
```

Run a single test with logging and no output capture:

```bash
RUST_TEST_THREADS=1 \
RUST_LOG=trace \
RUST_LOG_STYLE=always \
RUST_BACKTRACE=1 \
GREENTIC_PROVIDER_CORE_ONLY=1 \
cargo test -p greentic-integration --test e2e_config_precedence -- e2e_config_secrets_precedence --exact --nocapture
```

Notes:
- Logs use `tracing_subscriber` and respect `RUST_LOG`; set `GT_TEST_LOG=1` to force init even if `RUST_LOG` is unset.
- Progress `eprintln!` markers in the tests print to stderr even if logging is misconfigured.
- The harness writes compose logs under `target/e2e/<test>/logs/`; if Docker/Compose is involved, you can also run `docker compose -f tests/compose/compose.e2e.yml logs`.
