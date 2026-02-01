#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

log() {
  printf "\n[%s] %s\n" "$(date -u +%H:%M:%S)" "$*"
}

run_step() {
  local description=$1
  shift
  log "➡️  ${description}"
  "$@"
}

SERVER_PID=""
SERVER_URL="${SERVER_URL:-http://127.0.0.1:18080}"
SERVER_LOG="${ROOT_DIR}/.logs/ci-server.log"
SERVER_AVAILABLE=0
BIN_OVERRIDE="${ROOT_DIR}/target/debug/greentic-integration"

cleanup() {
  if [[ -n "${SERVER_PID}" ]] && kill -0 "${SERVER_PID}" 2>/dev/null; then
    log "Shutting down greentic-integration server (pid=${SERVER_PID})"
    kill "${SERVER_PID}" 2>/dev/null || true
    wait "${SERVER_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

start_server() {
  mkdir -p "${ROOT_DIR}/.logs"
  log "Starting greentic-integration server for demo replays"
  GREENTIC_SERVER__LISTEN_ADDR="${SERVER_URL#http://}" \
    "${CARGO_CMD[@]}" run -p greentic-integration -- serve >"${SERVER_LOG}" 2>&1 &
  SERVER_PID=$!
  for attempt in {1..30}; do
    if curl -fsS "${SERVER_URL}/healthz" >/dev/null 2>&1; then
      log "greentic-integration server is ready on ${SERVER_URL} (pid=${SERVER_PID})"
      SERVER_AVAILABLE=1
      return 0
    fi
    sleep 1
  done
  log "warn: greentic-integration server did not become ready; tailing log and falling back to stub"
  tail -n 50 "${SERVER_LOG}" || true
  cleanup
  SERVER_AVAILABLE=0
  return 0
}

TOOLCHAIN="${RUST_TOOLCHAIN:-1.90.0}"
if ! rustup toolchain list | grep -q "${TOOLCHAIN}"; then
  log "error: rustup toolchain '${TOOLCHAIN}' not installed. Run 'rustup toolchain install ${TOOLCHAIN} --profile minimal --component clippy --component rustfmt'."
  exit 1
fi

CARGO_CMD=(cargo "+${TOOLCHAIN}")

# Enable provider-core E2E tests by default for local check runs.
export GREENTIC_PROVIDER_CORE_ONLY="${GREENTIC_PROVIDER_CORE_ONLY:-1}"

run_step "cargo fmt" "${CARGO_CMD[@]}" fmt -- --check
run_step "cargo clippy" "${CARGO_CMD[@]}" clippy --all-targets --all-features -- -D warnings
run_step "cargo test" "${CARGO_CMD[@]}" test --workspace
run_step "make packs.test" make packs.test
run_step "make render.snapshot" make render.snapshot
run_step "make runner.smoke" make runner.smoke
run_step "make webchat.contract" make webchat.contract
run_step "make webchat.e2e" make webchat.e2e
run_step "start greentic-integration server" start_server
if [[ "${SERVER_AVAILABLE}" -eq 1 ]]; then
  run_step "make demo.replay.build (server mode)" env BIN="${BIN_OVERRIDE}" USE_SERVER=1 SERVER="${SERVER_URL}" make demo.replay.build
  run_step "make demo.replay.chat (server mode)" env BIN="${BIN_OVERRIDE}" USE_SERVER=1 SERVER="${SERVER_URL}" make demo.replay.chat
else
  log "Server unavailable; falling back to stub replay targets"
  run_step "make demo.replay.build (stub mode)" env BIN="${BIN_OVERRIDE}" make demo.replay.build
  run_step "make demo.replay.chat (stub mode)" env BIN="${BIN_OVERRIDE}" make demo.replay.chat
fi

log "✅ Local checks completed successfully."
