-- Extend generic investor-page title handling to "News Release Details" and
-- remove obsolete provenance whose entire public history repeats one sitewide
-- headline while a healthy active recipe already supplies corrected titles
-- for at least 80% of the same canonical URLs.

CREATE TEMP TABLE superseded_degenerate_sources ON COMMIT DROP AS
WITH source_totals AS (
    SELECT source_id, count(*) AS public_item_count
    FROM feed_items
    WHERE NOT is_private
    GROUP BY source_id
),
repeated_titles AS (
    SELECT
        source_id,
        lower(btrim(regexp_replace(title, '[[:space:]]+', ' ', 'g')))
            AS normalized_title,
        min(title) AS repeated_title,
        count(*) AS repeated_item_count
    FROM feed_items
    WHERE NOT is_private
    GROUP BY
        source_id,
        lower(btrim(regexp_replace(title, '[[:space:]]+', ' ', 'g')))
    HAVING count(*) >= 5
),
replacement_counts AS (
    SELECT
        repeated.source_id,
        repeated.normalized_title,
        count(*) FILTER (
            WHERE EXISTS (
                SELECT 1
                FROM feed_items AS replacement
                JOIN company_news_recipes AS recipe
                    ON recipe.source_id = replacement.source_id
                    AND recipe.status = 'active'
                WHERE
                    replacement.company_id = artifact.company_id
                    AND replacement.canonical_url = artifact.canonical_url
                    AND NOT replacement.is_private
                    AND lower(btrim(replacement.title))
                        <> lower(btrim(artifact.title))
            )
        ) AS replacement_item_count
    FROM repeated_titles AS repeated
    JOIN feed_items AS artifact
        ON artifact.source_id = repeated.source_id
        AND NOT artifact.is_private
        AND lower(
            btrim(regexp_replace(artifact.title, '[[:space:]]+', ' ', 'g'))
        ) = repeated.normalized_title
    GROUP BY repeated.source_id, repeated.normalized_title
)
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    repeated.repeated_title,
    repeated.repeated_item_count,
    totals.public_item_count,
    replacements.replacement_item_count
FROM repeated_titles AS repeated
JOIN source_totals AS totals ON totals.source_id = repeated.source_id
JOIN replacement_counts AS replacements
    ON replacements.source_id = repeated.source_id
    AND replacements.normalized_title = repeated.normalized_title
JOIN sources AS source ON source.id = repeated.source_id
WHERE
    source.status = 'approved'
    AND source.kind IN ('html', 'browser')
    AND repeated.repeated_item_count = totals.public_item_count
    AND replacements.replacement_item_count * 100
        >= repeated.repeated_item_count * 80
    AND NOT EXISTS (
        SELECT 1
        FROM company_news_recipes AS active_recipe
        WHERE
            active_recipe.source_id = source.id
            AND active_recipe.status = 'active'
    );

CREATE TEMP TABLE headline_artifact_items ON COMMIT DROP AS
WITH generic_detail_titles AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        'generic_news_release_detail_title'::text AS reason
    FROM feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
    WHERE
        NOT item.is_private
        AND source.kind IN ('html', 'browser')
        AND lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g')))
            IN ('news release detail', 'news release details')
),
superseded_titles AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        'superseded_sitewide_headline'::text AS reason
    FROM feed_items AS item
    JOIN superseded_degenerate_sources AS source
        ON source.source_id = item.source_id
    WHERE NOT item.is_private
)
SELECT * FROM generic_detail_titles
UNION
SELECT * FROM superseded_titles;

CREATE TEMP TABLE headline_artifact_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.headline_artifact_repair_started',
        jsonb_build_object(
            'item_count', (SELECT count(*) FROM headline_artifact_items),
            'disabled_source_count',
                (SELECT count(*) FROM superseded_degenerate_sources),
            'company_count',
                (SELECT count(DISTINCT company_id) FROM headline_artifact_items),
            'policy', 'recipe-listing-artifact.v11',
            'migration', '0055_quarantine_headline_artifacts'
        )
    WHERE EXISTS (SELECT 1 FROM headline_artifact_items)
    RETURNING id
)
INSERT INTO headline_artifact_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', 'superseded_sitewide_headline_source',
            'repeated_title', repair.repeated_title,
            'public_item_count', repair.public_item_count,
            'replacement_item_count', repair.replacement_item_count,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v11',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    superseded_degenerate_sources AS repair
    CROSS JOIN headline_artifact_wave AS wave
WHERE source.id = repair.source_id;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v11',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    headline_artifact_items AS repair
    CROSS JOIN headline_artifact_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM headline_artifact_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: source disabled after corrected active-recipe replacement'
FROM superseded_degenerate_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'repeated_title', repair.repeated_title,
        'public_item_count', repair.public_item_count,
        'replacement_item_count', repair.replacement_item_count,
        'reason', 'superseded_sitewide_headline_source',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v11',
        'repair_wave_event_id', wave.event_id,
        'migration', '0055_quarantine_headline_artifacts'
    )
FROM
    superseded_degenerate_sources AS repair
    CROSS JOIN headline_artifact_wave AS wave;

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
        'policy', 'recipe-listing-artifact.v11',
        'repair_wave_event_id', wave.event_id,
        'migration', '0055_quarantine_headline_artifacts'
    )
FROM
    headline_artifact_items AS repair
    CROSS JOIN headline_artifact_wave AS wave;
