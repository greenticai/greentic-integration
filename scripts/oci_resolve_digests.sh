#!/usr/bin/env bash
set -euo pipefail

MANIFEST_FILE=${PACKS_MANIFEST:-tests/packs/manifest.txt}
OUT=${OUT:-target/packs_resolved.json}
GHCR_ORG=${GHCR_ORG:-greenticai}
ALLOW_LATEST=${ALLOW_LATEST:-1}

usage() {
  cat <<'USAGE'
Usage: PACKS_MANIFEST=tests/packs/manifest.txt OUT=target/packs_resolved.json ./scripts/oci_resolve_digests.sh

Each line in PACKS_MANIFEST should be <name>[:tag]. Tags are required.

Resolution order:
  1) crane digest ghcr.io/${GHCR_ORG}/<name>:<tag>
  2) skopeo inspect docker://ghcr.io/${GHCR_ORG}/<name>:<tag> | jq -r .Digest
Options:
  ALLOW_LATEST=1   permit missing tags or :latest and resolve to the most recent tag (default: 1).
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ ! -f "$MANIFEST_FILE" ]]; then
  echo "Manifest not found: $MANIFEST_FILE" >&2
  exit 1
fi

maybe_install_crane() {
  # Auto-install crane in CI or when INSTALL_CRANE=1 is set and Go is available.
  if command -v crane >/dev/null 2>&1; then
    return
  fi
  if [[ -z "${CI:-}" && "${INSTALL_CRANE:-0}" != "1" ]]; then
    return
  fi
  if ! command -v go >/dev/null 2>&1; then
    echo "crane not found and Go is unavailable; install crane or skopeo+jq" >&2
    return
  fi
  echo "Installing crane via 'go install' (auto-triggered; set INSTALL_CRANE=1 to opt-in locally)..." >&2
  local gobin
  gobin="${GOBIN:-$(go env GOPATH 2>/dev/null)/bin}"
  if GO111MODULE=on GOBIN="$gobin" go install github.com/google/go-containerregistry/cmd/crane@v0.20.1; then
    export PATH="$gobin:${PATH}"
    echo "crane installed to $gobin" >&2
  else
    echo "auto-install of crane failed; install crane or skopeo+jq manually" >&2
  fi
}

has_crane=0
if command -v crane >/dev/null 2>&1; then
  has_crane=1
fi
has_skopeo=0
if command -v skopeo >/dev/null 2>&1; then
  has_skopeo=1
fi
if [[ $has_crane -eq 0 && $has_skopeo -eq 0 ]]; then
  maybe_install_crane
  if command -v crane >/dev/null 2>&1; then
    has_crane=1
  fi
fi
if [[ $has_crane -eq 0 && $has_skopeo -eq 0 ]]; then
  echo "Need either 'crane' or 'skopeo' + jq on PATH to resolve digests (set INSTALL_CRANE=1 to auto-install crane with Go)" >&2
  exit 1
fi

resolve_latest_tag() {
  local name="$1"
  local repo="ghcr.io/${GHCR_ORG}/${name}"
  if [[ $has_crane -eq 1 ]]; then
    if tag=$(crane ls "$repo" 2>/dev/null | sort | tail -n1) && [[ -n "$tag" ]]; then
      echo "$tag"
      return 0
    fi
  fi
  if [[ $has_skopeo -eq 1 ]]; then
    if tag=$(skopeo list-tags "docker://$repo" 2>/dev/null | jq -r '.Tags[]' | sort | tail -n1) && [[ -n "$tag" ]]; then
      echo "$tag"
      return 0
    fi
  fi
  return 1
}

resolve_digest_for() {
  local name="$1"
  local tag="$2"
  local tried_latest=0

  while :; do
    local image="ghcr.io/${GHCR_ORG}/${name}:${tag}"
    local digest=""

    if [[ $has_crane -eq 1 ]]; then
      digest=$(crane digest "$image" 2>/dev/null || true)
    fi
    if [[ -z "$digest" && $has_skopeo -eq 1 ]]; then
      digest=$(skopeo inspect "docker://$image" 2>/dev/null | jq -r .Digest || true)
    fi

    if [[ -n "$digest" ]]; then
      echo "$tag|$digest"
      return 0
    fi

    if [[ "$ALLOW_LATEST" == "1" && $tried_latest -eq 0 ]]; then
      if resolved=$(resolve_latest_tag "$name"); then
        echo "Tag '$tag' failed for $name; resolved latest -> $resolved and retrying" >&2
        tag="$resolved"
        tried_latest=1
        continue
      fi
    fi

    echo "Failed to resolve digest for ghcr.io/${GHCR_ORG}/${name}:${tag} (check tag existence and registry auth; set DOCKER_CONFIG or login ghcr.io)" >&2
    return 1
  done
}

mkdir -p "$(dirname "$OUT")"
entries=()
while IFS= read -r line || [[ -n "$line" ]]; do
  line="${line%%#*}" # strip trailing comments
  line="${line%%[[:space:]]*}"
  if [[ -z "$line" ]]; then
    continue
  fi
  name_tag="$line"
  name="${name_tag%%:*}"
  tag="${name_tag#*:}"
  if [[ "$name_tag" == "$name" ]]; then
    if [[ "$ALLOW_LATEST" == "1" ]]; then
      if ! tag=$(resolve_latest_tag "$name"); then
        echo "Missing tag for $name and failed to resolve latest (line: $line)" >&2
        exit 1
      fi
      echo "Resolved latest tag for $name -> $tag" >&2
    else
      echo "Missing tag for $name (line: $line); set ALLOW_LATEST=1 to auto-resolve latest tag" >&2
      exit 1
    fi
  elif [[ "$ALLOW_LATEST" == "1" && "$tag" == "latest" ]]; then
    if ! tag=$(resolve_latest_tag "$name"); then
      echo "Tag 'latest' requested for $name but resolving latest failed" >&2
      exit 1
    fi
    echo "Resolved 'latest' for $name -> $tag" >&2
  fi
  resolved=$(resolve_digest_for "$name" "$tag") || exit 1
  resolved_tag="${resolved%%|*}"
  digest="${resolved#*|}"
  image="ghcr.io/${GHCR_ORG}/${name}:${resolved_tag}"
  oci_ref="${image%:*}@${digest}"
  entries+=("$(jq -n --arg name "$name" --arg tag "$resolved_tag" --arg digest "$digest" --arg oci "$oci_ref" '{name:$name,tag:$tag,digest:$digest,oci:$oci}' )")
done < "$MANIFEST_FILE"

# join into JSON array deterministically
printf '%s\n' "[" >"$OUT"
for i in "${!entries[@]}"; do
  printf '  %s' "${entries[$i]}" >>"$OUT"
  if [[ $i -lt $((${#entries[@]} - 1)) ]]; then
    printf ',' >>"$OUT"
  fi
  printf '\n' >>"$OUT"
done
printf ']\n' >>"$OUT"

echo "Wrote $OUT" >&2

