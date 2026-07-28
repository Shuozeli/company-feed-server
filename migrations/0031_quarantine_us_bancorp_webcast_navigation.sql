-- The U.S. Bancorp publication pages currently expose only a navigation link
-- ("Webcasts & presentations") to the generic HTML recipe crawler. The link is
-- not a news article. Retire both equivalent recipe variants and reversibly
-- quarantine their already-normalized navigation items.

CREATE TEMP TABLE us_bancorp_navigation_repairs ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    'generic_navigation_title'::text AS reason
FROM companies AS company
JOIN company_news_recipes AS recipe ON recipe.company_id = company.id
WHERE
    company.company_key = 'u-s-bancorp-common-stock'
    AND recipe.status = 'active'
    AND public_url_identity_key(recipe.spec->>'publication_url') IN (
        'ir.usbank.com/news-events/news',
        'ir.usbank.com/news-events/news/default.aspx'
    );

CREATE TEMP TABLE us_bancorp_navigation_items ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    repair.reason
FROM us_bancorp_navigation_repairs AS repair
JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE
    NOT item.is_private
    AND lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g')))
        IN ('webcasts & presentations', 'webcasts and presentations');

CREATE TEMP TABLE us_bancorp_navigation_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.navigation_recipe_repair_started',
        jsonb_build_object(
            'recipe_count', (SELECT count(*) FROM us_bancorp_navigation_repairs),
            'item_count', (SELECT count(*) FROM us_bancorp_navigation_items),
            'policy', 'recipe-listing-artifact.v6',
            'migration', '0031_quarantine_us_bancorp_webcast_navigation'
        )
    WHERE EXISTS (SELECT 1 FROM us_bancorp_navigation_repairs)
    RETURNING id
)
INSERT INTO us_bancorp_navigation_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM us_bancorp_navigation_repairs AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures = GREATEST(
        state.consecutive_correctness_failures,
        3
    ),
    reason = repair.reason,
    metadata = state.metadata || jsonb_build_object(
        'navigation_repair',
        jsonb_build_object(
            'policy', 'recipe-listing-artifact.v6',
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    us_bancorp_navigation_repairs AS repair
    CROSS JOIN us_bancorp_navigation_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v6',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    us_bancorp_navigation_items AS repair
    CROSS JOIN us_bancorp_navigation_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM us_bancorp_navigation_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: recipe staled by navigation correctness repair'
FROM us_bancorp_navigation_repairs AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'reason', repair.reason,
        'rebuild_required', true,
        'policy', 'recipe-listing-artifact.v6',
        'repair_wave_event_id', wave.event_id,
        'migration', '0031_quarantine_us_bancorp_webcast_navigation'
    )
FROM
    us_bancorp_navigation_repairs AS repair
    CROSS JOIN us_bancorp_navigation_wave AS wave;

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
        'policy', 'recipe-listing-artifact.v6',
        'repair_wave_event_id', wave.event_id,
        'migration', '0031_quarantine_us_bancorp_webcast_navigation'
    )
FROM
    us_bancorp_navigation_items AS repair
    CROSS JOIN us_bancorp_navigation_wave AS wave;
