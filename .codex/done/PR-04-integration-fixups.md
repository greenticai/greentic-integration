# PR: greentic-integration — Update fixtures/tests for `ChannelMessageEnvelope` (`from` + `to[]`)

## Summary
Update integration tests and fixtures to match new envelope shape:
- remove `user_id`
- add `from` (Actor) where sender attribution matters
- add `to: vec![]` (or sample destination) where required

## Steps

1) Search & replace:
- Find all struct literals `ChannelMessageEnvelope { ... user_id: ... }`
- Replace with `from: Some(Actor { id: ..., kind: Some("user".into()) })` or `from: None`.

2) Ensure all envelope constructors include:
- `to: vec![]` unless test is about outbound send.

3) Update any JSON fixtures/snapshots that contain `user_id` or lack `from/to`.

## Commands
```bash
cargo fmt
cargo test -p greentic-integration -- --nocapture
```

## Acceptance criteria
- All integration tests compile and pass
- Fixtures updated to new schema shape
