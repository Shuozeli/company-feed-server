#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${DATABASE_URL:?DATABASE_URL must point to the PostgreSQL database to inspect}"

if command -v pg-schema-diff >/dev/null 2>&1; then
  DIFF=(pg-schema-diff)
elif command -v go >/dev/null 2>&1; then
  DIFF=(go run github.com/stripe/pg-schema-diff/cmd/pg-schema-diff@v1.0.8)
else
  echo "pg-schema-diff or Go is required" >&2
  exit 1
fi

exec "${DIFF[@]}" plan \
  --from-dsn "$DATABASE_URL" \
  --to-dir "$ROOT/schema" \
  --include-schema public \
  "$@"
