-- Finalize the reviewed publication-quality repair after migration 0135.
--
-- A legacy SailPoint blog extraction retained one Discourse thread under an
-- otherwise editorial source. Apply the shared `/discuss/` item policy to any
-- remaining public historical row. Also promote a recovered publication
-- incident from pending to recovered only after its dedicated revalidation
-- crawl completed successfully.

CREATE TEMP TABLE residual_discussion_items
ON COMMIT DROP AS
SELECT DISTINCT ON (item.id)
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    recipe.id AS recipe_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at
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
        lower(item.url) ~ '/(discuss|forum|forums)/'
        OR lower(item.canonical_url) ~ '/(discuss|forum|forums)/'
    )
ORDER BY item.id;

CREATE TEMP TABLE residual_discussion_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.residual_discussion_item_repair_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM residual_discussion_items),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM residual_discussion_items
                ),
            'policy', 'recipe-listing-artifact.v53',
            'migration',
                '0136_finalize_publication_quality_recovery'
        )
    WHERE EXISTS (
        SELECT 1 FROM residual_discussion_items
    )
    RETURNING id
)
INSERT INTO residual_discussion_wave (event_id)
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
            'reason', 'non_editorial_discussion_item',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v53',
            'recipe_id', repair.recipe_id,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration',
                '0136_finalize_publication_quality_recovery'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    residual_discussion_items AS repair
    CROSS JOIN residual_discussion_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: non_editorial_discussion_item',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM residual_discussion_items AS repair
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
        'reason', 'non_editorial_discussion_item',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v53',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0136_finalize_publication_quality_recovery'
    )
FROM
    residual_discussion_items AS repair
    CROSS JOIN residual_discussion_wave AS wave;

CREATE TEMP TABLE recovered_publication_sources
ON COMMIT DROP AS
SELECT DISTINCT ON (source.id)
    source.id AS source_id,
    source.company_id,
    source.url,
    job.completed_at AS revalidated_at
FROM
    sources AS source
    JOIN jobs AS job
        ON job.source_id = source.id
        AND job.job_type = 'crawl_source'
        AND job.status = 'completed'
        AND job.payload->>'trigger'
            = 'publication_topic_compromise_recovery_revalidation'
WHERE
    source.status = 'approved'
    AND source.metadata
        #>> '{publication_quality_incident,state}'
        = 'recovered_pending_revalidation'
ORDER BY source.id, job.completed_at DESC, job.id;

UPDATE sources AS source
SET
    metadata = source.metadata || jsonb_build_object(
        'publication_quality_incident',
        COALESCE(
            source.metadata->'publication_quality_incident',
            '{}'::jsonb
        ) || jsonb_build_object(
            'state', 'recovered',
            'revalidated_at', recovery.revalidated_at,
            'revalidation_trigger',
                'publication_topic_compromise_recovery_revalidation',
            'migration',
                '0136_finalize_publication_quality_recovery'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM recovered_publication_sources AS recovery
WHERE source.id = recovery.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.publication_quality_recovered',
    recovery.company_id,
    recovery.source_id,
    jsonb_build_object(
        'url', recovery.url,
        'revalidated_at', recovery.revalidated_at,
        'trigger',
            'publication_topic_compromise_recovery_revalidation',
        'policy', 'publication-topic-compromise.v1',
        'migration',
            '0136_finalize_publication_quality_recovery'
    )
FROM recovered_publication_sources AS recovery;
