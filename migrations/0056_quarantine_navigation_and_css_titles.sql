-- Remove two generic HTML extraction artifacts:
--   * investor-navigation pages titled "Why Invest"
--   * site-brand headings contaminated by embedded SVG/CSS rules
--
-- Both predicates require HTML/browser provenance and structural evidence in
-- the URL or title, so legitimate editorial headlines remain untouched.

CREATE TEMP TABLE navigation_and_css_title_artifacts ON COMMIT DROP AS
WITH candidates AS (
    SELECT
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        item.canonical_url,
        item.title,
        CASE
            WHEN
                lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) = 'why invest'
                AND lower(item.canonical_url)
                    ~ '/why-invest(/|$|[?])'
            THEN 'investor_navigation_title'
            ELSE 'embedded_css_title'
        END AS reason
    FROM feed_items AS item
    JOIN sources AS source ON source.id = item.source_id
    WHERE
        NOT item.is_private
        AND source.kind IN ('html', 'browser')
        AND (
            (
                lower(btrim(regexp_replace(
                    item.title,
                    '[[:space:]]+',
                    ' ',
                    'g'
                ))) = 'why invest'
                AND lower(item.canonical_url)
                    ~ '/why-invest(/|$|[?])'
            )
            OR (
                item.title LIKE '%{%'
                AND item.title LIKE '%}%'
                AND item.title LIKE '%:%'
                AND item.title LIKE '%;%'
                AND (
                    lower(item.title) LIKE '%.cls-%'
                    OR lower(item.title) LIKE '%stroke-width:%'
                    OR lower(item.title) LIKE '%fill:%'
                    OR lower(item.title) LIKE '%font-family:%'
                    OR lower(item.title) LIKE '%display:%'
                    OR lower(item.title) LIKE '%visibility:%'
                )
            )
        )
)
SELECT * FROM candidates;

CREATE TEMP TABLE navigation_and_css_title_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.navigation_and_css_title_repair_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM navigation_and_css_title_artifacts),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM navigation_and_css_title_artifacts
                ),
            'policy', 'recipe-listing-artifact.v12',
            'migration', '0056_quarantine_navigation_and_css_titles'
        )
    WHERE EXISTS (SELECT 1 FROM navigation_and_css_title_artifacts)
    RETURNING id
)
INSERT INTO navigation_and_css_title_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v12',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    navigation_and_css_title_artifacts AS repair
    CROSS JOIN navigation_and_css_title_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM navigation_and_css_title_artifacts AS repair
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
        'reason', repair.reason,
        'reversible', true,
        'policy', 'recipe-listing-artifact.v12',
        'repair_wave_event_id', wave.event_id,
        'migration', '0056_quarantine_navigation_and_css_titles'
    )
FROM
    navigation_and_css_title_artifacts AS repair
    CROSS JOIN navigation_and_css_title_wave AS wave;
