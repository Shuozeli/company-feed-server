-- Static CMS fixtures and self-titled collection roots are not articles.
-- Backfill the shared placeholder/utility policy while preserving substantive
-- articles whose subject legitimately begins with "Test".

CREATE TEMP TABLE cms_fixture_and_collection_items
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
        lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        ))) AS normalized_title,
        regexp_replace(
            lower(regexp_replace(
                regexp_replace(
                    split_part(
                        split_part(item.canonical_url, '?', 1),
                        '#',
                        1
                    ),
                    '/(default|index)(\.(aspx|html|asp|htm|php))?/?$',
                    ''
                ),
                '/$',
                ''
            )),
            '^.*/',
            ''
        ) AS canonical_terminal,
        char_length(COALESCE(item.body_text, '')) AS body_chars
    FROM feed_items AS item
    WHERE NOT item.is_private
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
FROM normalized
WHERE
    normalized_title = 'test post created'
    OR normalized_title ~ '^test post[[:space:]]+[0-9]+$'
    OR normalized_title LIKE 'test post%please ignore%'
    OR normalized_title ~ '^test article[[:space:]]+[0-9]+$'
    OR normalized_title ~ '^test in the news[[:space:]]+[0-9]+$'
    OR normalized_title IN (
        'another item to trigger pagination',
        'carousel display of multiple assets',
        'gallery display of multiple assets',
        'inline display of multiple assets'
    )
    OR (
        title LIKE 'test %'
        AND body_chars <= 200
    )
    OR (
        canonical_terminal IN (
            'articles',
            'blog',
            'blogs',
            'insights',
            'latest-news',
            'news',
            'news-releases',
            'newsroom',
            'press-releases',
            'resources',
            'stories',
            'updates'
        )
        AND normalized_title =
            replace(replace(canonical_terminal, '-', ' '), '_', ' ')
    );

CREATE TEMP TABLE cms_fixture_and_collection_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'feed_item.cms_fixture_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM cms_fixture_and_collection_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM cms_fixture_and_collection_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM cms_fixture_and_collection_items
                ),
            'policy', 'cms-placeholder-and-collection.v1',
            'migration',
                '0099_quarantine_cms_fixtures_and_collection_roots'
        )
    WHERE EXISTS (
        SELECT 1 FROM cms_fixture_and_collection_items
    )
    RETURNING id
)
INSERT INTO cms_fixture_and_collection_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'cms_fixture_or_collection_root',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'cms-placeholder-and-collection.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    cms_fixture_and_collection_items AS repair
    CROSS JOIN cms_fixture_and_collection_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: cms_fixture_or_collection_root',
    normalized_feed_item_id = NULL
FROM cms_fixture_and_collection_items AS repair
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
        'reason', 'cms_fixture_or_collection_root',
        'reversible', true,
        'policy', 'cms-placeholder-and-collection.v1',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0099_quarantine_cms_fixtures_and_collection_roots'
    )
FROM
    cms_fixture_and_collection_items AS repair
    CROSS JOIN cms_fixture_and_collection_wave AS wave;
