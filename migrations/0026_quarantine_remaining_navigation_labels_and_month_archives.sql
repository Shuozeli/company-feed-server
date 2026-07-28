-- A second corpus-wide audit found two provider-neutral gaps left after the
-- taxonomy repair: terminal YYYY/MM archives with nonstandard titles, and a
-- small vocabulary of unmistakable media/investor navigation labels. The
-- crawler now rejects both, plus multi-article card grids without individual
-- article metadata.

CREATE TEMP TABLE recipe_remaining_navigation_artifacts ON COMMIT DROP AS
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
        lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g'))) AS title_key,
        regexp_replace(
            regexp_replace(
                lower(split_part(split_part(item.canonical_url, '?', 1), '#', 1)),
                '/(default|index)(\.(asp|aspx|htm|html|php))?/?$',
                ''
            ),
            '\.(asp|aspx|htm|html|php)/?$',
            ''
        ) AS canonical_resource
    FROM latest_recipe_source AS recipe
    JOIN feed_items AS item ON item.source_id = recipe.source_id
    WHERE NOT item.is_private
), classified AS (
    SELECT
        item.*,
        CASE
            WHEN item.canonical_resource ~ '^https?://[^/]+/?$'
                THEN 'site_root_navigation'
            WHEN item.canonical_resource ~ '/[0-9]{4}/(0?[1-9]|1[0-2])/?$'
                THEN 'month_archive_path'
            WHEN item.canonical_resource ~
                '/(all-stories|contact-media-relations|conditions-generales-dutilisation|dossiers-de-presse|earnings-calls|fact-sheets|information-request-form|logo-gallery|media-coverage|media-faqs|media-gallery|media-relations-contacts|multimedia-library|newsletter|non-gaap-reconciliation|photos-and-videos|posts|presentations|presentations-and-webcasts|press-room|product-press-kits|product-reviews|publications|quarterly-earnings|reactions|reseaux-sociaux|revues-de-presse|shareholder-services|site-map|sitemap|social-channels|social-media-disclosure|statements|success-stories|terms-of-use|video-hub|video-library|webcasts|whitepapers)/?$'
                THEN 'terminal_navigation_hub'
            WHEN item.title_key = ANY (ARRAY[
                'communiqués de presse',
                'conditions générales d''utilisation (cgu)',
                'developers',
                'dossiers de presse',
                'earnings calls',
                'fact sheets',
                'financial news overview',
                'general news overview',
                'home',
                'in the press',
                'information request form',
                'media coverage',
                'media faqs',
                'media gallery',
                'media relations contacts',
                'multimedia library',
                'newsletter',
                'non-gaap reconciliation',
                'overview',
                'photos and videos',
                'presentations',
                'presentations & webcasts',
                'presentations and webcasts',
                'press kits',
                'press room',
                'product press kits',
                'product reviews',
                'publications',
                'réactions',
                'réseaux sociaux',
                'revues de presse',
                'scientific publications',
                'shareholder services',
                'site map',
                'social channels',
                'social media disclosure',
                'statements',
                'success stories',
                'terms of use',
                'thank you for subscribing',
                'thank you for subscribing.',
                'video hub',
                'video library',
                'view article',
                'webcasts',
                'whitepapers',
                'your browser is unsupported'
            ])
                THEN 'generic_navigation_title'
            WHEN item.title_key ~
                '^(home|product reviews|success stories)[[:space:]]*[|:–—-][[:space:]]*.+'
                THEN 'generic_navigation_title_with_site_suffix'
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

CREATE TEMP TABLE remaining_navigation_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.remaining_navigation_repair_started',
        jsonb_build_object(
            'item_count', count(*),
            'source_count', count(DISTINCT source_id),
            'policy', 'recipe-listing-artifact.v4',
            'migration', '0026_quarantine_remaining_navigation_labels_and_month_archives'
        )
    FROM recipe_remaining_navigation_artifacts
    HAVING count(*) > 0
    RETURNING id
)
INSERT INTO remaining_navigation_repair_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v4',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    recipe_remaining_navigation_artifacts AS artifact
    CROSS JOIN remaining_navigation_repair_wave AS wave
WHERE item.id = artifact.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || artifact.reason,
    normalized_feed_item_id = NULL
FROM recipe_remaining_navigation_artifacts AS artifact
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
        'policy', 'recipe-listing-artifact.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0026_quarantine_remaining_navigation_labels_and_month_archives'
    )
FROM
    recipe_remaining_navigation_artifacts AS artifact
    CROSS JOIN remaining_navigation_repair_wave AS wave;
