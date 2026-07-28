-- Retire exact sources whose feed contents or company-specific path identify a
-- distinct issuer. Each affected company remains eligible for its own source.

CREATE TEMP TABLE wrong_cross_company_sources
ON COMMIT DROP AS
WITH rejected_source (
    company_key,
    source_url,
    claiming_company_name
) AS (
    VALUES
        (
            'barings-corporate-investors-common-stock',
            'https://ir.barings.com/news-events/press-releases',
            'Barings BDC Inc. Common Stock'
        ),
        (
            'barings-corporate-investors-common-stock',
            'https://ir.barings.com/news-events/press-releases/rss',
            'Barings BDC Inc. Common Stock'
        ),
        (
            'teekay-corporation-ltd-common-stock',
            'https://www.teekay.com/investors/teekay-tankers-ltd/news-releases',
            'Teekay Tankers Ltd.'
        ),
        (
            'teekay-tankers-ltd',
            'https://www.teekay.com/feed/',
            'Teekay Corporation Ltd. Common Stock'
        )
)
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    rejected_source.claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM rejected_source
JOIN companies AS company
    ON company.company_key = rejected_source.company_key
JOIN sources AS source
    ON source.company_id = company.id
WHERE
    source.status = 'approved'
    AND public_url_identity_key(source.url)
        = public_url_identity_key(rejected_source.source_url);

CREATE TEMP TABLE wrong_cross_company_source_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    repair.claiming_company_name,
    repair.reason
FROM wrong_cross_company_sources AS repair
JOIN company_news_recipes AS recipe ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE wrong_cross_company_source_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    repair.reason
FROM wrong_cross_company_sources AS repair
JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE wrong_cross_company_source_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrong_company_publication_repair_started',
        jsonb_build_object(
            'source_count', (SELECT count(*) FROM wrong_cross_company_sources),
            'recipe_count',
                (SELECT count(*) FROM wrong_cross_company_source_recipes),
            'item_count', (SELECT count(*) FROM wrong_cross_company_source_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_cross_company_sources
                ),
            'policy', 'cross-company-source-ownership.v1',
            'migration', '0084_retire_cross_company_sources'
        )
    WHERE EXISTS (SELECT 1 FROM wrong_cross_company_sources)
    RETURNING id
)
INSERT INTO wrong_cross_company_source_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM wrong_cross_company_source_recipes AS repair
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
        'ownership_repair',
        jsonb_build_object(
            'policy', 'cross-company-source-ownership.v1',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_cross_company_source_recipes AS repair
    CROSS JOIN wrong_cross_company_source_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata =
        source.metadata - 'active_recipe_id' - 'recipe_schema_version'
        || jsonb_build_object(
            'quality_disable',
            jsonb_build_object(
                'reason', repair.reason,
                'reversible', false,
                'policy', 'cross-company-source-ownership.v1',
                'claiming_company_name', repair.claiming_company_name,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP
            )
        )
FROM
    wrong_cross_company_sources AS repair
    CROSS JOIN wrong_cross_company_source_wave AS wave
WHERE source.id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because source belongs to a different company',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
FROM wrong_cross_company_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.status IN ('pending', 'running');

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', false,
            'policy', 'cross-company-source-ownership.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_cross_company_source_items AS repair
    CROSS JOIN wrong_cross_company_source_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM wrong_cross_company_source_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'rebuild_required', true,
        'policy', 'cross-company-source-ownership.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0084_retire_cross_company_sources'
    )
FROM
    wrong_cross_company_source_recipes AS repair
    CROSS JOIN wrong_cross_company_source_wave AS wave;

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
        'reversible', false,
        'policy', 'cross-company-source-ownership.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0084_retire_cross_company_sources'
    )
FROM
    wrong_cross_company_source_items AS repair
    CROSS JOIN wrong_cross_company_source_wave AS wave;
