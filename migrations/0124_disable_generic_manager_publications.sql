-- Disable two exact manager-wide publications after composite-identity
-- revalidation proved that they do not represent the assigned vehicle. Other
-- fund-specific publications on the same manager hosts remain eligible.

CREATE TEMP TABLE generic_manager_sources
ON COMMIT DROP AS
WITH scoped AS (
    SELECT
        source.id AS source_id,
        source.company_id,
        source.url AS source_url,
        company.company_key
    FROM
        sources AS source
        JOIN companies AS company ON company.id = source.company_id
    WHERE source.status = 'approved'
)
SELECT
    source_id,
    company_id,
    source_url,
    'shared_manager_publication_not_entity_scoped'::text AS reason,
    'shared-manager-publication-scope.v3'::text AS policy
FROM scoped
WHERE
    (
        company_key = 'blackstone-mortgage-trust-inc-common-stock'
        AND public_url_identity_key(source_url)
            = public_url_identity_key(
                'https://www.blackstone.com/news/press'
            )
    )
    OR (
        company_key = 'sprott-focus-trust-inc-common-stock'
        AND public_url_identity_key(source_url)
            = public_url_identity_key(
                'https://sprott.com/investor-relations/press-releases'
            )
    );

CREATE TEMP TABLE generic_manager_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    repair.reason,
    repair.policy
FROM
    generic_manager_sources AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE generic_manager_items
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
    generic_manager_sources AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id;

CREATE TEMP TABLE generic_manager_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.generic_manager_publication_repair_started',
        jsonb_build_object(
            'source_count', (SELECT count(*) FROM generic_manager_sources),
            'recipe_count', (SELECT count(*) FROM generic_manager_recipes),
            'item_count', (SELECT count(*) FROM generic_manager_items),
            'public_item_count',
                (
                    SELECT count(*)
                    FROM
                        generic_manager_items AS repair
                        JOIN feed_items AS item
                            ON item.id = repair.feed_item_id
                    WHERE NOT item.is_private
                ),
            'reversible', false,
            'policy', 'shared-manager-publication-scope.v3',
            'migration', '0124_disable_generic_manager_publications'
        )
    WHERE EXISTS (SELECT 1 FROM generic_manager_sources)
    RETURNING id
)
INSERT INTO generic_manager_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'superseded',
    stale_at = coalesce(recipe.stale_at, CURRENT_TIMESTAMP),
    stale_reason = repair.reason,
    updated_at = CURRENT_TIMESTAMP
FROM generic_manager_recipes AS repair
WHERE recipe.id = repair.recipe_id;

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
                'policy', repair.policy,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration', '0124_disable_generic_manager_publications'
            )
        ),
    updated_at = CURRENT_TIMESTAMP
FROM
    generic_manager_sources AS repair
    CROSS JOIN generic_manager_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_state AS state
SET
    last_error = 'disabled: shared_manager_publication_not_entity_scoped',
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM generic_manager_sources AS repair
WHERE state.source_id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error =
        'cancelled because manager publication is not entity scoped',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM generic_manager_sources AS repair
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
            'policy', repair.policy,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0124_disable_generic_manager_publications'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    generic_manager_items AS repair
    CROSS JOIN generic_manager_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: shared_manager_publication_not_entity_scoped',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM generic_manager_sources AS repair
WHERE raw.source_id = repair.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.source_url,
        'reason', repair.reason,
        'reversible', false,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration', '0124_disable_generic_manager_publications'
    )
FROM
    generic_manager_sources AS repair
    CROSS JOIN generic_manager_wave AS wave;

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
        'reversible', false,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration', '0124_disable_generic_manager_publications'
    )
FROM
    generic_manager_items AS repair
    CROSS JOIN generic_manager_wave AS wave;
