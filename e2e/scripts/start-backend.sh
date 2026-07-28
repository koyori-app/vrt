#!/usr/bin/env bash
# Create the e2e database, apply the schema, then start the backend.
#
# Playwright starts webServers before any test runs, so the migration has to
# happen here rather than in a global setup hook.
#
# CI can skip the cargo builds by pointing BACKEND_BIN / MIGRATION_BIN at
# already-built binaries (see .github/workflows/e2e.yml).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

: "${DATABASE_URL:?DATABASE_URL must be set (playwright.config.ts passes it)}"

node "$SCRIPT_DIR/ensure-database.mjs"

# `fresh` (drop + re-apply) rather than `up`: every run starts from a known
# empty schema so the specs can assert on "no tenants yet" style empty states.
if [ -n "${MIGRATION_BIN:-}" ]; then
  "$MIGRATION_BIN" fresh
else
  cargo run --manifest-path "$ROOT/apps/backend/migration/Cargo.toml" -- fresh
fi

if [ -n "${BACKEND_BIN:-}" ]; then
  exec "$BACKEND_BIN"
fi

cd "$ROOT/apps/backend"
# Debug build on purpose: TEST_LOGIN_ENABLED is refused by release builds
# (see common::settings::load_settings).
exec cargo run --bin backend
