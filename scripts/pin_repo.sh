#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  cat <<USAGE
Usage: $0 <github-repo> <sha>
Example: $0 greenticai/greentic-messaging 0123456789abcdef
USAGE
  exit 2
fi

REPO_ID=$1
SHA=$2
CRATE_NAME=${REPO_ID##*/}
CONFIG_FILE=.cargo/config.toml
SECTION="[patch.\"https://github.com/${REPO_ID}.git\"]"

mkdir -p .cargo
if [[ -f "${CONFIG_FILE}" ]]; then
  python3 - "$CONFIG_FILE" "$SECTION" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
section = sys.argv[2]
text = path.read_text() if path.exists() else ""
lines = text.splitlines()
result = []
skip = False
for line in lines:
    stripped = line.strip()
    if stripped.startswith('['):
        if skip and stripped != section:
            skip = False
        if stripped == section:
            skip = True
            continue
    if skip:
        continue
    result.append(line)
path.write_text("\n".join(result).rstrip() + ("\n" if result else ""))
PY
else
  touch "${CONFIG_FILE}"
fi

cat <<PATCH >>"${CONFIG_FILE}"$'\n'
${SECTION}
${CRATE_NAME} = { git = "https://github.com/${REPO_ID}.git", rev = "${SHA}" }
PATCH

cat <<MSG
Pinned ${REPO_ID} to ${SHA} in ${CONFIG_FILE}.
Run 'cargo update -p ${CRATE_NAME}' to apply, and commit the config change when sharing the pin.
MSG

