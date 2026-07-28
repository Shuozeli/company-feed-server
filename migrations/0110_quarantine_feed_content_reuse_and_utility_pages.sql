-- Apply the shared article contract to legacy RSS/Atom rows as well as recipe
-- output. One sanitized body cannot represent multiple differently titled
-- stories, legal/subscription pages are utilities, and deterministic CMS
-- fixtures are not editorial content. Also retire the confirmed Novartis
-- association with bootdey.com's unrelated Bootstrap-snippet feed.

CREATE TEMP TABLE feed_quality_items
ON COMMIT DROP AS
WITH base AS (
    SELECT
        item.*,
        source.url AS source_url,
        company.name AS company_name,
        lower(regexp_replace(
            split_part(split_part(
                COALESCE(NULLIF(item.canonical_url, ''), item.url),
                '?',
                1
            ), '#', 1),
            '/$',
            ''
        )) AS clean_url,
        lower(regexp_replace(
            btrim(item.title),
            '[[:space:]]+',
            ' ',
            'g'
        )) AS normalized_title,
        regexp_replace(
            lower(btrim(COALESCE(
                NULLIF(item.body_text, ''),
                NULLIF(item.summary, '')
            ))),
            '[[:space:]]+',
            ' ',
            'g'
        ) AS normalized_body
    FROM
        feed_items AS item
        JOIN sources AS source ON source.id = item.source_id
        JOIN companies AS company ON company.id = item.company_id
    WHERE
        NOT item.is_private
        AND source.status = 'approved'
),
repeated_groups AS (
    SELECT
        source_id,
        md5(normalized_body) AS body_hash
    FROM base
    WHERE normalized_body <> ''
    GROUP BY source_id, md5(normalized_body)
    HAVING
        count(*) > 1
        AND count(DISTINCT normalized_title) > 1
),
classified AS (
    SELECT
        base.*,
        CASE
            WHEN
                lower(base.source_url) = 'http://bootdey.com/rss'
                AND lower(base.company_name) = 'novartis ag common stock'
            THEN 'publication_owned_by_different_company'
            WHEN clean_url ~ '/([0-9]+[-_])?(cookie-(notice|policy)|legal-notices|newsletter|privacy|privacy-(notice|policy)|terms|terms-(and-conditions|conditions|of-service|of-use))(\.(aspx|html|asp|htm|php))?$'
                OR clean_url ~ '/legal/(privacy|privacy-(notice|policy)|terms|terms-(and-conditions|conditions|of-service|of-use))(\.(aspx|html|asp|htm|php))?$'
            THEN 'non_editorial_legal_or_subscription_utility'
            WHEN
                (
                    normalized_title = 'coming soon'
                    AND length(btrim(base.body_text)) <= 200
                )
                OR normalized_title ~ '^blog title [0-9]+$'
                OR (
                    normalized_title ~ '^(story|storie)-[0-9]+$'
                    AND length(btrim(base.body_text)) <= 200
                )
                OR (
                    normalized_title ~ '^group_[a-f0-9]{8,}([ _-].*)?$'
                    AND length(btrim(base.body_text)) <= 200
                )
            THEN 'non_editorial_cms_fixture'
            WHEN repeated.source_id IS NOT NULL
            THEN 'repeated_sanitized_content'
        END AS reason
    FROM
        base
        LEFT JOIN repeated_groups AS repeated
            ON repeated.source_id = base.source_id
            AND repeated.body_hash = md5(base.normalized_body)
),
repairable AS (
    SELECT
        classified.*,
        CASE classified.reason
            WHEN 'publication_owned_by_different_company'
            THEN 'cross-company-source-ownership.v2'
            WHEN 'non_editorial_legal_or_subscription_utility'
            THEN 'recipe-listing-artifact.v46'
            WHEN 'non_editorial_cms_fixture'
            THEN 'cms-placeholder.v3'
            ELSE 'feed-content-diversity.v1'
        END AS policy,
        classified.reason <> 'publication_owned_by_different_company'
            AS reversible
    FROM classified
    WHERE classified.reason IS NOT NULL
)
SELECT
    repairable.id AS feed_item_id,
    repairable.raw_crawl_item_id,
    repairable.company_id,
    repairable.source_id,
    recipe.id AS recipe_id,
    repairable.url,
    repairable.canonical_url,
    repairable.title,
    repairable.published_at,
    repairable.reason,
    repairable.policy,
    repairable.reversible
