#!/usr/bin/env bash
set -euo pipefail

PACK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLOW="$PACK_DIR/flows/main.ygtc"
FLOW_DIR="$PACK_DIR/flows"
WASM_REL="../components/ai.greentic.component-echo/component_echo.wasm"

cd "$FLOW_DIR"

greentic-flow new \
  --flow "$FLOW" \
  --id echo \
  --type messaging \
  --force

greentic-flow add-step \
  --flow "$FLOW" \
  --node-id echo \
  --local-wasm "$WASM_REL" \
  --operation messaging.send \
  --payload '{"channel":"webchat","id":"echo-1","metadata":{"trace_id":"echo-trace"},"session_id":"sess-echo","text":"Echo test"}' \
  --routing-out

# Translate the generated operator node into a component node shape expected by pack tooling.
tmp_file="$(mktemp)"
awk '
  $0 ~ /^    messaging\.send:$/ {
    print "    ai.greentic.component-echo:"
    print "      operation: messaging.send"
    next
  }
  $0 ~ /^      component: / { next }
  { print }
' "$FLOW" > "$tmp_file"
mv "$tmp_file" "$FLOW"

greentic-flow bind-component \
  --flow "$FLOW" \
  --step echo \
  --local-wasm "$WASM_REL" \
  --write
