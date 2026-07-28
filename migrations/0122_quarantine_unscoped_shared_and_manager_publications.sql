-- Retire publication sources that are structurally valid but cannot represent
-- the assigned name-first company:
--
-- * exact global landing pages on shared wire/market-news hosts;
-- * BlackRock manager publications attached to a BlackRock fund or affiliate
--   rather than BlackRock, Inc.;
-- * Gabelli and Angel Oak manager-wide publications attached to one trust.
--
-- The repair is reversible and retains every source, recipe, raw row, and
-- normalized item for audit.

CREATE TEMP TABLE invalid_publication_sources (
    source_id uuid PRIMARY KEY,
    company_id uuid NOT NULL,
    source_url text NOT NULL,
    reason text NOT NULL,
    policy text NOT NULL
) ON COMMIT DROP;

WITH source_scope AS (
    SELECT
        source.id AS source_id,
        source.company_id,
        source.url AS source_url,
        company.company_key,
        lower(
            regexp_replace(
                split_part(split_part(source.url, '://', 2), '/', 1),
                '^www\.',
                ''
            )
        ) AS source_host,
        lower(
            regexp_replace(
                regexp_replace(
                    source.url,
                    '^https?://(www\.)?',
                    '',
                    'i'
                ),
                '/+$',
                ''
            )
        ) AS source_identity
    FROM
        sources AS source
        JOIN companies AS company ON company.id = source.company_id
    WHERE source.status = 'approved'
)
INSERT INTO invalid_publication_sources (
    source_id,
    company_id,
    source_url,
    reason,
    policy
)
SELECT
    source_id,
    company_id,
    source_url,
    CASE
        WHEN source_identity IN (
            'businesswire.com/news',
            'investing.com/news',
            'nasdaq.com/european-market-activity/news/company-news',
            'nasdaq.com/market-activity/quotes/press-releases',
            'nasdaq.com/press-release',
            'prnewswire.com/news',
            'prnewswire.com/news-releases',
            'prnewswire.com/resources/articles',
            'prnewswire.com/ru/press-releases',
            'stocktitan.net/news',
            'accessnewswire.com/newsroom',
            'globenewswire.com/newsroom',
            'blackrock.com/us/financial-professionals/investments/products/closed-end-funds/press-releases',
            'gabelli.com/insights/gabelli-media/press-releases'
        )
            THEN 'unscoped_shared_publication'
        ELSE 'shared_manager_publication_not_entity_scoped'
    END,
    CASE
        WHEN source_identity IN (
            'businesswire.com/news',
            'investing.com/news',
            'nasdaq.com/european-market-activity/news/company-news',
            'nasdaq.com/market-activity/quotes/press-releases',
            'nasdaq.com/press-release',
            'prnewswire.com/news',
            'prnewswire.com/news-releases',
            'prnewswire.com/resources/articles',
            'prnewswire.com/ru/press-releases',
            'stocktitan.net/news',
            'accessnewswire.com/newsroom',
            'globenewswire.com/newsroom',
            'blackrock.com/us/financial-professionals/investments/products/closed-end-funds/press-releases',
            'gabelli.com/insights/gabelli-media/press-releases'
        )
            THEN 'unscoped-shared-publication.v1'
        ELSE 'shared-manager-publication-scope.v2'
    END
FROM source_scope
WHERE
    source_identity IN (
        'businesswire.com/news',
        'investing.com/news',
        'nasdaq.com/european-market-activity/news/company-news',
        'nasdaq.com/market-activity/quotes/press-releases',
        'nasdaq.com/press-release',
        'prnewswire.com/news',
        'prnewswire.com/news-releases',
        'prnewswire.com/resources/articles',
        'prnewswire.com/ru/press-releases',
        'stocktitan.net/news',
        'accessnewswire.com/newsroom',
        'globenewswire.com/newsroom',
        'blackrock.com/us/financial-professionals/investments/products/closed-end-funds/press-releases',
        'gabelli.com/insights/gabelli-media/press-releases'
    )
    OR (
        company_key LIKE 'blackrock-%'
        AND company_key <> 'blackrock-inc-common-stock'
        AND (
            source_host = 'blackrock.com'
            OR source_host LIKE '%.blackrock.com'
        )
    )
    OR (
        company_key LIKE '%gabelli%'
        AND (
            source_host = 'gabelli.com'
            OR source_host LIKE '%.gabelli.com'
        )
    )
    OR (
        company_key = 'angel-oak-financial-strategies-income-term-trust-common-shares-of-beneficial-interest'
        AND (
            source_host = 'angeloakcapital.com'
            OR source_host LIKE '%.angeloakcapital.com'
        )
    );

CREATE TEMP TABLE invalid_publication_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.accepted_source_id AS source_id,
    repair.company_id,
    candidate.candidate_url,
    repair.reason,
    repair.policy
FROM
    invalid_publication_sources AS repair
    JOIN source_candidates AS candidate
        ON candidate.accepted_source_id = repair.source_id
WHERE candidate.status = 'accepted';

CREATE TEMP TABLE invalid_publication_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at,
    repair.reason,
    repair.policy
FROM
    invalid_publication_sources AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id;

CREATE TEMP TABLE invalid_publication_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    repair.reason,
    repair.policy
FROM
    invalid_publication_sources AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE manager_host_exclusions (
    company_id uuid PRIMARY KEY,
    excluded_host text NOT NULL
) ON COMMIT DROP;

