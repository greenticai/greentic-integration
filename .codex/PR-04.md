# PR-04: greentic-integration — Regression tests for provider extension CLIs

## Goal
Add CI regression tests that ensure the new provider-extension flow stays stable:
- greentic-pack providers list/info/validate
- greentic-dev pack new-provider

## Test cases
1) `providers_list_empty_when_no_extension`
- pack manifest fixture with no extensions
- `greentic-pack providers list` => exit 0, prints empty (or [] with --json)

2) `providers_validate_detects_duplicates`
- fixture with provider extension containing duplicate ProviderDecl IDs
- `greentic-pack providers validate --strict` => non-zero exit

3) `new_provider_adds_entry_and_lists`
- start with pack manifest fixture
- run `greentic-dev pack new-provider --id ... --runtime ...`
- run `greentic-pack providers list --json`
- assert new provider id present

## Harness
- Use existing integration harness patterns (temp dirs, command invocation helpers)
- No network calls.

## Acceptance criteria
- Tests pass locally and in CI, and fail when the CLIs regress.
