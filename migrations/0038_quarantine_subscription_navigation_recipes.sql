-- Subscription and email-alert utility pages can contain enough links and body
-- text to resemble an editorial listing, but they are not durable news
-- publications. Retire any active recipe rooted at one of these paths and
-- reversibly quarantine its normalized navigation artifacts.

CREATE TEMP TABLE subscription_navigation_recipes ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    'subscription_navigation_publication'::text AS reason
FROM company_news_recipes AS recipe
WHERE
    recipe.status = 'active'
    AND EXISTS (
        SELECT 1
        FROM regexp_split_to_table(
            lower(split_part(split_part(recipe.spec->>'publication_url', '?', 1), '#', 1)),
            '/'
        ) AS path(segment)
        WHERE
            path.segment IN (
                'email-alert',
                'email-alerts',
                'investor-email-alert',
                'investor-email-alerts',
                'news-alert',
                'news-alerts',
                'press-release-alert',
                'press-release-alerts',
                'subscribe',
                'subscriptions',
                'unsubscribe'
            )
            OR path.segment LIKE 'subscribe-%'
            OR path.segment LIKE 'subscribe\_%' ESCAPE '\'
    );

CREATE TEMP TABLE subscription_navigation_items ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    repair.reason
FROM subscription_navigation_recipes AS repair
JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private
UNION ALL
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    'generic_investor_navigation_title'::text AS reason
FROM company_news_recipes AS recipe
JOIN feed_items AS item ON item.source_id = recipe.source_id
WHERE
    NOT item.is_private
    AND lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g')))
        IN (
            'conversion price',
            'dividend information',
            'proxy statements',
            'tax information'
        )
    AND NOT EXISTS (
        SELECT 1
        FROM subscription_navigation_recipes AS repair
        WHERE repair.source_id = item.source_id
    );

CREATE TEMP TABLE subscription_navigation_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.subscription_navigation_repair_started',
        jsonb_build_object(
            'recipe_count', (SELECT count(*) FROM subscription_navigation_recipes),
            'item_count', (SELECT count(*) FROM subscription_navigation_items),
            'policy', 'recipe-listing-artifact.v8',
            'migration', '0038_quarantine_subscription_navigation_recipes'
        )
    WHERE
        EXISTS (SELECT 1 FROM subscription_navigation_recipes)
        OR EXISTS (SELECT 1 FROM subscription_navigation_items)
    RETURNING id
)
INSERT INTO subscription_navigation_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM subscription_navigation_recipes AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures = GREATEST(
        state.consecutive_correctness_failures,
        3
    ),
    reason = repair.reason,
    metadata = state.metadata || jsonb_build_object(
        'subscription_navigation_repair',
        jsonb_build_object(
            'policy', 'recipe-listing-artifact.v8',
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM
    subscription_navigation_recipes AS repair
    CROSS JOIN subscription_navigation_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'reversible', true,
            'policy', 'recipe-listing-artifact.v8',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    subscription_navigation_items AS repair
    CROSS JOIN subscription_navigation_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM subscription_navigation_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: recipe staled by subscription navigation repair'
FROM subscription_navigation_recipes AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'reason', repair.reason,
        'rebuild_required', true,
        'policy', 'recipe-listing-artifact.v8',
        'repair_wave_event_id', wave.event_id,
        'migration', '0038_quarantine_subscription_navigation_recipes'
    )
FROM
    subscription_navigation_recipes AS repair
    CROSS JOIN subscription_navigation_wave AS wave;

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
        'policy', 'recipe-listing-artifact.v8',
        'repair_wave_event_id', wave.event_id,
        'migration', '0038_quarantine_subscription_navigation_recipes'
    )
FROM
    subscription_navigation_items AS repair
    CROSS JOIN subscription_navigation_wave AS wave;
