-- Reversibly remove detail shells whose resource identifier is empty and
-- generic CMS placeholder headlines that older article-page validation
-- accepted. The crawler now rejects invalid resource query values before
-- canonicalization and treats the exact title "Headline" as page chrome.

CREATE TEMP TABLE invalid_resource_and_placeholder_artifacts ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    CASE
        WHEN item.url ~*
            '[?&](article|article_id|articleid|cid|content|content_id|contentid|id|item|item_id|itemid|news_id|newsid|nid|p|page_id|pageid|post|post_id|postid|record_id|recordid|release_id|releaseid|story|story_id|storyid)=(&|#|$)'
        THEN 'empty_resource_query'
        ELSE 'generic_placeholder_headline'
    END AS reason
FROM feed_items AS item
WHERE
    NOT item.is_private
    AND item.source_kind IN ('html', 'browser')
    AND (
        item.url ~*
            '[?&](article|article_id|articleid|cid|content|content_id|contentid|id|item|item_id|itemid|news_id|newsid|nid|p|page_id|pageid|post|post_id|postid|record_id|recordid|release_id|releaseid|story|story_id|storyid)=(&|#|$)'
        OR lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        ))) = 'headline'
    );

CREATE TEMP TABLE invalid_resource_and_placeholder_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.invalid_resource_and_placeholder_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM invalid_resource_and_placeholder_artifacts
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM invalid_resource_and_placeholder_artifacts
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM invalid_resource_and_placeholder_artifacts
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM invalid_resource_and_placeholder_artifacts
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v16',
            'migration',
                '0060_quarantine_invalid_resource_and_placeholder_headline'
        )
    WHERE EXISTS (
        SELECT 1 FROM invalid_resource_and_placeholder_artifacts
    )
    RETURNING id
)
INSERT INTO invalid_resource_and_placeholder_wave (event_id)
SELECT id FROM repair_started;

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
            'policy', 'recipe-listing-artifact.v16',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    invalid_resource_and_placeholder_artifacts AS repair
    CROSS JOIN invalid_resource_and_placeholder_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM invalid_resource_and_placeholder_artifacts AS repair
WHERE raw.id = repair.raw_crawl_item_id;

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
        'reversible', true,
        'policy', 'recipe-listing-artifact.v16',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0060_quarantine_invalid_resource_and_placeholder_headline'
    )
FROM
    invalid_resource_and_placeholder_artifacts AS repair
    CROSS JOIN invalid_resource_and_placeholder_wave AS wave;
