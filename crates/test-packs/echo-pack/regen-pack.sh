#!/usr/bin/env bash
set -euo pipefail

PACK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

"${PACK_DIR}/regen.sh"

greentic-pack build --in "${PACK_DIR}"

greentic-pack doctor "${PACK_DIR}/dist/echo-pack.gtpack"
