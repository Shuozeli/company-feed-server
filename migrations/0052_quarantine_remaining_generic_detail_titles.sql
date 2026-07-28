-- Finish the generic investor-detail title cleanup for URL layouts that do not
-- contain a literal `*-details` path segment. Exact framework labels are never
-- valid article titles. Keep the quarantine versioned so a later successful
-- direct-import recrawl can release each corrected item automatically.

CREATE TEMP TABLE remaining_generic_detail_title_items ON COMMIT DROP AS
WITH normalized AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g')))
            AS normalized_title
    FROM feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
    WHERE
        NOT item.is_private
        AND source.status = 'approved'
        AND source.kind IN ('html', 'browser')
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    canonical_url,
    title,
    'generic_detail_page_title'::text AS reason
FROM normalized
WHERE
    normalized_title IN (
        'news detail',
        'news details',
        'press release detail',
        'press release details',
        'press releases detail',
        'press releases details'
    )
    OR (
        normalized_title ~ '[>›»]'
        AND btrim(regexp_replace(normalized_title, '^.*[>›»]', '')) IN (
            'news detail',
            'news details',
            'press release detail',
            'press release details',
            'press releases detail',
            'press releases details'
        )
    );

CREATE TEMP TABLE remaining_generic_detail_title_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.generic_detail_title_repair_started',
        jsonb_build_object(
            'item_count', count(*),
            'source_count', count(DISTINCT source_id),
            'company_count', count(DISTINCT company_id),
            'policy', 'recipe-listing-artifact.v10',
            'migration', '0052_quarantine_remaining_generic_detail_titles'
        )
    FROM remaining_generic_detail_title_items
    HAVING count(*) > 0
    RETURNING id
)
INSERT INTO remaining_generic_detail_title_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v10',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    remaining_generic_detail_title_items AS repair
    CROSS JOIN remaining_generic_detail_title_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM remaining_generic_detail_title_items AS repair
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
        'reason', repair.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v10',
        'repair_wave_event_id', wave.event_id,
        'migration', '0052_quarantine_remaining_generic_detail_titles'
    )
FROM
    remaining_generic_detail_title_items AS repair
    CROSS JOIN remaining_generic_detail_title_wave AS wave;
