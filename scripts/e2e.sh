#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/e2e.sh <tier> [--focus <pattern>]

Tiers:
  l0   smoke-level E2E (fast)
  l1   core E2E (pack lifecycle, config)
  l2   infra/stack and isolation

Examples:
  ./scripts/e2e.sh l1
  ./scripts/e2e.sh l2 --focus e2e_multi_tenant_isolation

Set E2E_KEEP=1 to retain target/e2e artifacts between runs.
EOF
}

if [[ ${1:-} == "-h" || ${1:-} == "--help" || $# -lt 1 ]]; then
  usage
  exit 0
fi

tier="$1"; shift
focus=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --focus)
      focus="$2"
      shift 2
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

tests_l0=("e2e_smoke" "e2e_scenario_smoke")
tests_l1=(
  "${tests_l0[@]}"
  "e2e_retry_backoff_flaky_tool"
  "e2e_config_precedence"
  "e2e_pack_lifecycle"
  "e2e_wrapper_canonical_parity"
  "pr14_provider_core_flows_and_index"
  "pr14_provider_core_schema_onboarding"
)
tests_l2=("${tests_l1[@]}" "e2e_infra" "e2e_stack_boot" "e2e_multi_tenant_isolation")

select_tests() {
  case "$tier" in
    l0) printf "%s " "${tests_l0[@]}" ;;
    l1) printf "%s " "${tests_l1[@]}" ;;
    l2) printf "%s " "${tests_l2[@]}" ;;
    *) echo "Unknown tier: $tier" >&2; usage; exit 2 ;;
  esac
}

tests=()
if [[ -n "$focus" ]]; then
  tests+=("$focus")
else
  read -ra tests <<<"$(select_tests)"
fi

if [[ "${E2E_KEEP:-0}" != "1" && -d target/e2e ]]; then
  rm -rf target/e2e
fi

export RUST_LOG=${RUST_LOG:-info}
export GREENTIC_PROVIDER_CORE_ONLY=1
echo "Running E2E tier '$tier' (focus: ${focus:-none})"
echo "Tests: ${tests[*]}"

status=0
for t in "${tests[@]}"; do
  echo "==> cargo test -p greentic-integration $t"
  if ! cargo test -p greentic-integration "$t"; then
    status=1
    echo "Test failed: $t"
    break
  fi
done

artifact_dir="target/e2e"
if [[ -d "$artifact_dir" ]]; then
  echo "E2E artifacts: $artifact_dir"
  find "$artifact_dir" -maxdepth 2 -type f | sed 's/^/  /'
else
  echo "No artifacts found (maybe tests skipped)."
fi

exit $status
