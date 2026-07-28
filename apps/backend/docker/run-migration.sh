#!/usr/bin/env bash
# マイグレーション適用（one-shot）。
# `up`（冪等・データ保持）を使う。`fresh` は全テーブル DROP のため、compose の
# depends_on 経由で再実行されるたびに開発データが全消去される。
# まっさらにしたい時だけ手動で: docker compose run --rm migration /app/migration fresh
set -euo pipefail

/app/migration up
