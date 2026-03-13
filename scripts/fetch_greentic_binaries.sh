#!/usr/bin/env bash
set -euo pipefail

# Fetch pinned or latest greentic binaries (runner/deployer/store) for linux-x86_64 with checksum verification.
# Environment:
#   GREENTIC_RUNNER_VERSION / GREENTIC_DEPLOYER_VERSION / GREENTIC_STORE_VERSION (optional; tag, defaults to latest)
#   GREENTIC_RUNNER_URL / GREENTIC_RUNNER_SHA256 (optional pin overrides; if set, skip GitHub API)
#   ... same for DEPLOYER/STORE
#   CI / GREENTIC_STACK_STRICT / GREENTIC_INTEGRATION_STRICT (strict mode -> fail on missing assets/checksums)
#   GITHUB_TOKEN (optional to avoid rate limits)

OWNER="greenticai"
ARCH="x86_64-unknown-linux-gnu"
BIN_DIR="$(cd "$(dirname "$0")/.." && pwd)/tests/bin/linux-x86_64"
mkdir -p "$BIN_DIR"

strict=${CI:-0}
if [[ "${GREENTIC_STACK_STRICT:-}" == "1" || "${GREENTIC_INTEGRATION_STRICT:-}" == "1" ]]; then
  strict=1
fi

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || { echo "ERROR: missing required tool '$1'" >&2; exit 1; }
}
require_cmd curl
require_cmd jq
require_cmd sha256sum
require_cmd tar

GITHUB_TOKEN="${GREENTIC_GITHUB_TOKEN:-${GITHUB_TOKEN:-}}"

declare -a auth_args=()
[[ -n "${GITHUB_TOKEN:-}" ]] && auth_args=(-H "Authorization: Bearer ${GITHUB_TOKEN}")

