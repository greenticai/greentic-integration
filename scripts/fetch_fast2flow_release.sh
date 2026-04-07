#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_ROOT="${GREENTIC_FAST2FLOW_RELEASE_DIR:-${ROOT_DIR}/artifacts/fast2flow-release}"
REPO="${GREENTIC_FAST2FLOW_GH_REPO:-greentic-biz/greentic-fast2flow}"
REQUIRE_GTPACK="${GREENTIC_FAST2FLOW_REQUIRE_GTPACK:-0}"

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
need python3

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

assets_json="${tmp_dir}/assets.json"
gh release view --repo "${REPO}" --json assets > "${assets_json}"

readarray -t asset_names < <(
  python3 - "${assets_json}" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as fh:
    data = json.load(fh)

for asset in data.get("assets", []):
    name = asset.get("name")
    if name:
        print(name)
PY
)

pick_asset() {
  local kind="$1"
  python3 - "$kind" "$target" "$ext" "${asset_names[@]}" <<'PY'
import sys

kind = sys.argv[1]
target = sys.argv[2]
ext = sys.argv[3]
assets = sys.argv[4:]

def choose(candidates):
    return sorted(candidates)[-1] if candidates else ""

if kind == "cli":
    chosen = choose(
        [
            a for a in assets
            if a.startswith("greentic-fast2flow-")
            and a.endswith("." + ext)
            and target in a
            and "routing-host" not in a
        ]
    )
elif kind == "host":
    chosen = choose(
        [
            a for a in assets
            if a.startswith("greentic-fast2flow-routing-host-")
            and a.endswith("." + ext)
            and target in a
        ]
    )
elif kind == "gtpack":
    preferred = [
        a for a in assets
        if a.endswith(".gtpack")
        and ("fast2flow" in a.lower())
    ]
    chosen = choose(preferred)
else:
    chosen = ""

if chosen:
    print(chosen)
PY
}

cli_asset="$(pick_asset cli)"
host_asset="$(pick_asset host)"
gtpack_asset="$(pick_asset gtpack)"

if [[ -z "${cli_asset}" ]]; then
  log "error: could not find greentic-fast2flow release asset for target ${target}"
  exit 1
fi

if [[ -z "${host_asset}" ]]; then
  log "error: could not find greentic-fast2flow-routing-host release asset for target ${target}"
  exit 1
fi

if [[ "${REQUIRE_GTPACK}" == "1" && -z "${gtpack_asset}" ]]; then
  log "error: release ${tag} does not contain a fast2flow .gtpack asset"
  exit 1
fi

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

if [[ -n "${gtpack_asset}" ]]; then
  gh release download "${tag}" \
    --repo "${REPO}" \
    --dir "${tmp_dir}" \
    --pattern "${gtpack_asset}"
fi

tar -xzf "${tmp_dir}/${cli_asset}" -C "${release_dir}"
tar -xzf "${tmp_dir}/${host_asset}" -C "${release_dir}"

mkdir -p "${latest_dir}"
cp "${release_dir}/greentic-fast2flow" "${latest_dir}/greentic-fast2flow"
cp "${release_dir}/greentic-fast2flow-routing-host" "${latest_dir}/greentic-fast2flow-routing-host"
chmod +x "${latest_dir}/greentic-fast2flow" "${latest_dir}/greentic-fast2flow-routing-host"

if [[ -n "${gtpack_asset}" ]]; then
  cp "${tmp_dir}/${gtpack_asset}" "${release_dir}/fast2flow.gtpack"
  cp "${tmp_dir}/${gtpack_asset}" "${latest_dir}/fast2flow.gtpack"
fi

cat > "${latest_dir}/env.sh" <<EOF
export GREENTIC_FAST2FLOW_CLI_BIN="${latest_dir}/greentic-fast2flow"
export GREENTIC_FAST2FLOW_HOST_BIN="${latest_dir}/greentic-fast2flow-routing-host"
export GREENTIC_FAST2FLOW_GTPACK="${latest_dir}/fast2flow.gtpack"
export GREENTIC_FAST2FLOW_RELEASE_VERSION="${version}"
EOF

log "ready: ${latest_dir}/greentic-fast2flow"
log "ready: ${latest_dir}/greentic-fast2flow-routing-host"
if [[ -n "${gtpack_asset}" ]]; then
  log "ready: ${latest_dir}/fast2flow.gtpack"
else
  log "warn: no fast2flow gtpack asset found in release ${tag}"
fi
log "env file: ${latest_dir}/env.sh"
