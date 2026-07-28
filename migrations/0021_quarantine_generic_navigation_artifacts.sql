-- Generic investor-relations templates and CMS navigation hubs can emit
-- misleading Article/OpenGraph metadata. The crawler now rejects these
-- provider-neutral paths/titles plus author and month-archive collections.
-- Preserve raw evidence while removing the bounded historical artifacts from
-- public reads.

CREATE TEMP TABLE recipe_generic_navigation_artifacts ON COMMIT DROP AS
WITH latest_recipe_source AS (
    SELECT DISTINCT ON (recipe.source_id)
        recipe.source_id
    FROM company_news_recipes AS recipe
    ORDER BY recipe.source_id, recipe.version DESC
), recipe_items AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        lower(btrim(item.title)) AS title_key,
        regexp_replace(
            lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1)),
            '/(default|index)(\.(asp|aspx|htm|html|php))?/?$',
            ''
        ) AS canonical_resource
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE NOT item.is_private
), classified AS (
    SELECT
        item.*,
        CASE
            WHEN item.canonical_resource ~ '/(user|users)(/|$)'
                THEN 'author_profile_path'
            WHEN item.canonical_resource ~
                '/(annual-reports|awards|awards-and-recognition|awards-recognition|board-of-directors|case-studies|clinical-evidence|code-of-conduct|committee-composition|conferences-and-presentations|congresses|contact-us|corporate-governance|corporate-profile|dividend-history|email-alerts|event-calendar|events-calendar|events-and-presentations|events-presentations|financial-information|financials|frequently-asked-questions|governance|image-gallery|investor-contacts|investor-email-alerts|investor-faqs|investor-overview|investors|ir-calendar|management|media-contacts|media-hub|media-kit|media-resources|podcasts|resources|sec-filings|stock-information|sustainability|whistleblower-hotline)/?$'
                THEN 'terminal_navigation_hub'
            WHEN item.title_key = ANY (ARRAY[
                'additional insights',
                'annual reports',
                'awards & recognition',
                'awards and recognition',
                'board of directors',
                'case studies',
                'clinical evidence',
                'code of conduct',
                'committee composition',
                'conferences & presentations',
                'conferences and presentations',
                'congresses',
                'contact us',
                'corporate governance',
                'corporate profile',
                'data & analytics',
                'data and analytics',
                'dividend history',
                'earnings releases',
                'email alerts',
                'event calendar',
                'events & presentations',
                'events and presentations',
                'events calendar',
                'financial',
                'financial information',
                'frequently asked questions',
                'glossary',
                'governance',
                'image gallery',
                'insights & media',
                'insights and media',
                'investor contacts',
                'investor email alerts',
                'investor faqs',
                'investor overview',
                'investors',
                'ir calendar',
                'latest stories',
                'management',
                'media contacts',
                'media hub',
                'media kit',
                'media relations',
                'media resources',
                'news details',
                'podcasts',
                'press kit',
                'product resources',
                'sec filings',
                'stock information',
                'vulnerability disclosure',
                'whistleblower hotline'
            ])
                THEN 'generic_navigation_title'
            WHEN
                array_length(
                    regexp_split_to_array(item.title_key, '[[:space:]]+'),
                    1
                ) <= 4
                AND item.title_key !~ '[0-9]'
                AND item.title_key ~
                    ' (events|in the news|media|podcasts|research|shorts|tv|white papers)$'
                THEN 'branded_navigation_title'
            WHEN
                item.canonical_resource ~ '/[0-9]{4}/(0?[1-9]|1[0-2])/?$'
                AND item.title_key ~
                    '^(january|february|march|april|may|june|july|august|september|october|november|december)[[:space:]]+[0-9]{4}$'
                THEN 'month_archive_collection'
            ELSE NULL
        END AS reason
    FROM recipe_items AS item
)
SELECT
    feed_item_id,
    raw_crawl_item_id,
    company_id,
    source_id,
    canonical_url,
    title,
    reason
FROM classified
WHERE reason IS NOT NULL;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', artifact.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v1',
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM recipe_generic_navigation_artifacts AS artifact
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_generic_navigation_artifacts AS artifact
WHERE raw.id = artifact.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    artifact.company_id,
    artifact.source_id,
    jsonb_build_object(
        'feed_item_id', artifact.feed_item_id,
        'canonical_url', artifact.canonical_url,
        'title', artifact.title,
        'reason', artifact.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v1',
        'repair_wave_event_id', 103074,
        'migration', '0021_quarantine_generic_navigation_artifacts'
    )
FROM recipe_generic_navigation_artifacts AS artifact;
