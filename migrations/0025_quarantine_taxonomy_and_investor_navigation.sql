-- A cross-site audit found a provider-neutral tail of taxonomy profiles,
-- pagination pages, scoped news-wire category lists, and ordinary investor or
-- media navigation destinations carrying misleading article metadata. The
-- crawler now rejects these structural patterns without any site parser.

CREATE TEMP TABLE recipe_taxonomy_navigation_artifacts ON COMMIT DROP AS
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
            WHEN item.canonical_resource ~
                '/(categoria|categorias|content-type|contributor|contributors|label|label-name|labels|production-platform|production_platform|type|types)(/|$)'
                OR item.canonical_resource ~ '/blogs?/hubs?(/|$)'
                THEN 'taxonomy_collection_path'
            WHEN item.canonical_resource ~ '/(p[0-9]+|page-[0-9]+)/?$'
                THEN 'pagination_collection_path'
            WHEN item.canonical_resource ~
                '/[^/]*latest-news/[^/]+-list/?$'
                OR item.canonical_resource ~
                    '/(multimedia/multimedia-list|photos/photos-list|videos/videos-list)/?$'
                THEN 'news_wire_category_list'
            WHEN item.canonical_resource ~
                '/(about|accessibility|accessibility-statement|acquisitions|all|all-posts|all-stories|ambassadors|analyst-coverage|analysts|annual-filings|annual-meeting|annual-meeting-materials|annual-proxy|annual-report|articles-list|asset-alerts|asset-library|brand-guide|brand-resources|capabilities-statement|clinical-trials|committee-charters|company-announcements|company-fact-sheet|company-overview|company-statements|contact|contact-ir|contacts|corporate-governance-guidelines|corporate-press-kits|customer-stories|customers|editorial-policy|financial-releases|image-library|investor-conferences|investor-presentations|investor-resources|ir-updates|leadership|logo-gallery|media-assets|media-center-search|media-library|multimedia|news-search|partnering-news|press-kits|quarterly-earnings|quarterly-earnings-materials|releases|updates-and-statements|view-all|xbrl-files)/?$'
                THEN 'terminal_navigation_hub'
            WHEN item.title_key = ANY (ARRAY[
                'accessibility statement',
                'acquisitions',
                'all blog posts',
                'all multimedia',
                'all news releases',
                'all photos',
                'all posts',
                'all stories',
                'all videos',
                'ambassadors',
                'analyst coverage',
                'analysts',
                'annual filings',
                'annual meeting',
                'annual meeting materials',
                'annual proxy',
                'annual report',
                'asset alerts',
                'asset library',
                'brand guide',
                'brand resources',
                'capabilities statement',
                'chevron_right',
                'click here',
                'clinical trials',
                'committee charters',
                'company announcements',
                'company fact sheet',
                'company overview',
                'company statements',
                'contact investor relations',
                'contact ir',
                'contacts',
                'corporate governance guidelines',
                'corporate press kits',
                'customer stories',
                'customers',
                'editorial policy',
                'financial press releases and webcasts',
                'image library',
                'investor conferences',
                'investor presentations',
                'investor resources',
                'ir updates',
                'last_page',
                'leadership',
                'media asset library',
                'media information',
                'media library & contacts',
                'multimedia',
                'partnering news',
                'quarterly earnings',
                'quarterly earnings materials',
                'updates and statements',
                'view all',
                'xbrl files'
            ])
                THEN 'generic_navigation_title'
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

CREATE TEMP TABLE taxonomy_navigation_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.taxonomy_navigation_repair_started',
        jsonb_build_object(
            'item_count', count(*),
            'source_count', count(DISTINCT source_id),
            'policy', 'recipe-listing-artifact.v3',
            'migration', '0025_quarantine_taxonomy_and_investor_navigation'
        )
    FROM recipe_taxonomy_navigation_artifacts
    HAVING count(*) > 0
    RETURNING id
)
INSERT INTO taxonomy_navigation_repair_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', artifact.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v3',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    recipe_taxonomy_navigation_artifacts AS artifact
    CROSS JOIN taxonomy_navigation_repair_wave AS wave
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_taxonomy_navigation_artifacts AS artifact
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
        'policy', 'recipe-listing-artifact.v3',
        'repair_wave_event_id', wave.event_id,
        'migration', '0025_quarantine_taxonomy_and_investor_navigation'
    )
FROM
    recipe_taxonomy_navigation_artifacts AS artifact
    CROSS JOIN taxonomy_navigation_repair_wave AS wave;
