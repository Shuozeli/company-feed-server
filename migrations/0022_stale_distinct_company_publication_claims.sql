-- A publication listing may legitimately be shared by multiple security rows
-- for the same issuer, but it must not silently become the news source for a
-- different company. Remove the bounded, manually audited cross-company
-- claims found before the runtime ownership guard was introduced. Two generic
-- fund-manager collections are also unscoped to either imported fund.

CREATE TEMP TABLE distinct_company_publication_repairs ON COMMIT DROP AS
WITH targets (
    company_key,
    publication_identity,
    reason,
    preferred_company_key
) AS (
    VALUES
        (
            'chart-industries-inc-common-stock',
            'bakerhughes.com/company/newsroom',
            'publication_claimed_by_distinct_company',
            'baker-hughes-company-class-a-common-stock'
        ),
        (
            'blackrock-credit-allocation-income-trust',
            'blackrock.com/us/financial-professionals/investments/products/closed-end-funds/press-releases',
            'unscoped_manager_publication',
            NULL
        ),
        (
            'blackrock-enhanced-equity-dividend-trust',
            'blackrock.com/us/financial-professionals/investments/products/closed-end-funds/press-releases',
            'unscoped_manager_publication',
            NULL
        ),
        (
            'brookfield-asset-management-inc-class-a-limited-voting-shares',
            'brookfield.com/views-news/news',
            'publication_claimed_by_distinct_company',
            'brookfield-corporation-class-a-limited-voting-shares'
        ),
        (
            'cheniere-energy-partners-lp-common-units',
            'cheniere.com/newsroom',
            'publication_claimed_by_distinct_company',
            'cheniere-energy-inc-common-stock'
        ),
        (
            'gabelli-dividend-income-trust-common-shares-of-beneficial-interest',
            'gabelli.com/insights/gabelli-media/press-releases',
            'unscoped_manager_publication',
            NULL
        ),
        (
            'the-gabelli-healthcare-wellness-trust-common-shares-of-beneficial-interest',
            'gabelli.com/insights/gabelli-media/press-releases',
            'unscoped_manager_publication',
            NULL
        ),
        (
            'sprott-focus-trust-inc-common-stock',
            'sprott.com/insights',
            'publication_claimed_by_distinct_company',
            'sprott-inc-common-shares'
        )
)
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    target.reason,
    target.preferred_company_key
FROM targets AS target
JOIN companies AS company ON company.company_key = target.company_key
JOIN company_news_recipes AS recipe ON recipe.company_id = company.id
WHERE
    recipe.status = 'active'
    AND public_url_identity_key(recipe.spec->>'publication_url')
        = target.publication_identity;

CREATE TEMP TABLE distinct_company_publication_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.publication_ownership_repair_started',
        jsonb_build_object(
            'recipe_count', count(*),
            'policy', 'distinct-company-publication-ownership.v1',
            'migration', '0022_stale_distinct_company_publication_claims'
        )
    FROM distinct_company_publication_repairs
    HAVING count(*) > 0
    RETURNING id
)
INSERT INTO distinct_company_publication_repair_wave (event_id)
SELECT id FROM repair_started;

CREATE TEMP TABLE distinct_company_publication_items ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    repair.reason,
    repair.preferred_company_key
FROM distinct_company_publication_repairs AS repair
JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM distinct_company_publication_repairs AS repair
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
            'policy', 'distinct-company-publication-ownership.v1',
            'repair_wave_event_id', wave.event_id,
            'preferred_company_key', repair.preferred_company_key,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    distinct_company_publication_repairs AS repair
    CROSS JOIN distinct_company_publication_repair_wave AS wave
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
            'policy', 'distinct-company-publication-ownership.v1',
            'repair_wave_event_id', wave.event_id,
            'preferred_company_key', repair.preferred_company_key,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    distinct_company_publication_items AS repair
    CROSS JOIN distinct_company_publication_repair_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM distinct_company_publication_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: recipe staled by publication ownership repair'
FROM distinct_company_publication_repairs AS repair
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
        'policy', 'distinct-company-publication-ownership.v1',
        'repair_wave_event_id', wave.event_id,
        'preferred_company_key', repair.preferred_company_key,
        'migration', '0022_stale_distinct_company_publication_claims'
    )
FROM
    distinct_company_publication_repairs AS repair
    CROSS JOIN distinct_company_publication_repair_wave AS wave;

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
        'policy', 'distinct-company-publication-ownership.v1',
        'repair_wave_event_id', wave.event_id,
        'preferred_company_key', repair.preferred_company_key,
        'migration', '0022_stale_distinct_company_publication_claims'
    )
FROM
    distinct_company_publication_items AS repair
    CROSS JOIN distinct_company_publication_repair_wave AS wave;