INSERT INTO manager_host_exclusions (company_id, excluded_host)
SELECT DISTINCT
    company.id,
    CASE
        WHEN company.company_key LIKE 'blackrock-%' THEN 'blackrock.com'
        WHEN company.company_key LIKE '%gabelli%' THEN 'gabelli.com'
        ELSE 'angeloakcapital.com'
    END
FROM
    invalid_publication_sources AS repair
    JOIN companies AS company ON company.id = repair.company_id
WHERE
    (
        company.company_key LIKE 'blackrock-%'
        AND company.company_key <> 'blackrock-inc-common-stock'
    )
    OR company.company_key LIKE '%gabelli%'
    OR company.company_key = 'angel-oak-financial-strategies-income-term-trust-common-shares-of-beneficial-interest';

UPDATE companies AS company
SET
    metadata = company.metadata || jsonb_build_object(
        'publication_host_policy',
        coalesce(
            company.metadata -> 'publication_host_policy',
            '{}'::jsonb
        ) || jsonb_build_object(
            'policy', 'company-publication-host-policy.v4',
            'excluded_hosts',
                CASE
                    WHEN coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'excluded_hosts',
                        '[]'::jsonb
                    ) @> jsonb_build_array(exclusion.excluded_host)
                        THEN coalesce(
                            company.metadata
                                -> 'publication_host_policy'
                                -> 'excluded_hosts',
                            '[]'::jsonb
                        )
                    ELSE coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'excluded_hosts',
                        '[]'::jsonb
                    ) || jsonb_build_array(exclusion.excluded_host)
                END,
            'direct_evidence_excluded_hosts',
                CASE
                    WHEN coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'direct_evidence_excluded_hosts',
                        '[]'::jsonb
                    ) @> jsonb_build_array(exclusion.excluded_host)
                        THEN coalesce(
                            company.metadata
                                -> 'publication_host_policy'
                                -> 'direct_evidence_excluded_hosts',
                            '[]'::jsonb
                        )
                    ELSE coalesce(
                        company.metadata
                            -> 'publication_host_policy'
                            -> 'direct_evidence_excluded_hosts',
                        '[]'::jsonb
                    ) || jsonb_build_array(exclusion.excluded_host)
                END,
            'reviewed_at', CURRENT_TIMESTAMP,
            'migration',
                '0122_quarantine_unscoped_shared_and_manager_publications'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM manager_host_exclusions AS exclusion
WHERE company.id = exclusion.company_id;

CREATE TEMP TABLE invalid_publication_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'source.invalid_publication_repair_started',
        jsonb_build_object(
            'source_count',
                (SELECT count(*) FROM invalid_publication_sources),
            'candidate_count',
                (SELECT count(*) FROM invalid_publication_candidates),
            'recipe_count',
                (SELECT count(*) FROM invalid_publication_recipes),
            'item_count',
                (SELECT count(*) FROM invalid_publication_items),
            'public_item_count',
                (
                    SELECT count(*)
                    FROM
                        invalid_publication_items AS repair
                        JOIN feed_items AS item
                            ON item.id = repair.feed_item_id
                    WHERE NOT item.is_private
                ),
            'reversible', true,
            'migration',
                '0122_quarantine_unscoped_shared_and_manager_publications'
        )
    WHERE EXISTS (SELECT 1 FROM invalid_publication_sources)
    RETURNING id
)
INSERT INTO invalid_publication_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', repair.reason,
            'reversible', true,
            'policy', repair.policy,
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP,
            'migration',
                '0122_quarantine_unscoped_shared_and_manager_publications'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    invalid_publication_sources AS repair
    CROSS JOIN invalid_publication_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_state AS state
SET
    last_error = 'disabled: ' || repair.reason,
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_publication_sources AS repair
WHERE state.source_id = repair.source_id;

UPDATE company_news_recipes AS recipe
SET
    status = 'superseded',
    stale_at = coalesce(recipe.stale_at, CURRENT_TIMESTAMP),
    stale_reason = repair.reason,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_publication_recipes AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because source was disabled: ' || repair.reason,
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_publication_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running');

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_publication_candidates AS repair
WHERE candidate.id = repair.candidate_id;

INSERT INTO candidate_decisions (
    candidate_id,
    source_id,
    decision,
    decision_mode,
    actor,
    reason,
    metadata
)
SELECT
    repair.candidate_id,
    repair.source_id,
    'rejected',
    'automatic',
    'migration:0122',
    CASE repair.reason
        WHEN 'unscoped_shared_publication'
            THEN 'global shared-news landing page is not company scoped'
        ELSE
            'asset-manager publication is not scoped to the assigned vehicle'
    END,
    jsonb_build_object(
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0122_quarantine_unscoped_shared_and_manager_publications'
    )
FROM
    invalid_publication_candidates AS repair
    CROSS JOIN invalid_publication_wave AS wave;

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
            'policy', repair.policy,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration',
                '0122_quarantine_unscoped_shared_and_manager_publications'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    invalid_publication_items AS repair
    CROSS JOIN invalid_publication_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_publication_sources AS repair
WHERE raw.source_id = repair.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.source_url,
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0122_quarantine_unscoped_shared_and_manager_publications'
    )
FROM
    invalid_publication_sources AS repair
    CROSS JOIN invalid_publication_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'raw_crawl_item_id', repair.raw_crawl_item_id,
        'url', repair.url,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0122_quarantine_unscoped_shared_and_manager_publications'
    )
FROM
    invalid_publication_items AS repair
    CROSS JOIN invalid_publication_wave AS wave;
