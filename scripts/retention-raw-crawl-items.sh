#!/usr/bin/env bash
# Nightly retention for raw_crawl_items: delete rows older than RETENTION_DAYS whose
# payload has been offloaded to MinIO (payload_s3_key IS NOT NULL) and that are NOT
# referenced by a feed_item (keep those to preserve article -> raw-crawl provenance).
# Deletes in bounded batches so it never opens a giant transaction / blows up WAL.
#
# Payloads live in MinIO regardless of this row; deleting the row only drops the crawl
# metadata (url/headers/timestamps/s3_key). The initial 17.7M-row backfill was run by
# hand + VACUUM FULL; steady state is ~one day of aged rows (~360k) per night.
#
# Installed as the systemd unit raw-crawl-retention.{service,timer} (nightly).
set -euo pipefail

PG_CONTAINER="${PG_CONTAINER:-company-feed-server-postgres-1}"
PG_USER="${PG_USER:-company_feed}"
PG_DB="${PG_DB:-company_feed}"
RETENTION_DAYS="${RETENTION_DAYS:-30}"
BATCH="${BATCH:-100000}"
MAX_BATCHES="${MAX_BATCHES:-1000}"   # safety cap (100M rows) -- far above any nightly need

psql() { docker exec "$PG_CONTAINER" psql -U "$PG_USER" -d "$PG_DB" -tAc "$1"; }

total=0
for ((b = 1; b <= MAX_BATCHES; b++)); do
  n=$(psql "
    WITH del AS (
      DELETE FROM raw_crawl_items
      WHERE id IN (
        SELECT r.id FROM raw_crawl_items r
        WHERE r.created_at < now() - interval '${RETENTION_DAYS} days'
          AND r.payload_s3_key IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM feed_items f WHERE f.raw_crawl_item_id = r.id)
        LIMIT ${BATCH}
      ) RETURNING 1
    ) SELECT count(*) FROM del" | tr -d ' ')
  [ -z "$n" ] && { echo "$(date -u +%FT%TZ) ERROR: empty result at batch $b" >&2; exit 1; }
  total=$((total + n))
  [ "$n" -lt 1 ] && break
done

echo "$(date -u +%FT%TZ) raw_crawl_items retention: deleted=${total} (>${RETENTION_DAYS}d, offloaded, unreferenced)"
