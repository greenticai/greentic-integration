# Cross-Repo Pinning

Use `scripts/pin_repo.sh` to generate a `[patch]` override inside `.cargo/config.toml` for
testing unreleased changes from other Greentic repositories.

```bash
./scripts/pin_repo.sh greenticai/greentic-messaging <sha>
cargo update -p greentic-messaging
```

The script rewrites `.cargo/config.toml` to include a dedicated `[patch."https://github.com/..."]`
section and prints the follow-up `cargo update` command. To drop the override, remove the
section from the config or pin to a different SHA.

