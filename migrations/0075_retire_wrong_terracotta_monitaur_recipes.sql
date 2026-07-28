-- Terracotta AI (YC S23, tryterracotta.com) and Monitaur (the independently
-- founded 2019 AI-governance company at monitaur.ai) are distinct companies.
-- Historical universe aliases incorrectly joined the two, allowing the
-- research adapter to activate Monitaur publications for Terracotta. Remove
-- the false aliases, retire those recipes, and quarantine their items.

CREATE TEMP TABLE wrong_terracotta_monitaur_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    'publication_owned_by_different_company'::text AS reason
FROM
    company_news_recipes AS recipe
    JOIN companies AS company ON company.id = recipe.company_id
    JOIN sources AS source ON source.id = recipe.source_id
WHERE
    company.company_key = 'yc-terracotta-ai'
    AND recipe.status = 'active'
    AND (
        lower(recipe.spec->>'publication_url') ~
            '^https?://([^/]+\.)?monitaur\.ai(/|$)'
        OR lower(source.url) ~
            '^https?://([^/]+\.)?monitaur\.ai(/|$)'
    );

CREATE TEMP TABLE wrong_terracotta_monitaur_items
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
FROM
    wrong_terracotta_monitaur_recipes AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE wrong_terracotta_monitaur_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrong_company_publication_repair_started',
        jsonb_build_object(
            'recipe_count',
                (
                    SELECT count(*)
                    FROM wrong_terracotta_monitaur_recipes
                ),
            'item_count',
                (
                    SELECT count(*)
                    FROM wrong_terracotta_monitaur_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_terracotta_monitaur_recipes
                ),
            'policy', 'company-ownership.v1',
            'migration',
                '0075_retire_wrong_terracotta_monitaur_recipes'
        )
    WHERE EXISTS (
        SELECT 1 FROM wrong_terracotta_monitaur_recipes
    )
    RETURNING id
)
INSERT INTO wrong_terracotta_monitaur_wave (event_id)
SELECT id FROM repair_started;

UPDATE companies
SET
    aliases = aliases
        - 'Monitaur'
        - 'Monitaur (Pivoted)'
        - 'OpsBerry AI (formally Monitaur)',
    metadata = metadata || jsonb_build_object(
        'alias_correction',
        jsonb_build_object(
            'removed_aliases',
                jsonb_build_array(
                    'Monitaur',
                    'Monitaur (Pivoted)',
                    'OpsBerry AI (formally Monitaur)'
                ),
            'reason', 'aliases_belong_to_distinct_monitaur_company',
            'policy', 'company-ownership.v1',
            'corrected_at', CURRENT_TIMESTAMP
        )
    ),
    updated_at = CURRENT_TIMESTAMP
WHERE company_key = 'yc-terracotta-ai';

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM wrong_terracotta_monitaur_recipes AS repair
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
            'policy', 'company-ownership.v1',
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_terracotta_monitaur_recipes AS repair
    CROSS JOIN wrong_terracotta_monitaur_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', repair.reason,
            'reversible', false,
            'policy', 'company-ownership.v1',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_terracotta_monitaur_recipes AS repair
    CROSS JOIN wrong_terracotta_monitaur_wave AS wave
WHERE source.id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error =
        'cancelled because source belongs to a different company',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
FROM wrong_terracotta_monitaur_recipes AS repair
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
            'policy', 'company-ownership.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_terracotta_monitaur_items AS repair
    CROSS JOIN wrong_terracotta_monitaur_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM wrong_terracotta_monitaur_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

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
        'policy', 'company-ownership.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0075_retire_wrong_terracotta_monitaur_recipes'
    )
FROM
    wrong_terracotta_monitaur_recipes AS repair
    CROSS JOIN wrong_terracotta_monitaur_wave AS wave;

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
        'policy', 'company-ownership.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0075_retire_wrong_terracotta_monitaur_recipes'
    )
FROM
    wrong_terracotta_monitaur_items AS repair
    CROSS JOIN wrong_terracotta_monitaur_wave AS wave;
