#!/usr/bin/env bash
# Generate entity models (pure sea-orm-cli output, no postprocess)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BACKEND="$ROOT/apps/backend"

if [[ -f "$BACKEND/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$BACKEND/.env"
  set +a
fi

: "${database_url:?database_url must be set in apps/backend/.env}"

TABLES="${1:-}"
if [[ -z "$TABLES" ]]; then
  echo "usage: $0 <comma-separated-table-names>" >&2
  echo "example: $0 users,oauth_connections,personal_tokens" >&2
  exit 1
fi

mkdir -p "$BACKEND/crates/entity/src/_generated"

sea-orm-cli generate entity \
  --database-url "$database_url" \
  --output-dir "$BACKEND/crates/entity/src/_generated" \
  --tables "$TABLES" \
  --entity-format dense \
  --date-time-crate chrono \
  --with-prelude none \
  --impl-active-model-behavior

# Re-apply hand-written enum / JSON column types onto the pure cli output.
"$ROOT/scripts/seaorm_postprocess.sh"
