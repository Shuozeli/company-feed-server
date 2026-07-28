-- Remove three generic normalization artifacts while preserving the source
-- rows for a later successful crawl to release:
--   * publication dates outside the supported modern article window;
--   * year-archive and transcript resource labels published as headlines;
--   * descriptive "Read more about ..." CTA text published as a headline.
--
-- The crawler now rejects implausible dates, unwraps descriptive CTA prefixes,
-- expands year archives instead of publishing them, and rejects generic
-- transcript labels.

CREATE TEMP TABLE implausible_date_and_listing_artifacts ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    CASE
        WHEN
            item.published_at < TIMESTAMPTZ '1990-01-01 00:00:00+00'
            OR item.published_at >=
                date_trunc('year', CURRENT_TIMESTAMP) + INTERVAL '3 years'
        THEN 'implausible_publication_date'
        WHEN
            item.source_kind IN ('html', 'browser')
            AND lower(btrim(regexp_replace(
                item.title,
                '[[:space:]]+',
                ' ',
                'g'
            ))) IN ('news archive', 'view transcript')
        THEN 'generic_listing_resource_title'
        ELSE 'cta_prefixed_headline'
    END AS reason
FROM feed_items AS item
WHERE
    NOT item.is_private
    AND (
        item.published_at < TIMESTAMPTZ '1990-01-01 00:00:00+00'
        OR item.published_at >=
            date_trunc('year', CURRENT_TIMESTAMP) + INTERVAL '3 years'
        OR (
            item.source_kind IN ('html', 'browser')
            AND (
                lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) IN ('news archive', 'view transcript')
                OR lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) LIKE 'read more about %'
            )
        )
    );

CREATE TEMP TABLE implausible_date_and_listing_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.implausible_date_and_listing_repair_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM implausible_date_and_listing_artifacts),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM implausible_date_and_listing_artifacts
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM implausible_date_and_listing_artifacts
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM implausible_date_and_listing_artifacts
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v14',
            'migration',
                '0058_quarantine_implausible_dates_and_listing_titles'
        )
    WHERE EXISTS (SELECT 1 FROM implausible_date_and_listing_artifacts)
    RETURNING id
)
INSERT INTO implausible_date_and_listing_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v14',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    implausible_date_and_listing_artifacts AS repair
    CROSS JOIN implausible_date_and_listing_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM implausible_date_and_listing_artifacts AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', repair.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v14',
        'repair_wave_event_id', wave.event_id,
        'migration', '0058_quarantine_implausible_dates_and_listing_titles'
    )
FROM
    implausible_date_and_listing_artifacts AS repair
    CROSS JOIN implausible_date_and_listing_wave AS wave;
