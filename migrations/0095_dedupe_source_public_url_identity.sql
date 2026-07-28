-- A public article URL is the source-local item identity even when a CMS
-- alternates its canonical tag between the public site and an origin host.
-- Keep the strongest public/canonical row, quarantine historical duplicates,
-- and enforce the same invariant for subsequent crawls.

CREATE TEMP TABLE duplicate_source_url_items
ON COMMIT DROP AS
WITH ranked AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.url,
        item.canonical_url,
        item.title,
        item.published_at,
        row_number() OVER (
            PARTITION BY
                item.source_id,
                public_url_identity_key(item.url)
            ORDER BY
                (
                    public_url_identity_key(item.canonical_url)
                        = public_url_identity_key(item.url)
                ) DESC,
                (
                    regexp_replace(
                        lower(split_part(
                            split_part(item.canonical_url, '://', 2),
                            '/',
                            1
                        )),
                        '^www\.',
                        ''
                    )
                    =
                    regexp_replace(
                        lower(split_part(
                            split_part(item.url, '://', 2),
                            '/',
                            1
                        )),
                        '^www\.',
                        ''
                    )
                ) DESC,
                item.fetched_at DESC,
                item.created_at DESC,
                item.id
        ) AS identity_rank,
        count(*) OVER (
            PARTITION BY
                item.source_id,
                public_url_identity_key(item.url)
        ) AS identity_count
    FROM feed_items AS item
    WHERE NOT item.is_private
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    url,
    canonical_url,
    title,
    published_at
FROM ranked
WHERE
    identity_count > 1
    AND identity_rank > 1;

CREATE TEMP TABLE source_url_identity_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'feed_item.source_url_identity_dedup_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM duplicate_source_url_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM duplicate_source_url_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM duplicate_source_url_items
                ),
            'policy', 'source-public-url-identity.v1',
            'migration', '0095_dedupe_source_public_url_identity'
        )
    WHERE EXISTS (
        SELECT 1 FROM duplicate_source_url_items
    )
    RETURNING id
)
INSERT INTO source_url_identity_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'duplicate_source_public_url_identity',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'source-public-url-identity.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    duplicate_source_url_items AS repair
    CROSS JOIN source_url_identity_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: duplicate_source_public_url_identity',
    normalized_feed_item_id = NULL
FROM duplicate_source_url_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'raw_crawl_item_id', repair.raw_crawl_item_id,
        'url', repair.url,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', 'duplicate_source_public_url_identity',
        'reversible', true,
        'policy', 'source-public-url-identity.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0095_dedupe_source_public_url_identity'
    )
FROM
    duplicate_source_url_items AS repair
    CROSS JOIN source_url_identity_wave AS wave;

CREATE UNIQUE INDEX feed_items_source_public_url_identity_unique_idx
ON feed_items (source_id, public_url_identity_key(url))
WHERE NOT is_private;
