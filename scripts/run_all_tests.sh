#!/usr/bin/env bash
set -u

TARGETS=(
  packs.test
  app.test
  runner.smoke
  render.snapshot
  webchat.contract
  webchat.e2e
)

fail=0
summary=()

for t in "${TARGETS[@]}"; do
  echo "==> running $t"
  if make "$t"; then
    summary+=("✅ $t")
  else
    summary+=("❌ $t")
    fail=1
  fi
done

echo ""
echo "Summary:"
for line in "${summary[@]}"; do
  echo "  $line"
done

exit $fail
