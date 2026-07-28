-- Finish the Cost Plus / Paytient ownership repair across direct evidence.
-- The Paytient origin is a different company and must stay disabled. A legacy
-- Paytient press-wire article is quarantined item-by-item while the shared
-- PRNewswire origin remains available for genuinely Cost Plus-scoped evidence.

CREATE TEMP TABLE wrong_costplus_paytient_sources
ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    'Paytient'::text AS claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM
    companies AS company
    JOIN sources AS source ON source.company_id = company.id
WHERE
    company.company_key = 'yc-mark-cuban-cost-plus-drug-company-pbc'
    AND source.status = 'approved'
    AND lower(source.url) ~ '^https?://([^/]+[.])?paytient[.]com(/|$)';

CREATE TEMP TABLE wrong_costplus_paytient_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    repair.claiming_company_name,
    repair.reason
FROM
    wrong_costplus_paytient_sources AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE wrong_costplus_paytient_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    'Paytient'::text AS claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM
    companies AS company
    JOIN feed_items AS item ON item.company_id = company.id
WHERE
    company.company_key = 'yc-mark-cuban-cost-plus-drug-company-pbc'
    AND NOT item.is_private
    AND (
        lower(item.canonical_url)
            ~ '^https?://([^/]+[.])?paytient[.]com(/|$)'
        OR public_url_identity_key(item.canonical_url)
            = public_url_identity_key(
                'https://www.prnewswire.com/news-releases/paytient-expands-into-prescription-benefits-to-help-employers-and-employees-combat-rising-drug-costs-302823036.html'
            )
    );

CREATE TEMP TABLE wrong_costplus_paytient_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrong_cross_domain_repair_started',
        jsonb_build_object(
            'source_count',
                (SELECT count(*) FROM wrong_costplus_paytient_sources),
            'recipe_count',
                (SELECT count(*) FROM wrong_costplus_paytient_recipes),
            'item_count',
                (SELECT count(*) FROM wrong_costplus_paytient_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_costplus_paytient_items
                ),
            'policy', 'cross-domain-company-ownership.v3',
            'migration', '0113_quarantine_residual_paytient_evidence'
        )
    WHERE
        EXISTS (SELECT 1 FROM wrong_costplus_paytient_sources)
        OR EXISTS (SELECT 1 FROM wrong_costplus_paytient_items)
    RETURNING id
)
INSERT INTO wrong_costplus_paytient_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM wrong_costplus_paytient_recipes AS repair
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
            'policy', 'cross-domain-company-ownership.v3',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0113_quarantine_residual_paytient_evidence'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    wrong_costplus_paytient_recipes AS repair
    CROSS JOIN wrong_costplus_paytient_wave AS wave
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
                'policy', 'cross-domain-company-ownership.v3',
                'claiming_company_name', repair.claiming_company_name,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration', '0113_quarantine_residual_paytient_evidence'
            )
        )
FROM
    wrong_costplus_paytient_sources AS repair
    CROSS JOIN wrong_costplus_paytient_wave AS wave
WHERE source.id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because publication belongs to a different company',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
FROM wrong_costplus_paytient_sources AS repair
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
            'policy', 'cross-domain-company-ownership.v3',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_costplus_paytient_items AS repair
    CROSS JOIN wrong_costplus_paytient_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM wrong_costplus_paytient_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'reversible', false,
        'policy', 'cross-domain-company-ownership.v3',
        'repair_wave_event_id', wave.event_id,
        'migration', '0113_quarantine_residual_paytient_evidence'
    )
FROM
    wrong_costplus_paytient_sources AS repair
    CROSS JOIN wrong_costplus_paytient_wave AS wave;

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
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'reversible', false,
        'policy', 'cross-domain-company-ownership.v3',
        'repair_wave_event_id', wave.event_id,
        'migration', '0113_quarantine_residual_paytient_evidence'
    )
FROM
    wrong_costplus_paytient_items AS repair
    CROSS JOIN wrong_costplus_paytient_wave AS wave;
