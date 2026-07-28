-- Reversibly remove collection, archive, utility, and framework-chrome pages
-- that older HTML recipe crawls normalized as articles. The crawler now:
--   * rejects the same terminal editorial collection paths;
--   * treats repeated framework labels as generic title chrome so a real
--     structural or listing-card headline can replace them;
--   * rejects bounded contact utility titles; and
--   * recognizes common terminal-year archive title shapes without rejecting
--     substantive numeric article IDs or editorial year-in-review articles.

CREATE TEMP TABLE navigation_year_archive_and_chrome_artifacts
ON COMMIT DROP AS
WITH base AS (
    SELECT
        item.*,
        split_part(item.canonical_url, '?', 1) AS url_without_query,
        lower(btrim(regexp_replace(
            item.title,
            '[[:space:]]+',
            ' ',
            'g'
        ))) AS normalized_title
    FROM feed_items AS item
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
),
classified AS (
    SELECT
        base.*,
        CASE
            WHEN base.normalized_title = ANY (ARRAY[
                'api success',
                'arrow icon',
                'article summary',
                'case study',
                'code examples',
                'community',
                'corporate news',
                'cve analysis',
                'developer spotlight',
                'explore all',
                'image link',
                'key articles',
                'leadership perspectives',
                'media inquiries',
                'media releases',
                'news',
                'newsletters',
                'our company',
                'partnerships',
                'press details',
                'product updates',
                'release details',
                'snapshots',
                'solution briefs',
                'strategy',
                'sustainability leadership',
                'trending topics',
                'we announced'
            ])
            THEN 'generic_navigation_or_placeholder_title'
            WHEN base.url_without_query ~* '/latest-stories/?$'
            THEN 'localized_latest_stories_collection'
            WHEN base.normalized_title ~ '^contact [^[:space:]]+$'
            THEN 'short_contact_utility_title'
            WHEN
                base.url_without_query ~ '/(19|20)[0-9]{2}/?$'
                AND (
                    base.normalized_title ~
                        '(^|[^[:alnum:]])archives?([^[:alnum:]]|$)'
                    OR base.normalized_title ~
                        '^press releases in (19|20)[0-9]{2}$'
                    OR base.normalized_title = 'search for more'
                    OR base.normalized_title ~
                        '^(19|20)[0-9]{2}[[:space:]]*[-–—|:][[:space:]]*[^0-9]+$'
                    OR base.normalized_title ~
                        '^news (19|20)[0-9]{2}$'
                    OR (
                        array_length(
                            regexp_split_to_array(
                                base.normalized_title,
                                '[[:space:]]+'
                            ),
                            1
                        ) <= 4
                        AND base.normalized_title ~ '(^| )newsroom$'
                    )
                )
            THEN 'year_archive_collection'
            ELSE NULL
        END AS reason
    FROM base
)
SELECT
    classified.id AS feed_item_id,
    classified.raw_crawl_item_id,
    classified.company_id,
    classified.source_id,
    classified.canonical_url,
    classified.title,
    classified.published_at,
    classified.reason
FROM classified
WHERE classified.reason IS NOT NULL;

CREATE TEMP TABLE navigation_year_archive_and_chrome_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.navigation_year_archive_and_chrome_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM navigation_year_archive_and_chrome_artifacts
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM navigation_year_archive_and_chrome_artifacts
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM navigation_year_archive_and_chrome_artifacts
                ),
            'reason_counts',
                (
                    SELECT jsonb_object_agg(reason, item_count)
                    FROM (
                        SELECT reason, count(*) AS item_count
                        FROM navigation_year_archive_and_chrome_artifacts
                        GROUP BY reason
                    ) AS counts
                ),
            'policy', 'recipe-listing-artifact.v18',
            'migration',
                '0062_quarantine_navigation_year_archive_and_chrome_titles'
        )
    WHERE EXISTS (
        SELECT 1 FROM navigation_year_archive_and_chrome_artifacts
    )
    RETURNING id
)
INSERT INTO navigation_year_archive_and_chrome_wave (event_id)
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
            'policy', 'recipe-listing-artifact.v18',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    navigation_year_archive_and_chrome_artifacts AS repair
    CROSS JOIN navigation_year_archive_and_chrome_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM navigation_year_archive_and_chrome_artifacts AS repair
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
        'policy', 'recipe-listing-artifact.v18',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0062_quarantine_navigation_year_archive_and_chrome_titles'
    )
FROM
    navigation_year_archive_and_chrome_artifacts AS repair
    CROSS JOIN navigation_year_archive_and_chrome_wave AS wave;