curl_auth() {
  # Expand auth_args only when set to avoid empty args under nounset.
  if [[ ${#auth_args[@]} -gt 0 ]]; then
    curl -fsSL "${auth_args[@]}" "$@"
  else
    curl -fsSL "$@"
  fi
}

resolve_latest() {
  local repo="$1" binary="$2" tag="$3"
  local api="https://api.github.com/repos/${OWNER}/${repo}/releases"
  if [[ "$tag" != "latest" ]]; then
    api="${api}/tags/${tag}"
  else
    api="${api}/latest"
  fi
  local release
  if ! release=$(curl_auth "$api"); then
    echo "ERROR: failed to fetch release info for ${repo} (tag=${tag}); ensure GITHUB_TOKEN can access ${OWNER}/${repo} or provide explicit URLs." >&2
    return 1
  fi
  local resolved_tag asset_name asset_url asset_digest sums_name sums_url asset_id sums_id
  resolved_tag=$(echo "$release" | jq -r 'if type=="array" then .[0].tag_name else .tag_name end')
  asset_name=$(echo "$release" | jq -r --arg arch "$ARCH" '.assets[] | select(.name | test($arch)) | .name' | head -n1)
  asset_url=$(echo "$release" | jq -r --arg arch "$ARCH" '.assets[] | select(.name | test($arch)) | .browser_download_url' | head -n1)
  asset_id=$(echo "$release" | jq -r --arg arch "$ARCH" '.assets[] | select(.name | test($arch)) | (.id | tostring)' | head -n1)
  asset_digest=$(echo "$release" | jq -r --arg arch "$ARCH" '.assets[] | select(.name | test($arch)) | (.digest // empty)' | head -n1)
  if [[ -z "$asset_name" || -z "$asset_url" ]]; then
    # Fallback to any linux/x86 asset if ARCH-specific not found.
    asset_name=$(echo "$release" | jq -r '.assets[] | select(.name | test("linux|x86_64|amd64")) | .name' | head -n1)
    asset_url=$(echo "$release" | jq -r '.assets[] | select(.name | test("linux|x86_64|amd64")) | .browser_download_url' | head -n1)
    asset_id=$(echo "$release" | jq -r '.assets[] | select(.name | test("linux|x86_64|amd64")) | (.id | tostring)' | head -n1)
    asset_digest=$(echo "$release" | jq -r '.assets[] | select(.name | test("linux|x86_64|amd64")) | (.digest // empty)' | head -n1)
  fi
  # Construct a browser URL if missing but we have a tag and asset name.
  if [[ -z "$asset_url" && -n "$asset_name" && -n "$resolved_tag" ]]; then
    asset_url="https://github.com/${OWNER}/${repo}/releases/download/${resolved_tag}/${asset_name}"
  fi
  # Final best-effort guesses for common naming schemes when API omits URLs.
  if [[ -z "$asset_url" && -n "$resolved_tag" ]]; then
    for guess in \
      "${repo}-${resolved_tag}-${ARCH}.tar.gz" \
      "${repo}-${resolved_tag}-${ARCH}.tgz" \
      "${repo}-${ARCH}.tar.gz" \
      "${repo}-${ARCH}.tgz"
    do
      asset_name="$guess"
      asset_url="https://github.com/${OWNER}/${repo}/releases/download/${resolved_tag}/${guess}"
      break
    done
  fi
  if [[ -z "$asset_url" && -n "$asset_id" ]]; then
    # Browser URL missing but API asset id present; construct API URL placeholder.
    asset_url="https://api.github.com/repos/${OWNER}/${repo}/releases/assets/${asset_id}"
  fi
  # Prefer SHA256SUMS, fall back to *.sha256 checksum assets.
  sums_name=$(echo "$release" | jq -r '[ .assets[] | select(.name | test("SHA256SUMS|\\.sha256$")) | .name ][0] // empty')
  sums_url=$(echo "$release" | jq -r '[ .assets[] | select(.name | test("SHA256SUMS|\\.sha256$")) | .browser_download_url ][0] // empty')
  sums_id=$(echo "$release" | jq -r '[ .assets[] | select(.name | test("SHA256SUMS|\\.sha256$")) | (.id | tostring) ][0] // empty')
  if [[ -z "$asset_name" || ( -z "$asset_url" && -z "$asset_id" ) ]]; then
    echo "ERROR: missing asset for ${repo} tag=${resolved_tag}" >&2
    echo "Available assets:" >&2
    echo "$release" | jq -r '.assets[] | "  \(.name)"' >&2
    return 1
  fi
  jq -n --arg resolved_tag "$resolved_tag" \
        --arg asset_name "$asset_name" \
        --arg asset_url "$asset_url" \
        --arg sums_name "$sums_name" \
        --arg sums_url "$sums_url" \
        --arg asset_digest "$asset_digest" \
        --arg asset_id "$asset_id" \
        --arg sums_id "$sums_id" \
        '{resolved_tag:$resolved_tag, asset_name:$asset_name, asset_url:$asset_url, sums_name:$sums_name, sums_url:$sums_url, asset_digest:$asset_digest, asset_id:$asset_id, sums_id:$sums_id}'
}

download_and_verify() {
  local binary="$1" repo="$2" tag_var="$3" url_override="$4" sha_override="$5"

  local dest_bin="$BIN_DIR/${binary}"
  if [[ -x "$dest_bin" ]]; then
    echo "Found existing ${dest_bin}; skipping download"
    echo "${binary} preexisting" >> "$BIN_DIR/resolved_versions.txt"
    return
  fi

  local resolved_tag asset_name asset_url sums_name sums_url env_prefix resolved_output asset_digest asset_id sums_id
  env_prefix="$(echo "${binary//-/_}" | tr '[:lower:]' '[:upper:]')"
  if [[ -n "$url_override" && -n "$sha_override" ]]; then
    asset_name="$(basename "$url_override")"
    asset_url="$url_override"
    sums_name=""
    sums_url=""
    resolved_tag="${!tag_var:-override}"
  else
    local tag="${!tag_var:-latest}"
    if ! resolved_output=$(resolve_latest "$repo" "$binary" "$tag"); then
      echo "ERROR: could not resolve ${binary} (repo=${repo}, tag=${tag})." >&2
      echo "Hint: set ${env_prefix}_URL and ${env_prefix}_SHA256, or drop the binary at ${dest_bin}." >&2
      if [[ "$strict" == "1" ]]; then
        exit 1
      fi
      echo "Skipping ${binary} because strict mode is off." >&2
      return
    fi
    resolved_tag=$(echo "$resolved_output" | jq -r '.resolved_tag')
    asset_name=$(echo "$resolved_output" | jq -r '.asset_name')
    asset_url=$(echo "$resolved_output" | jq -r '.asset_url')
    sums_name=$(echo "$resolved_output" | jq -r '.sums_name')
    sums_url=$(echo "$resolved_output" | jq -r '.sums_url')
    asset_digest=$(echo "$resolved_output" | jq -r '.asset_digest')
    asset_id=$(echo "$resolved_output" | jq -r '.asset_id')
    sums_id=$(echo "$resolved_output" | jq -r '.sums_id')
    [[ "$asset_url" == "null" ]] && asset_url=""
    [[ "$asset_name" == "null" ]] && asset_name=""
    [[ "$asset_id" == "null" ]] && asset_id=""
    [[ "$sums_url" == "null" ]] && sums_url=""
    [[ "$sums_name" == "null" ]] && sums_name=""
    [[ "$asset_digest" == "null" ]] && asset_digest=""
    [[ "$sums_id" == "null" ]] && sums_id=""
  fi

  echo "DEBUG ${binary}: resolved_tag=${resolved_tag} asset_name=${asset_name} asset_url=${asset_url} asset_id=${asset_id} digest=${asset_digest} sums_url=${sums_url}" >&2

  if [[ -z "${asset_url:-}" && -z "${asset_id:-}" ]]; then
    if [[ "$strict" == "1" ]]; then
      echo "ERROR: asset URL missing for ${binary}" >&2
      exit 1
    fi
    echo "Skipping ${binary}: asset URL missing" >&2
    return
  fi

  local download_path="$BIN_DIR/${asset_name}"
  if [[ -n "${asset_id:-}" ]]; then
    echo "Downloading ${binary} (${resolved_tag}) via asset API id=${asset_id}"
  else
    echo "Downloading ${binary} (${resolved_tag}) from ${asset_url}"
  fi
  if [[ -n "${asset_id:-}" ]]; then
    if ! curl_auth -H "Accept: application/octet-stream" -L "https://api.github.com/repos/${OWNER}/${repo}/releases/assets/${asset_id}" -o "$download_path"; then
      echo "WARN: asset API download failed, retrying browser URL" >&2
      curl_auth "$asset_url" -o "$download_path"
    fi
  else
    curl_auth "$asset_url" -o "$download_path"
  fi

  if [[ -n "$sha_override" ]]; then
    echo "Verifying SHA256 (override) for ${binary}"
    echo "${sha_override}  ${download_path}" | sha256sum -c -
  elif [[ -n "${sums_url:-}" ]]; then
    echo "Fetching checksums ${sums_url}"
    local sums_path="$BIN_DIR/${sums_name}"
    if [[ -n "${sums_id:-}" ]]; then
      if ! curl_auth -H "Accept: application/octet-stream" -L "https://api.github.com/repos/${OWNER}/${repo}/releases/assets/${sums_id}" -o "$sums_path"; then
        echo "WARN: checksum asset API download failed, retrying browser URL" >&2
        curl_auth "$sums_url" -o "$sums_path"
      fi
    else
      curl_auth "$sums_url" -o "$sums_path"
    fi
    (cd "$BIN_DIR" && sha256sum -c "$sums_name" --ignore-missing)
  elif [[ -n "${asset_digest:-}" ]]; then
    echo "Verifying SHA256 (asset digest) for ${binary}"
    echo "${asset_digest#sha256:}  ${download_path}" | sha256sum -c -
  else
    if [[ "$strict" == "1" ]]; then
      echo "ERROR: no checksum available for ${binary} in strict mode" >&2
      echo "Hint: set ${env_prefix}_URL and ${env_prefix}_SHA256, or upload a checksum asset for tag ${resolved_tag}." >&2
      exit 1
    fi
    echo "WARN: skipping checksum for ${binary}" >&2
  fi

  # Extract if tarball, else copy
  if [[ "$download_path" == *.tar.gz || "$download_path" == *.tgz ]]; then
    echo "Extracting ${download_path}"
    tar -xzf "$download_path" -C "$BIN_DIR"
    # Try to locate binary inside extracted contents
    local found
    found=$(find "$BIN_DIR" -maxdepth 2 -type f -name "${binary}" | head -n1)
    if [[ -n "$found" ]]; then
      mv "$found" "$dest_bin"
    fi
  else
    cp "$download_path" "$dest_bin"
  fi
  chmod +x "$dest_bin" || true
  echo "${binary} ${resolved_tag}" >> "$BIN_DIR/resolved_versions.txt"
}

download_and_verify "greentic-runner" "greentic-runner" "GREENTIC_RUNNER_VERSION" "${GREENTIC_RUNNER_URL:-}" "${GREENTIC_RUNNER_SHA256:-}"
download_and_verify "greentic-deployer" "greentic-deployer" "GREENTIC_DEPLOYER_VERSION" "${GREENTIC_DEPLOYER_URL:-}" "${GREENTIC_DEPLOYER_SHA256:-}"
download_and_verify "greentic-store" "greentic-store" "GREENTIC_STORE_VERSION" "${GREENTIC_STORE_URL:-}" "${GREENTIC_STORE_SHA256:-}"

echo "Binaries available under $BIN_DIR"

