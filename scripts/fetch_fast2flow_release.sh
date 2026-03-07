#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_ROOT="${GREENTIC_FAST2FLOW_RELEASE_DIR:-${ROOT_DIR}/artifacts/fast2flow-release}"
REPO="${GREENTIC_FAST2FLOW_GH_REPO:-greentic-biz/greentic-fast2flow}"

mkdir -p "${OUT_ROOT}"

log() {
  printf '[fetch_fast2flow_release] %s\n' "$*"
}

need() {
  command -v "$1" >/dev/null 2>&1 || {
    log "error: missing required command '$1'"
    exit 1
  }
}

need gh
need tar
need uname

arch="$(uname -m)"
os="$(uname -s)"

case "${arch}" in
  x86_64|amd64) target_arch="x86_64" ;;
  aarch64|arm64) target_arch="aarch64" ;;
  *)
    log "error: unsupported architecture '${arch}'"
    exit 1
    ;;
esac

case "${os}" in
  Linux) target_os="unknown-linux-gnu"; ext="tar.gz" ;;
  Darwin) target_os="apple-darwin"; ext="tar.gz" ;;
  *)
    log "error: unsupported OS '${os}'"
    exit 1
    ;;
esac

tag="$(gh release view --repo "${REPO}" --json tagName -q .tagName)"
version="${tag#v}"
target="${target_arch}-${target_os}"
release_dir="${OUT_ROOT}/${tag}"
latest_dir="${OUT_ROOT}/latest"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

cli_asset="greentic-fast2flow-${tag}-${target}.${ext}"
host_asset="greentic-fast2flow-routing-host-${tag}-${target}.${ext}"

log "repo: ${REPO}"
log "tag: ${tag}"
log "target: ${target}"
log "output: ${release_dir}"

mkdir -p "${release_dir}"

gh release download "${tag}" \
  --repo "${REPO}" \
  --dir "${tmp_dir}" \
  --pattern "${cli_asset}" \
  --pattern "${host_asset}"

tar -xzf "${tmp_dir}/${cli_asset}" -C "${release_dir}"
tar -xzf "${tmp_dir}/${host_asset}" -C "${release_dir}"

mkdir -p "${latest_dir}"
cp "${release_dir}/greentic-fast2flow" "${latest_dir}/greentic-fast2flow"
cp "${release_dir}/greentic-fast2flow-routing-host" "${latest_dir}/greentic-fast2flow-routing-host"
chmod +x "${latest_dir}/greentic-fast2flow" "${latest_dir}/greentic-fast2flow-routing-host"

cat > "${latest_dir}/env.sh" <<EOF
export GREENTIC_FAST2FLOW_CLI_BIN="${latest_dir}/greentic-fast2flow"
export GREENTIC_FAST2FLOW_HOST_BIN="${latest_dir}/greentic-fast2flow-routing-host"
export GREENTIC_FAST2FLOW_RELEASE_VERSION="${version}"
EOF

log "ready: ${latest_dir}/greentic-fast2flow"
log "ready: ${latest_dir}/greentic-fast2flow-routing-host"
log "env file: ${latest_dir}/env.sh"
