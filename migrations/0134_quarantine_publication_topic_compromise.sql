-- Close two correctness gaps exposed by the terminal all-company audit.
--
-- 1. Generic legal-notice and video-library hubs are navigation utilities,
--    not individual editorial items. Align historical rows with the shared
--    runtime utility policy and immediately recrawl affected approved sources.
--
-- 2. Infina's first-party WordPress publication is currently compromised and
--    emitting a high-volume, multilingual casino-SEO stream unrelated to its
--    wealth-management product. Host ownership is valid, but source content is
--    not. Disable the affected blog sources, stale the HTML recipe, and retain
--    every observed row privately under a reversible incident policy.
--
-- The generic runtime guard rejects future feeds or recipe samples when at
-- least four fifths of a five-item-or-larger sample contains casino/gambling
-- plus wager, promotion, or game signals and the company profile does not
-- identify a gambling publication.

CREATE TEMP TABLE non_editorial_media_utility_items
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
                ~ '/legal-notice([.]html?)?/?([?#].*)?$'
                OR lower(regexp_replace(
                    btrim(item.title),
                    '[[:space:]]+',
                    ' ',
                    'g'
                )) = 'legal notice'
                THEN 'non_editorial_legal_notice'
            ELSE 'non_editorial_video_library'
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
        AND (
            lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) IN ('legal notice', 'videos')
            OR lower(regexp_replace(
                btrim(item.title),
                '[[:space:]]+',
                ' ',
                'g'
            )) ~ '^videos[[:space:]]*[|–—-][[:space:]]*news and media([[:space:]]*[|–—-].*)?$'
            OR lower(item.canonical_url)
                ~ '/legal-notice([.]html?)?/?([?#].*)?$'
            OR (
                lower(item.canonical_url) ~ '/videos?/?([?#].*)?$'
                AND lower(regexp_replace(
                    btrim(item.title),
                    '[[:space:]]+',
                    ' ',
                    'g'
                )) ~ '(^videos?$|videos?[[:space:]]*[|–—-])'
            )
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE non_editorial_media_utility_sources
ON COMMIT DROP AS
SELECT DISTINCT
    item.company_id,
    item.source_id,
    item.recipe_id
FROM non_editorial_media_utility_items AS item
JOIN sources AS source ON source.id = item.source_id
WHERE source.status = 'approved';

CREATE TEMP TABLE non_editorial_media_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.non_editorial_media_utility_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM non_editorial_media_utility_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM non_editorial_media_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM non_editorial_media_utility_items
                ),
            'policy', 'recipe-listing-artifact.v52',
            'migration',
                '0134_quarantine_publication_topic_compromise'
        )
    WHERE EXISTS (
        SELECT 1 FROM non_editorial_media_utility_items
    )
    RETURNING id
)
INSERT INTO non_editorial_media_utility_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v52',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration',
                '0134_quarantine_publication_topic_compromise'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    non_editorial_media_utility_items AS repair
    CROSS JOIN non_editorial_media_utility_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM non_editorial_media_utility_items AS repair
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
        'policy', 'recipe-listing-artifact.v52',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0134_quarantine_publication_topic_compromise'
    )
FROM
    non_editorial_media_utility_items AS repair
    CROSS JOIN non_editorial_media_utility_wave AS wave;

-- Revalidate every affected approved source with the new runtime utility
-- policy. Existing active jobs are promoted instead of duplicated.
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
        FROM non_editorial_media_utility_sources
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
        'trigger', 'non_editorial_media_utility_revalidation',
        'policy', 'recipe-listing-artifact.v52',
        'migration',
            '0134_quarantine_publication_topic_compromise'
    )
FROM non_editorial_media_utility_sources AS target
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
    'source.media_utility_recrawl_queued',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'recipe_id', target.recipe_id,
        'policy', 'recipe-listing-artifact.v52',
        'migration',
            '0134_quarantine_publication_topic_compromise'
    )
FROM non_editorial_media_utility_sources AS target;

CREATE TEMP TABLE compromised_publication_sources
ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.status AS previous_status,
    source.url,
    recipe.id AS active_recipe_id
FROM
    companies AS company
    JOIN sources AS source ON source.company_id = company.id
    LEFT JOIN company_news_recipes AS recipe
        ON recipe.source_id = source.id
        AND recipe.status = 'active'
WHERE
    company.company_key = 'yc-infina'
    AND lower(source.url) ~ '^https?://([^/]+[.])?infina[.]vn/blog/?'
    AND (
        recipe.id IS NOT NULL
        OR EXISTS (
            SELECT 1
            FROM feed_items AS item
            WHERE
                item.source_id = source.id
                AND NOT item.is_private
        )
    );

