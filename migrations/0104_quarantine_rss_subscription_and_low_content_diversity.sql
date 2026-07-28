-- Align historical public data with two shared recipe-correctness safeguards:
-- RSS subscription landing pages are navigation utilities, and batches whose
-- normalized bodies collapse below 50% diversity are extraction failures.

CREATE TEMP TABLE rss_subscription_utility_items
ON COMMIT DROP AS
SELECT DISTINCT ON (item.id)
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    recipe.id AS recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = item.source_id
WHERE
    NOT item.is_private
    AND recipe.status IN ('active', 'stale')
    AND lower(btrim(regexp_replace(
        item.title,
        '[[:space:]]+',
        ' ',
        'g'
    ))) ~ '^subscribe([^[:alnum:]]|$)'
    AND lower(item.title) ~ '(^|[^[:alnum:]])rss([^[:alnum:]]|$)'
ORDER BY
    item.id,
    (recipe.status = 'active') DESC,
    recipe.created_at DESC,
    recipe.id;

CREATE TEMP TABLE rss_subscription_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.rss_subscription_utility_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM rss_subscription_utility_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM rss_subscription_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM rss_subscription_utility_items
                ),
            'policy', 'recipe-listing-artifact.v35',
            'migration',
                '0104_quarantine_rss_subscription_and_low_content_diversity'
        )
    WHERE EXISTS (SELECT 1 FROM rss_subscription_utility_items)
    RETURNING id
)
INSERT INTO rss_subscription_utility_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'non_editorial_rss_subscription_utility',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v35',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    rss_subscription_utility_items AS repair
    CROSS JOIN rss_subscription_utility_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: non_editorial_rss_subscription_utility',
    normalized_feed_item_id = NULL
FROM rss_subscription_utility_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'raw_crawl_item_id', repair.raw_crawl_item_id,
        'recipe_id', repair.recipe_id,
        'url', repair.url,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', 'non_editorial_rss_subscription_utility',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v35',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0104_quarantine_rss_subscription_and_low_content_diversity'
    )
FROM
    rss_subscription_utility_items AS repair
    CROSS JOIN rss_subscription_utility_wave AS wave;

CREATE TEMP TABLE low_content_diversity_recipes
ON COMMIT DROP AS
WITH active_item_stats AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id,
        source.url AS source_url,
        count(*) AS item_count,
        count(DISTINCT md5(btrim(COALESCE(
            NULLIF(item.body_text, ''),
            NULLIF(item.summary, ''),
            item.title
        )))) AS distinct_content_count
    FROM
        company_news_recipes AS recipe
        JOIN sources AS source ON source.id = recipe.source_id
        JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE
        recipe.status = 'active'
        AND source.kind IN ('html', 'browser')
        AND NOT item.is_private
    GROUP BY
        recipe.id,
        recipe.company_id,
        recipe.source_id,
        source.url
)
SELECT *
FROM active_item_stats
WHERE
    item_count >= 3
    AND distinct_content_count * 2 < item_count;

CREATE TEMP TABLE low_content_diversity_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    invalid.recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN low_content_diversity_recipes AS invalid
        ON invalid.source_id = item.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE low_content_diversity_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.low_content_diversity_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM low_content_diversity_items),
            'recipe_count',
                (SELECT count(*) FROM low_content_diversity_recipes),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM low_content_diversity_recipes
                ),
            'policy', 'recipe-content-diversity.v1',
            'migration',
                '0104_quarantine_rss_subscription_and_low_content_diversity'
        )
    WHERE EXISTS (SELECT 1 FROM low_content_diversity_recipes)
    RETURNING id
)
INSERT INTO low_content_diversity_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'content_diversity_below_minimum',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-content-diversity.v1',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    low_content_diversity_items AS repair
    CROSS JOIN low_content_diversity_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: content_diversity_below_minimum',
    normalized_feed_item_id = NULL
FROM low_content_diversity_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO company_news_recipe_state (
    recipe_id,
    last_attempt_at,
    consecutive_correctness_failures,
    freshness_status,
    correctness_status,
    rebuild_required,
    reason,
    metadata
)
SELECT
    invalid.recipe_id,
    CURRENT_TIMESTAMP,
    1,
    'unknown',
    'failing',
    true,
    'content_diversity_below_minimum',
    jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'item_count', invalid.item_count,
            'distinct_content_count', invalid.distinct_content_count,
            'source_url', invalid.source_url,
            'policy', 'recipe-content-diversity.v1',
            'repair_wave_event_id', wave.event_id,
            'migration',
                '0104_quarantine_rss_subscription_and_low_content_diversity'
        )
    )
FROM
    low_content_diversity_recipes AS invalid
    CROSS JOIN low_content_diversity_wave AS wave
ON CONFLICT (recipe_id) DO UPDATE
SET
    last_attempt_at = CURRENT_TIMESTAMP,
    consecutive_correctness_failures = GREATEST(
        company_news_recipe_state.consecutive_correctness_failures,
        1
    ),
    correctness_status = 'failing',
    rebuild_required = true,
    reason = 'content_diversity_below_minimum',
    metadata = company_news_recipe_state.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_rebuild_required',
    invalid.company_id,
    invalid.source_id,
    jsonb_build_object(
        'recipe_id', invalid.recipe_id,
        'source_url', invalid.source_url,
        'item_count', invalid.item_count,
        'distinct_content_count', invalid.distinct_content_count,
        'reason', 'content_diversity_below_minimum',
        'policy', 'recipe-content-diversity.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0104_quarantine_rss_subscription_and_low_content_diversity'
    )
FROM
    low_content_diversity_recipes AS invalid
    CROSS JOIN low_content_diversity_wave AS wave;
