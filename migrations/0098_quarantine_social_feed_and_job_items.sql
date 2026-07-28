-- Social-feed CMS records and job listings can leak into otherwise valid
-- corporate RSS feeds. They are not company news. The shared content policy
-- now rejects these conservative path scopes while preserving editorial
-- articles nested beneath an explicit blog/news root.

CREATE TEMP TABLE social_and_job_utility_items
ON COMMIT DROP AS
WITH normalized AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.url,
        item.canonical_url,
        item.title,
        item.published_at,
        lower(split_part(split_part(item.url, '?', 1), '#', 1))
            AS item_url,
        lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1))
            AS canonical_item_url
    FROM feed_items AS item
    WHERE NOT item.is_private
),
classified AS (
    SELECT
        item.*,
        (
            item.item_url ~
                '/(instagram-feed|social-feed|job|job-openings|jobs|open-roles|vacancies)(/|$)'
            OR item.canonical_item_url ~
                '/(instagram-feed|social-feed|job|job-openings|jobs|open-roles|vacancies)(/|$)'
            OR item.item_url ~ '/career_[^/]+/?$'
            OR item.canonical_item_url ~ '/career_[^/]+/?$'
        ) AS has_utility_scope,
        (
            item.item_url ~
                '/(blog|blogs|changelog|changelogs|company-news|engineering|insights|news|newsroom|press|press-release|press-releases|pressrelease|pressreleases|product-updates|release-notes|research|stories|updates|what-s-new|whats-new)(/|$)'
            OR item.canonical_item_url ~
                '/(blog|blogs|changelog|changelogs|company-news|engineering|insights|news|newsroom|press|press-release|press-releases|pressrelease|pressreleases|product-updates|release-notes|research|stories|updates|what-s-new|whats-new)(/|$)'
        ) AS has_explicit_editorial_scope
    FROM normalized AS item
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    url,
    canonical_url,
    title,
    published_at
FROM classified
WHERE
    has_utility_scope
    AND NOT has_explicit_editorial_scope;

CREATE TEMP TABLE social_and_job_utility_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'feed_item.social_and_job_utility_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM social_and_job_utility_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM social_and_job_utility_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM social_and_job_utility_items
                ),
            'policy', 'non-editorial-utility-item.v2',
            'migration',
                '0098_quarantine_social_feed_and_job_items'
        )
    WHERE EXISTS (
        SELECT 1 FROM social_and_job_utility_items
    )
    RETURNING id
)
INSERT INTO social_and_job_utility_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'non_editorial_social_or_job_utility',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'non-editorial-utility-item.v2',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    social_and_job_utility_items AS repair
    CROSS JOIN social_and_job_utility_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: non_editorial_social_or_job_utility',
    normalized_feed_item_id = NULL
FROM social_and_job_utility_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

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
        'reason', 'non_editorial_social_or_job_utility',
        'reversible', true,
        'policy', 'non-editorial-utility-item.v2',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0098_quarantine_social_feed_and_job_items'
    )
FROM
    social_and_job_utility_items AS repair
    CROSS JOIN social_and_job_utility_wave AS wave;