CREATE TEMP TABLE compromised_publication_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    incident.active_recipe_id AS recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN compromised_publication_sources AS incident
        ON incident.source_id = item.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE compromised_publication_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH incident_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.publication_topic_compromise_detected',
        jsonb_build_object(
            'company_key', 'yc-infina',
            'item_count',
                (SELECT count(*) FROM compromised_publication_items),
            'source_count',
                (SELECT count(*) FROM compromised_publication_sources),
            'policy', 'publication-topic-compromise.v1',
            'reason',
                'first_party_publication_dominated_by_unrelated_casino_seo_content',
            'migration',
                '0134_quarantine_publication_topic_compromise'
        )
    WHERE EXISTS (
        SELECT 1 FROM compromised_publication_sources
    )
    RETURNING id
)
INSERT INTO compromised_publication_wave (event_id)
SELECT id FROM incident_started;

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
            'reason', 'publication_topic_compromise_detected',
            'original_published_at', incident.published_at,
            'reversible', true,
            'policy', 'publication-topic-compromise.v1',
            'recipe_id', incident.recipe_id,
            'incident_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration',
                '0134_quarantine_publication_topic_compromise'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    compromised_publication_items AS incident
    CROSS JOIN compromised_publication_wave AS wave
WHERE
    item.id = incident.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: publication_topic_compromise_detected',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM compromised_publication_items AS incident
WHERE raw.id = incident.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    incident.company_id,
    incident.source_id,
    jsonb_build_object(
        'feed_item_id', incident.feed_item_id,
        'raw_crawl_item_id', incident.raw_crawl_item_id,
        'recipe_id', incident.recipe_id,
        'url', incident.url,
        'canonical_url', incident.canonical_url,
        'title', incident.title,
        'published_at', incident.published_at,
        'reason', 'publication_topic_compromise_detected',
        'reversible', true,
        'policy', 'publication-topic-compromise.v1',
        'incident_event_id', wave.event_id,
        'migration',
            '0134_quarantine_publication_topic_compromise'
    )
FROM
    compromised_publication_items AS incident
    CROSS JOIN compromised_publication_wave AS wave;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = 'publication_topic_compromise_detected',
    updated_at = CURRENT_TIMESTAMP
FROM compromised_publication_sources AS incident
WHERE
    recipe.id = incident.active_recipe_id
    AND recipe.status = 'active';

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures =
        GREATEST(state.consecutive_correctness_failures, 3),
    reason = 'publication_topic_compromise_detected',
    metadata = state.metadata || jsonb_build_object(
        'publication_topic_compromise',
        jsonb_build_object(
            'policy', 'publication-topic-compromise.v1',
            'detected_at', CURRENT_TIMESTAMP,
            'migration',
                '0134_quarantine_publication_topic_compromise'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM compromised_publication_sources AS incident
WHERE state.recipe_id = incident.active_recipe_id;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata =
        source.metadata - 'active_recipe_id' - 'recipe_schema_version'
        || jsonb_build_object(
            'quality_disable',
            jsonb_build_object(
                'reason', 'publication_topic_compromise_detected',
                'reversible', true,
                'policy', 'publication-topic-compromise.v1',
                'previous_status', incident.previous_status,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration',
                    '0134_quarantine_publication_topic_compromise'
            )
        ),
    updated_at = CURRENT_TIMESTAMP
FROM compromised_publication_sources AS incident
WHERE source.id = incident.source_id;

UPDATE source_state AS state
SET
    last_attempt_at = CURRENT_TIMESTAMP,
    last_error =
        'publication topic compromise: unrelated casino SEO content',
    consecutive_failures = GREATEST(state.consecutive_failures, 1),
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM compromised_publication_sources AS incident
WHERE state.source_id = incident.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error =
        'source disabled after publication topic compromise detection',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
WHERE
    job.job_type = 'crawl_source'
    AND job.status = 'pending'
    AND job.source_id IN (
        SELECT source_id FROM compromised_publication_sources
    );

UPDATE companies AS company
SET
    metadata = company.metadata || jsonb_build_object(
        'publication_quality_incident',
        jsonb_build_object(
            'state', 'source_disabled',
            'reason', 'publication_topic_compromise_detected',
            'reversible', true,
            'policy', 'publication-topic-compromise.v1',
            'detected_at', CURRENT_TIMESTAMP,
            'migration',
                '0134_quarantine_publication_topic_compromise'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
WHERE
    company.company_key = 'yc-infina'
    AND EXISTS (
        SELECT 1 FROM compromised_publication_sources
    );

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.disabled_for_publication_topic_compromise',
    incident.company_id,
    incident.source_id,
    jsonb_build_object(
        'source_url', incident.url,
        'recipe_id', incident.active_recipe_id,
        'reason', 'publication_topic_compromise_detected',
        'reversible', true,
        'policy', 'publication-topic-compromise.v1',
        'migration',
            '0134_quarantine_publication_topic_compromise'
    )
FROM compromised_publication_sources AS incident;