FROM
    repairable
    LEFT JOIN LATERAL (
        SELECT candidate.id
        FROM company_news_recipes AS candidate
        WHERE candidate.source_id = repairable.source_id
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
    ) AS recipe ON true;

CREATE TEMP TABLE wrong_company_feed_sources
ON COMMIT DROP AS
SELECT DISTINCT
    item.source_id,
    item.company_id,
    source.url,
    item.reason,
    item.policy
FROM
    feed_quality_items AS item
    JOIN sources AS source ON source.id = item.source_id
WHERE item.reason = 'publication_owned_by_different_company';

CREATE TEMP TABLE feed_quality_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.feed_quality_backfill_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM feed_quality_items),
            'recipe_count',
                (
                    SELECT count(DISTINCT recipe_id)
                    FROM feed_quality_items
                    WHERE recipe_id IS NOT NULL
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM feed_quality_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM feed_quality_items
                ),
            'disabled_source_count',
                (SELECT count(*) FROM wrong_company_feed_sources),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM feed_quality_items
                        GROUP BY reason
                    ) AS counts
                ),
            'migration',
                '0110_quarantine_feed_content_reuse_and_utility_pages'
        )
    WHERE EXISTS (SELECT 1 FROM feed_quality_items)
    RETURNING id
)
INSERT INTO feed_quality_wave (event_id)
SELECT id FROM repair_started;

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
                'migration',
                    '0110_quarantine_feed_content_reuse_and_utility_pages'
            )
        )
FROM
    wrong_company_feed_sources AS repair
    CROSS JOIN feed_quality_wave AS wave
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
FROM wrong_company_feed_sources AS repair
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
            'reversible', repair.reversible,
            'policy', repair.policy,
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    feed_quality_items AS repair
    CROSS JOIN feed_quality_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM feed_quality_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

CREATE TEMP TABLE feed_quality_recipe_targets
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    count(*) AS item_count,
    array_agg(DISTINCT repair.reason ORDER BY repair.reason) AS reasons
FROM
    feed_quality_items AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
        AND recipe.status = 'active'
    LEFT JOIN company_news_recipe_state AS state
        ON state.recipe_id = recipe.id
    LEFT JOIN wrong_company_feed_sources AS wrong
        ON wrong.source_id = recipe.source_id
WHERE
    wrong.source_id IS NULL
    AND NOT COALESCE(state.rebuild_required, false)
GROUP BY
    recipe.id,
    recipe.company_id,
    recipe.source_id;

INSERT INTO company_news_recipe_state (
    recipe_id,
    consecutive_correctness_failures,
    freshness_status,
    correctness_status,
    rebuild_required,
    reason,
    metadata
)
SELECT
    target.recipe_id,
    1,
    'unknown',
    'failing',
    false,
    'quality_revalidation_required',
    jsonb_build_object(
        'quality_revalidation',
        jsonb_build_object(
            'item_count', target.item_count,
            'reasons', to_jsonb(target.reasons),
            'repair_wave_event_id', wave.event_id,
            'migration',
                '0110_quarantine_feed_content_reuse_and_utility_pages'
        )
    )
FROM
    feed_quality_recipe_targets AS target
    CROSS JOIN feed_quality_wave AS wave
ON CONFLICT (recipe_id) DO UPDATE
SET
    consecutive_correctness_failures = GREATEST(
        company_news_recipe_state.consecutive_correctness_failures,
        1
    ),
    correctness_status = 'failing',
    rebuild_required = false,
    reason = 'quality_revalidation_required',
    metadata = company_news_recipe_state.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

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
        'reversible', repair.reversible,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0110_quarantine_feed_content_reuse_and_utility_pages'
    )
FROM
    feed_quality_items AS repair
    CROSS JOIN feed_quality_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_quality_revalidation_required',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'item_count', target.item_count,
        'reasons', to_jsonb(target.reasons),
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0110_quarantine_feed_content_reuse_and_utility_pages'
    )
FROM
    feed_quality_recipe_targets AS target
    CROSS JOIN feed_quality_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'reason', repair.reason,
        'reversible', false,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0110_quarantine_feed_content_reuse_and_utility_pages'
    )
FROM
    wrong_company_feed_sources AS repair
    CROSS JOIN feed_quality_wave AS wave;
