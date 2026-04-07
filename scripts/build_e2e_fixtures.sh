#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${GREENTIC_E2E_FIXTURES_DIR:-${ROOT_DIR}/fixtures/packs}"
CONTROL_CHAIN_GTPACK="${GREENTIC_CONTROL_CHAIN_GTPACK:-}"
FAST2FLOW_GTPACK="${GREENTIC_FAST2FLOW_GTPACK:-}"
LOCAL_FALLBACK_GTPACK="${ROOT_DIR}/crates/test-packs/echo-pack/dist/echo-pack.gtpack"

mkdir -p "${OUT_DIR}"

log() {
  printf '[build_e2e_fixtures] %s\n' "$*"
}

resolve_control_chain() {
  if [[ -n "${CONTROL_CHAIN_GTPACK}" ]]; then
    printf '%s\n' "${CONTROL_CHAIN_GTPACK}"
    return 0
  fi
  printf '%s\n' "${LOCAL_FALLBACK_GTPACK}"
}

resolve_fast2flow() {
  if [[ -n "${FAST2FLOW_GTPACK}" ]]; then
    printf '%s\n' "${FAST2FLOW_GTPACK}"
    return 0
  fi
  printf '%s\n' "${LOCAL_FALLBACK_GTPACK}"
}

require_file() {
  local path="$1"
  local label="$2"
  if [[ ! -f "${path}" ]]; then
    log "error: ${label} fixture not found at ${path}"
    exit 1
  fi
}

log "output dir: ${OUT_DIR}"
log "control-chain fixture override: ${CONTROL_CHAIN_GTPACK:-<local fallback>}"
log "fast2flow fixture override: ${FAST2FLOW_GTPACK:-<local fallback>}"
log "flows fixture: ${ROOT_DIR}/fixtures/fast2flow/demo_app_flows.json"

control_src="$(resolve_control_chain || true)"
if [[ -z "${control_src}" ]]; then
  log "error: could not resolve control-chain fixture. Set GREENTIC_CONTROL_CHAIN_GTPACK."
  exit 1
fi

fast2flow_src="$(resolve_fast2flow || true)"
if [[ -z "${fast2flow_src}" ]]; then
  log "error: could not resolve fast2flow fixture. Set GREENTIC_FAST2FLOW_GTPACK."
  exit 1
fi

require_file "${control_src}" "control-chain"
require_file "${fast2flow_src}" "fast2flow"

cp "${control_src}" "${OUT_DIR}/control-chain.gtpack"
cp "${fast2flow_src}" "${OUT_DIR}/fast2flow.gtpack"

log "wrote: ${OUT_DIR}/control-chain.gtpack"
log "wrote: ${OUT_DIR}/fast2flow.gtpack"
