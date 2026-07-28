-- A CMS collection hub may vary its page title between a branded collection
-- breadcrumb and a responsive-navigation label such as "Our Company Sub
-- Menu". The runtime crawler now rejects short titles ending in "sub menu" or
-- "submenu". Quarantine existing public HTML items with that navigation-only
-- title so a replay cannot make the hub visible while its recipe is rechecked.

CREATE TEMP TABLE submenu_navigation_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
WHERE
    NOT item.is_private
    AND source.kind = 'html'
    AND array_length(
        regexp_split_to_array(
            lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g'))),
            '[[:space:]]+'
        ),
        1
    ) <= 5
    AND lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g'))) ~
        '(^|[[:space:]])sub[[:space:]-]?menu$';

CREATE TEMP TABLE submenu_navigation_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.submenu_navigation_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM submenu_navigation_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM submenu_navigation_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM submenu_navigation_items
                ),
            'policy', 'recipe-listing-artifact.v32',
            'migration',
                '0088_quarantine_submenu_navigation_titles'
        )
    WHERE EXISTS (
        SELECT 1 FROM submenu_navigation_items
    )
    RETURNING id
)
INSERT INTO submenu_navigation_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'generic_listing_title',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v32',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    submenu_navigation_items AS repair
    CROSS JOIN submenu_navigation_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: generic_listing_title',
    normalized_feed_item_id = NULL
FROM submenu_navigation_items AS repair
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
        'reason', 'generic_listing_title',
        'reversible', true,
        'policy', 'recipe-listing-artifact.v32',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0088_quarantine_submenu_navigation_titles'
    )
FROM
    submenu_navigation_items AS repair
    CROSS JOIN submenu_navigation_wave AS wave;
