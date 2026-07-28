-- Align historical HTML/browser rows with the shared resource-hub policy.
-- Demo series, product tours, and product-tour hubs nested below a broad
-- resource/library route are product utilities, not individual news or blog
-- articles. A parent `/insights/` segment does not convert those more-specific
-- child namespaces into editorial content.
--
-- The quarantine is reversible. A queued recipe crawl applies the current
-- policy to the complete source and may keep distinct blog, research, or news
-- articles from the same broad listing active.

CREATE TEMP TABLE non_editorial_resource_utility_items
ON COMMIT DROP AS
WITH classified AS (
    SELECT DISTINCT ON (item.id)
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        recipe.id AS recipe_id,
        item.url,
        item.canonical_url,
        item.title,
        item.published_at,
        CASE
            WHEN lower(item.canonical_url)
                ~ '/(resource|resources|resource-center|resource-centre|library)/(demo|demos|demo-series)(/|$|[?#])'
                THEN 'non_editorial_resource_demo'
            ELSE 'non_editorial_product_tour_hub'
        END AS reason
    FROM
        feed_items AS item
        LEFT JOIN LATERAL (
            SELECT candidate.id
            FROM company_news_recipes AS candidate
            WHERE candidate.source_id = item.source_id
            ORDER BY
                CASE candidate.status
                    WHEN 'active' THEN 0
                    WHEN 'stale' THEN 1
                    WHEN 'superseded' THEN 2
                    WHEN 'draft' THEN 3
                    ELSE 4
                END,
                candidate.created_at DESC,
                candidate.id
            LIMIT 1
        ) AS recipe ON true
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
        AND (
            lower(item.canonical_url)
                ~ '/(resource|resources|resource-center|resource-centre|library)/(demo|demos|demo-series|product-tour|product-tours)(/|$|[?#])'
            OR lower(item.canonical_url)
                ~ '/product-tour-hub(/|$|[?#])'
            OR lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) ~ '^product tour hub([[:space:]]*[|–—-].*)?$'
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE non_editorial_resource_utility_sources
ON COMMIT DROP AS
SELECT DISTINCT
    item.company_id,
    item.source_id,
    item.recipe_id
FROM
    non_editorial_resource_utility_items AS item
    JOIN company_news_recipes AS recipe
        ON recipe.id = item.recipe_id
WHERE recipe.status = 'active';

CREATE TEMP TABLE non_editorial_resource_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.non_editorial_resource_utility_backfill_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM non_editorial_resource_utility_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM non_editorial_resource_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM non_editorial_resource_utility_items
                ),
            'policy', 'recipe-listing-artifact.v51',
            'migration',
                '0133_quarantine_non_editorial_resource_utilities'
        )
    WHERE EXISTS (
        SELECT 1 FROM non_editorial_resource_utility_items
    )
    RETURNING id
)
INSERT INTO non_editorial_resource_utility_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = COALESCE(
        item.content_processing,
        '{}'::jsonb
    ) || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v51',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration',
                '0133_quarantine_non_editorial_resource_utilities'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    non_editorial_resource_utility_items AS repair
    CROSS JOIN non_editorial_resource_utility_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM non_editorial_resource_utility_items AS repair
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
        'reason', repair.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v51',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0133_quarantine_non_editorial_resource_utilities'
    )
FROM
    non_editorial_resource_utility_items AS repair
    CROSS JOIN non_editorial_resource_utility_wave AS wave;

-- Revalidate every affected active source immediately. Existing active jobs
-- are promoted instead of duplicated.
UPDATE jobs AS job
SET
    priority = GREATEST(job.priority, 16384),
    run_after = LEAST(job.run_after, CURRENT_TIMESTAMP),
    updated_at = CURRENT_TIMESTAMP
WHERE
    job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running')
    AND job.source_id IN (
        SELECT source_id
        FROM non_editorial_resource_utility_sources
    );

INSERT INTO jobs (
    job_type,
    job_key,
    status,
    priority,
    run_after,
    max_attempts,
    company_id,
    source_id,
    payload
)
SELECT
    'crawl_source',
    'source:' || target.source_id::text,
    'pending',
    16384,
    CURRENT_TIMESTAMP,
    5,
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'source_id', target.source_id,
        'recipe_id', target.recipe_id,
        'trigger', 'non_editorial_resource_utility_revalidation',
        'policy', 'recipe-listing-artifact.v51',
        'migration',
            '0133_quarantine_non_editorial_resource_utilities'
    )
FROM non_editorial_resource_utility_sources AS target
WHERE NOT EXISTS (
    SELECT 1
    FROM jobs AS active_job
    WHERE
        active_job.job_type = 'crawl_source'
        AND active_job.job_key = 'source:' || target.source_id::text
        AND active_job.status IN ('pending', 'running')
)
ON CONFLICT (job_type, job_key)
    WHERE status IN ('pending', 'running')
DO NOTHING;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.resource_utility_recrawl_queued',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'reason', 'non_editorial_resource_utility',
        'policy', 'recipe-listing-artifact.v51',
        'migration',
            '0133_quarantine_non_editorial_resource_utilities'
    )
FROM non_editorial_resource_utility_sources AS target;
