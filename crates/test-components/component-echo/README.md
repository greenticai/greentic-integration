# component-echo

`component-echo` is a deterministic messaging component used in the greentic-integration suite.
It simply echoes back the JSON payload it receives over the `messaging.send` and `messaging.ingress` operations.

## Building

```bash
greentic-component build --path crates/test-components/component-echo
```

## Validating

```bash
greentic-component doctor --path crates/test-components/component-echo
```

## Testing

```bash
cargo test --package component-echo
```

The component expects valid JSON messaging envelopes (e.g., the sample under
`fixtures/inputs/channel_message.json`) and returns them verbatim.
