#!/usr/bin/env bash
# Re-apply hand-written enum / JSON column types onto pure sea-orm-cli output.
#
# sea-orm-cli emits varchar-backed columns as `String` and json columns as `Json`.
# We want type-safe wrapper enums (DeriveActiveEnum) instead, so the application
# code can rely on the entity Model carrying the real domain types.
#
# This runs after `sea-orm-cli generate` (see seaorm_generate.sh). It is idempotent:
# re-running it on already-processed files is a no-op.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="$ROOT/apps/backend/crates/entity/src/_generated"

# replace <file> <original-field-line> <typed-field-line>
replace() {
  local file="$GEN/$1"
  local from="$2"
  local to="$3"
  if [[ ! -f "$file" ]]; then
    return 0
  fi
  # Only rewrite the pure-output form; if already typed this is a no-op.
  perl -0pi -e "s/\Q$from\E/$to/" "$file"
}

# table file                    pure output field                  typed field
# （現時点で型の付け替えが必要なテーブルはない。builds.status 等のドメイン enum を
#   導入するフェーズで、task リポジトリと同じ形式のルールをここへ追加する。）

echo "seaorm_postprocess: enum/json column types applied"
