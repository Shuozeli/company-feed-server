-- Quarantine historical HTML/browser rows that entered through an implicit
-- child-subdomain expansion but resolved to a non-production environment or a
-- documentation/help/tutorial host without an editorial URL namespace.
--
-- Exact recipe hosts remain authoritative. Changelog, release, news, press,
-- research, and update paths on documentation-style hosts also remain eligible.
-- The quarantine is reversible if a later explicitly evidenced recipe accepts
-- the same canonical URL.

CREATE TEMP TABLE implicit_unsafe_subdomain_items
ON COMMIT DROP AS
WITH classified AS (
    SELECT DISTINCT ON (item.id)
        item.id AS feed_item_id,
        item.raw_crawl_item_id,
        item.company_id,
        item.source_id,
        recipe.id AS recipe_id,
        item.url,
        item.canonical_url,
        item.title,
        item.published_at,
        normalized.normalized_host,
        normalized.host_label
    FROM
        feed_items AS item
        JOIN company_news_recipes AS recipe
            ON recipe.source_id = item.source_id
            AND recipe.status = 'active'
        CROSS JOIN LATERAL (
            SELECT
                lower(regexp_replace(
                    split_part(
                        regexp_replace(
                            item.canonical_url,
                            '^https?://',
                            '',
                            'i'
                        ),
                        '/',
                        1
                    ),
                    '^www\.',
                    '',
                    'i'
                )) AS normalized_host,
                lower(split_part(
                    regexp_replace(
                        split_part(
                            regexp_replace(
                                item.canonical_url,
                                '^https?://',
                                '',
                                'i'
                            ),
                            '/',
                            1
                        ),
                        '^www\.',
                        '',
                        'i'
                    ),
                    '.',
                    1
                )) AS host_label,
                lower(regexp_replace(
                    split_part(
                        split_part(item.canonical_url, '?', 1),
                        '#',
                        1
                    ),
                    '^https?://[^/]+',
                    '',
                    'i'
                )) AS normalized_path
        ) AS normalized
    WHERE
        NOT item.is_private
        AND item.source_kind IN ('html', 'browser')
        -- An exact host in the recipe is explicit evidence and wins.
        AND NOT EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text(
                recipe.spec->'allowed_hosts'
            ) AS configured(host)
            WHERE normalized.normalized_host = lower(regexp_replace(
                configured.host,
                '^www\.',
                '',
                'i'
            ))
        )
        -- Restrict the repair to hosts admitted by the former implicit
        -- descendant rule; unrelated cross-domain rows are handled by the
        -- ownership policies.
        AND EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text(
                recipe.spec->'allowed_hosts'
            ) AS configured(host)
            WHERE normalized.normalized_host LIKE
                '%.' || lower(regexp_replace(
                    configured.host,
                    '^www\.',
                    '',
                    'i'
                ))
        )
        AND (
            normalized.host_label = ANY(ARRAY[
                'preview',
                'sandbox',
                'stage',
                'staging',
                'test',
                'testing',
                'uat'
            ])
            OR normalized.host_label
                ~ '(^|[-_])(preview|sandbox|stage|staging|test|testing|uat)$'
            OR normalized.host_label ~ 'prod$'
            OR (
                normalized.host_label = ANY(ARRAY[
                    'developer',
                    'developers',
                    'doc',
                    'docs',
                    'documentation',
                    'help',
                    'support',
                    'tutorial',
                    'tutorials',
                    'tutoriales'
                ])
                AND normalized.normalized_path
                    !~ '(^|/|[-_])(blog|blogs|changelog|changelogs|engineering|insights|news|newsroom|press|release|releases|research|stories|updates)(/|[-_]|$)'
            )
        )
    ORDER BY item.id
)
SELECT * FROM classified;

CREATE TEMP TABLE implicit_unsafe_subdomain_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.implicit_unsafe_subdomain_backfill_started',
        jsonb_build_object(
            'item_count',
                (SELECT count(*) FROM implicit_unsafe_subdomain_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM implicit_unsafe_subdomain_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM implicit_unsafe_subdomain_items
                ),
            'policy', 'recipe-host-boundary.v51',
            'migration', '0129_quarantine_implicit_unsafe_subdomains'
        )
    WHERE EXISTS (SELECT 1 FROM implicit_unsafe_subdomain_items)
    RETURNING id
)
INSERT INTO implicit_unsafe_subdomain_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'implicit_unsafe_subdomain',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'recipe-host-boundary.v51',
            'recipe_id', repair.recipe_id,
            'resolved_host', repair.normalized_host,
            'host_label', repair.host_label,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0129_quarantine_implicit_unsafe_subdomains'
        )
    )
FROM
    implicit_unsafe_subdomain_items AS repair
    CROSS JOIN implicit_unsafe_subdomain_wave AS wave
WHERE
    item.id = repair.feed_item_id
    AND NOT item.is_private;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: implicit_unsafe_subdomain',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM implicit_unsafe_subdomain_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'raw_crawl_item_id', repair.raw_crawl_item_id,
        'recipe_id', repair.recipe_id,
        'url', repair.url,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'resolved_host', repair.normalized_host,
        'host_label', repair.host_label,
        'reason', 'implicit_unsafe_subdomain',
        'reversible', true,
        'policy', 'recipe-host-boundary.v51',
        'repair_wave_event_id', wave.event_id,
        'migration', '0129_quarantine_implicit_unsafe_subdomains'
    )
FROM
    implicit_unsafe_subdomain_items AS repair
    CROSS JOIN implicit_unsafe_subdomain_wave AS wave;
