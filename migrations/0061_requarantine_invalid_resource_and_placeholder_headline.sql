-- Migration 0060 can race with a crawl that began on the preceding binary:
-- that in-flight attempt may successfully normalize and release the same
-- invalid detail shell after 0060 commits. Reapply the now-enforced policy
-- after the guarded crawler is deployed. The predicate remains generic and
-- idempotent.

CREATE TEMP TABLE invalid_resource_requarantine ON COMMIT DROP AS
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

CREATE TEMP TABLE invalid_resource_requarantine_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.invalid_resource_requarantine_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM invalid_resource_requarantine),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM invalid_resource_requarantine
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM invalid_resource_requarantine
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM invalid_resource_requarantine
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v17',
            'migration',
                '0061_requarantine_invalid_resource_and_placeholder_headline'
        )
    WHERE EXISTS (SELECT 1 FROM invalid_resource_requarantine)
    RETURNING id
)
INSERT INTO invalid_resource_requarantine_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v17',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    invalid_resource_requarantine AS repair
    CROSS JOIN invalid_resource_requarantine_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM invalid_resource_requarantine AS repair
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
        'policy', 'recipe-listing-artifact.v17',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0061_requarantine_invalid_resource_and_placeholder_headline'
    )
FROM
    invalid_resource_requarantine AS repair
    CROSS JOIN invalid_resource_requarantine_wave AS wave;
