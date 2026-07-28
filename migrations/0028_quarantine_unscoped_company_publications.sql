-- Structural recipe correctness is not sufficient when a third-party listing
-- mixes many issuers. Retain the raw audit trail, but stale the two manually
-- verified unscoped recipes and quarantine their misattributed public items.
-- A scoped Barchart recipe remains valid after removing its eight unrelated
-- sitewide stories. Runtime company-scope filtering prevents recurrence.

CREATE TEMP TABLE company_scope_recipe_repairs ON COMMIT DROP AS
WITH targets (company_key, publication_identity) AS (
    VALUES
        (
            'keros-therapeutics-inc-common-stock',
            'biospace.com/press-releases'
        ),
        (
            'nexa-resources-s-a-common-shares',
            'stocktitan.net/news'
        )
)
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    'unscoped_third_party_publication'::text AS reason
FROM targets AS target
JOIN companies AS company ON company.company_key = target.company_key
JOIN company_news_recipes AS recipe ON recipe.company_id = company.id
WHERE
    recipe.status = 'active'
    AND public_url_identity_key(recipe.spec->>'publication_url')
        = target.publication_identity;

CREATE TEMP TABLE company_scope_item_repairs ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    repair.reason
FROM company_scope_recipe_repairs AS repair
JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private
UNION ALL
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    'article_not_company_scoped'::text AS reason
FROM companies AS company
JOIN company_news_recipes AS recipe ON recipe.company_id = company.id
JOIN feed_items AS item ON item.source_id = recipe.source_id
WHERE
    company.company_key = 'neumora-therapeutics-inc-common-stock'
    AND recipe.status = 'active'
    AND public_url_identity_key(recipe.spec->>'publication_url')
        = 'barchart.com/stocks/quotes/NMRA/news'
    AND NOT item.is_private
    AND lower(item.title || ' ' || item.canonical_url) NOT LIKE '%neumora%';

CREATE TEMP TABLE company_scope_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.company_scope_repair_started',
        jsonb_build_object(
            'recipe_count', (SELECT count(*) FROM company_scope_recipe_repairs),
            'item_count', (SELECT count(*) FROM company_scope_item_repairs),
            'policy', 'company-scope-relevance.v1',
            'migration', '0028_quarantine_unscoped_company_publications'
        )
    WHERE
        EXISTS (SELECT 1 FROM company_scope_recipe_repairs)
        OR EXISTS (SELECT 1 FROM company_scope_item_repairs)
    RETURNING id
)
INSERT INTO company_scope_repair_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM company_scope_recipe_repairs AS repair
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
        'company_scope_repair',
        jsonb_build_object(
            'policy', 'company-scope-relevance.v1',
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    company_scope_recipe_repairs AS repair
    CROSS JOIN company_scope_repair_wave AS wave
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
            'policy', 'company-scope-relevance.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    company_scope_item_repairs AS repair
    CROSS JOIN company_scope_repair_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM company_scope_item_repairs AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: recipe staled by company-scope correctness repair'
FROM company_scope_recipe_repairs AS repair
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
        'policy', 'company-scope-relevance.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0028_quarantine_unscoped_company_publications'
    )
FROM
    company_scope_recipe_repairs AS repair
    CROSS JOIN company_scope_repair_wave AS wave;

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
        'policy', 'company-scope-relevance.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0028_quarantine_unscoped_company_publications'
    )
FROM
    company_scope_item_repairs AS repair
    CROSS JOIN company_scope_repair_wave AS wave;
