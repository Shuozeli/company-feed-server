-- Quiver Quantitative is a shared multi-company market-news host. Direct
-- manual-import evidence from this host predating the runtime host policy must
-- be revalidated under company-identity scope. This also removes one bounded
-- false match where the short URL path `/news/Art` was accepted for
-- Art's-Way even though the article was about Artelo Biosciences.

CREATE TEMP TABLE quiverquant_scope_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    'shared_host_manual_item_requires_revalidation'::text AS reason
FROM
    feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
WHERE
    NOT item.is_private
    AND item.source_kind IN ('html', 'browser')
    AND (
        lower(split_part(
            split_part(btrim(source.url), '://', 2),
            '/',
            1
        )) = 'quiverquant.com'
        OR lower(split_part(
            split_part(btrim(source.url), '://', 2),
            '/',
            1
        )) = 'www.quiverquant.com'
    );

CREATE TEMP TABLE quiverquant_scope_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    source.url AS source_url,
    'shared_host_scope_policy_changed'::text AS reason
FROM
    company_news_recipes AS recipe
    JOIN sources AS source ON source.id = recipe.source_id
WHERE
    recipe.status = 'active'
    AND (
        lower(split_part(
            split_part(btrim(source.url), '://', 2),
            '/',
            1
        )) = 'quiverquant.com'
        OR lower(split_part(
            split_part(btrim(source.url), '://', 2),
            '/',
            1
        )) = 'www.quiverquant.com'
    );

CREATE TEMP TABLE quiverquant_wrong_short_identity_sources
ON COMMIT DROP AS
SELECT DISTINCT
    source.id AS source_id,
    source.company_id,
    source.url,
    'wrong_company_short_identity_match'::text AS reason
FROM
    sources AS source
    JOIN companies AS company ON company.id = source.company_id
    JOIN quiverquant_scope_items AS item ON item.source_id = source.id
WHERE
    company.name = 'Art''s-Way Manufacturing Co. Inc. Common Stock'
    AND item.title ILIKE 'Artelo Biosciences%'
    AND public_url_identity_key(item.canonical_url)
        = public_url_identity_key('https://www.quiverquant.com/news/Art');

CREATE TEMP TABLE quiverquant_scope_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.shared_host_scope_repair_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM quiverquant_scope_items),
            'recipe_count', (SELECT count(*) FROM quiverquant_scope_recipes),
            'wrong_source_count',
                (
                    SELECT count(*)
                    FROM quiverquant_wrong_short_identity_sources
                ),
            'policy', 'shared-direct-scope.v2',
            'migration', '0100_revalidate_quiverquant_company_scope'
        )
    WHERE
        EXISTS (SELECT 1 FROM quiverquant_scope_items)
        OR EXISTS (SELECT 1 FROM quiverquant_scope_recipes)
        OR EXISTS (
            SELECT 1
            FROM quiverquant_wrong_short_identity_sources
        )
    RETURNING id
)
INSERT INTO quiverquant_scope_repair_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM quiverquant_scope_recipes AS repair
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
        'scope_repair',
        jsonb_build_object(
            'policy', 'shared-direct-scope.v2',
            'repair_wave_event_id', wave.event_id,
            'source_url', repair.source_url,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    quiverquant_scope_recipes AS repair
    CROSS JOIN quiverquant_scope_repair_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', repair.reason,
            'reversible', true,
            'policy', 'shared-direct-scope.v2',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    quiverquant_wrong_short_identity_sources AS repair
    CROSS JOIN quiverquant_scope_repair_wave AS wave
WHERE source.id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled by Quiver Quantitative company-scope repair',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
FROM quiverquant_wrong_short_identity_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.status IN ('pending', 'running');

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: recipe staled by shared-host scope repair'
FROM quiverquant_scope_recipes AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

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
            'policy', 'shared-direct-scope.v2',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    quiverquant_scope_items AS repair
    CROSS JOIN quiverquant_scope_repair_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM quiverquant_scope_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'source_url', repair.source_url,
        'reason', repair.reason,
        'rebuild_required', true,
        'policy', 'shared-direct-scope.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0100_revalidate_quiverquant_company_scope'
    )
FROM
    quiverquant_scope_recipes AS repair
    CROSS JOIN quiverquant_scope_repair_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.wrong_company_content_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'reason', repair.reason,
        'reversible', true,
        'policy', 'shared-direct-scope.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0100_revalidate_quiverquant_company_scope'
    )
FROM
    quiverquant_wrong_short_identity_sources AS repair
    CROSS JOIN quiverquant_scope_repair_wave AS wave;

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
        'policy', 'shared-direct-scope.v2',
        'repair_wave_event_id', wave.event_id,
        'migration', '0100_revalidate_quiverquant_company_scope'
    )
FROM
    quiverquant_scope_items AS repair
    CROSS JOIN quiverquant_scope_repair_wave AS wave;
