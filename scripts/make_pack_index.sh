#!/usr/bin/env bash
set -euo pipefail

TENANT_NAME=${TENANT_NAME:-integration}
RESOLVED_JSON=${RESOLVED_JSON:-target/packs_resolved.json}
MAIN_PACK_NAME=${MAIN_PACK_NAME:-greentic-packs/messaging-dummy}
OVERLAY_PACK_NAMES=${OVERLAY_PACK_NAMES:-greentic-packs/secrets-k8s,greentic-packs/events-dummy}
OUT=${OUT:-target/index.json}
GHCR_ORG=${GHCR_ORG:-greentic-ai}

usage() {
  cat <<'USAGE'
Usage: RESOLVED_JSON=target/packs_resolved.json TENANT_NAME=integration ./scripts/make_pack_index.sh

Inputs:
  TENANT_NAME           tenant key (default: integration)
  RESOLVED_JSON         path to packs_resolved.json from oci_resolve_digests.sh
  MAIN_PACK_NAME        pack to use as main_pack (default: greentic-packs/messaging-dummy)
  OVERLAY_PACK_NAMES    comma-separated list or @file with overlay pack names (optional)

Output:
  target/index.json with runner-compatible pack index.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ ! -f "$RESOLVED_JSON" ]]; then
  echo "Resolved JSON not found: $RESOLVED_JSON" >&2
  exit 1
fi

load_overlays() {
  local raw="$1"
  local out=()
  if [[ -z "$raw" ]]; then
    echo ""; return
  fi
  if [[ "$raw" == @* ]]; then
    local file=${raw#@}
    if [[ ! -f "$file" ]]; then
      echo "overlay file not found: $file" >&2
      exit 1
    fi
    while IFS= read -r line || [[ -n "$line" ]]; do
      line="${line%%#*}"
      line="${line%%[[:space:]]*}"
      [[ -z "$line" ]] && continue
      out+=("$line")
    done <"$file"
  else
    IFS=',' read -ra out <<<"$raw"
  fi
  printf '%s\n' "${out[@]}"
}

overlay_names=()
while IFS= read -r line; do
  overlay_names+=("$line")
done <<EOF
$(load_overlays "$OVERLAY_PACK_NAMES")
EOF

build_entry() {
  local name="$1"
  local digest_tag
  digest_tag=$(jq -r --arg name "$name" '.[] | select(.name==$name) | "\(.digest)|\(.tag)"' "$RESOLVED_JSON" | head -n1)
  if [[ -z "$digest_tag" ]]; then
    echo "pack $name not found in $RESOLVED_JSON" >&2
    exit 1
  fi
  IFS='|' read -r digest tag <<<"$digest_tag"
  local locator="oci://ghcr.io/${GHCR_ORG}/${name}@${digest}"
  jq -n --arg name "$name" --arg tag "$tag" --arg digest "$digest" --arg locator "$locator" '{reference:{name:$name,version:$tag},locator:$locator,path:$locator,digest:$digest}'
}

mkdir -p "$(dirname "$OUT")"

main_entry=$(build_entry "$MAIN_PACK_NAME")
overlay_entries=()
for overlay in "${overlay_names[@]}"; do
  [[ -z "$overlay" ]] && continue
  overlay_entries+=("$(build_entry "$overlay")")
done
overlays_json="[]"
if [[ ${#overlay_entries[@]} -gt 0 ]]; then
  overlays_json=$(printf '%s\n' "${overlay_entries[@]}" | jq -s '.')
fi

{
  printf '{"tenants":{';
  printf '%s' "$(jq -n --arg tenant "$TENANT_NAME" --argjson main "$main_entry" --argjson overlays "$overlays_json" '{($tenant): {main_pack: $main, overlays: $overlays}}')";
  printf '}}\n';
} >"$OUT"

echo "Wrote $OUT" >&2
