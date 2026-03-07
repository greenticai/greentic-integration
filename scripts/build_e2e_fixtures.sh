#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${GREENTIC_E2E_FIXTURES_DIR:-${ROOT_DIR}/fixtures/packs}"

CONTROL_CHAIN_PATH="${GREENTIC_CONTROL_CHAIN_PATH:-../greentic-control-chain}"
FAST2FLOW_PATH="${GREENTIC_FAST2FLOW_PATH:-../greentic-fast2flow}"

CONTROL_CHAIN_GTPACK="${GREENTIC_CONTROL_CHAIN_GTPACK:-}"
FAST2FLOW_GTPACK="${GREENTIC_FAST2FLOW_GTPACK:-}"

mkdir -p "${OUT_DIR}"

log() {
  printf '[build_e2e_fixtures] %s\n' "$*"
}

find_gtpack_in_repo() {
  local repo="$1"
  local preferred="$2"
  local first
  if [[ -f "${preferred}" ]]; then
    printf '%s\n' "${preferred}"
    return 0
  fi
  if [[ ! -d "${repo}" ]]; then
    return 1
  fi
  first="$(find "${repo}" -maxdepth 4 -type f -name '*.gtpack' | sort | head -n 1 || true)"
  if [[ -n "${first}" ]]; then
    printf '%s\n' "${first}"
    return 0
  fi
  return 1
}

resolve_control_chain() {
  if [[ -n "${CONTROL_CHAIN_GTPACK}" ]]; then
    printf '%s\n' "${CONTROL_CHAIN_GTPACK}"
    return 0
  fi

  local preferred="${CONTROL_CHAIN_PATH}/dist/routing-ingress-control-chain.gtpack"
  if find_gtpack_in_repo "${CONTROL_CHAIN_PATH}" "${preferred}"; then
    return 0
  fi

  if [[ -x "${CONTROL_CHAIN_PATH}/build/build_gtpack.sh" ]]; then
    log "building control-chain fixture via ${CONTROL_CHAIN_PATH}/build/build_gtpack.sh"
    if (cd "${CONTROL_CHAIN_PATH}" && bash build/build_gtpack.sh); then
      :
    else
      log "warn: control-chain build script failed; continuing with artifact discovery"
    fi
  fi

  find_gtpack_in_repo "${CONTROL_CHAIN_PATH}" "${preferred}"
}

resolve_fast2flow() {
  if [[ -n "${FAST2FLOW_GTPACK}" ]]; then
    printf '%s\n' "${FAST2FLOW_GTPACK}"
    return 0
  fi

  local preferred="${FAST2FLOW_PATH}/dist/fast2flow.gtpack"
  if find_gtpack_in_repo "${FAST2FLOW_PATH}" "${preferred}"; then
    return 0
  fi

  if [[ -x "${FAST2FLOW_PATH}/ci/build_gtpack.sh" ]]; then
    log "building fast2flow fixture via ${FAST2FLOW_PATH}/ci/build_gtpack.sh"
    if (cd "${FAST2FLOW_PATH}" && bash ci/build_gtpack.sh dist); then
      :
    else
      log "warn: fast2flow build script failed; continuing with artifact discovery"
    fi
  fi

  find_gtpack_in_repo "${FAST2FLOW_PATH}" "${preferred}"
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
log "control-chain path: ${CONTROL_CHAIN_PATH}"
log "fast2flow path: ${FAST2FLOW_PATH}"
log "flows fixture: ${ROOT_DIR}/fixtures/fast2flow/demo_app_flows.json"

control_src="$(resolve_control_chain || true)"
if [[ -z "${control_src}" ]]; then
  log "error: could not resolve control-chain fixture. Set GREENTIC_CONTROL_CHAIN_GTPACK or GREENTIC_CONTROL_CHAIN_PATH."
  exit 1
fi

fast2flow_src="$(resolve_fast2flow || true)"
if [[ -z "${fast2flow_src}" ]]; then
  log "error: could not resolve fast2flow fixture. Set GREENTIC_FAST2FLOW_GTPACK or GREENTIC_FAST2FLOW_PATH."
  exit 1
fi

require_file "${control_src}" "control-chain"
require_file "${fast2flow_src}" "fast2flow"

cp "${control_src}" "${OUT_DIR}/control-chain.gtpack"
cp "${fast2flow_src}" "${OUT_DIR}/fast2flow.gtpack"

log "wrote: ${OUT_DIR}/control-chain.gtpack"
log "wrote: ${OUT_DIR}/fast2flow.gtpack"
