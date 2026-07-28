-- Older recipes could validate a structurally healthy global press-wire root
-- before company-identity scope was enforced. The current runtime has already
-- marked these recipes stale, but their historical rows must also leave the
-- public view. Preserve every row and its raw provenance under a reversible
-- company-scope quarantine.

CREATE TEMP TABLE stale_unscoped_shared_recipe_items
ON COMMIT DROP AS
WITH shared_domains(domain) AS (
    VALUES
        ('accessnewswire.com'),
        ('barchart.com'),
        ('benzinga.com'),
        ('biospace.com'),
        ('bloomberg.com'),
        ('businesswire.com'),
        ('einpresswire.com'),
        ('finance.yahoo.com'),
        ('forbes.com'),
        ('globenewswire.com'),
        ('investing.com'),
        ('marketbeat.com'),
        ('marketscreener.com'),
        ('marketwatch.com'),
        ('msn.com'),
        ('nasdaq.com'),
        ('newsfilecorp.com'),
        ('prnewswire.com'),
        ('reuters.com'),
        ('seekingalpha.com'),
        ('stocktitan.net'),
        ('tipranks.com'),
        ('tradingview.com')
),
stale_recipes AS (
    SELECT
        recipe.id AS recipe_id,
        recipe.company_id,
        recipe.source_id,
        recipe.spec->>'publication_url' AS publication_url,
        lower(split_part(
            split_part(
                recipe.spec->>'publication_url',
                '://',
                2
            ),
            '/',
            1
        )) AS publication_host,
        recipe.stale_reason
    FROM company_news_recipes AS recipe
    WHERE
        recipe.status = 'stale'
        AND recipe.stale_reason = ANY (ARRAY[
            'company_scope_relevance_below_minimum',
            'publication_url_lacks_editorial_scope',
            'unscoped_third_party_publication'
        ])
),
targets AS (
    SELECT stale.*
    FROM
        stale_recipes AS stale
        JOIN shared_domains AS shared
            ON stale.publication_host = shared.domain
            OR stale.publication_host LIKE '%.' || shared.domain
)
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    target.recipe_id,
    target.publication_url,
    target.stale_reason,
    'stale_unscoped_shared_recipe'::text AS reason
FROM
    targets AS target
    JOIN feed_items AS item
        ON item.source_id = target.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE stale_unscoped_shared_recipe_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.stale_unscoped_shared_recipe_repair_started',
        jsonb_build_object(
            'item_count',
                (
                    SELECT count(*)
                    FROM stale_unscoped_shared_recipe_items
                ),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM stale_unscoped_shared_recipe_items
                ),
            'source_count',
                (
                    SELECT count(DISTINCT source_id)
                    FROM stale_unscoped_shared_recipe_items
                ),
            'recipe_count',
                (
                    SELECT count(DISTINCT recipe_id)
                    FROM stale_unscoped_shared_recipe_items
                ),
            'policy', 'company-scope-relevance.v2',
            'migration',
                '0070_quarantine_stale_unscoped_shared_recipe_items'
        )
    WHERE EXISTS (
        SELECT 1 FROM stale_unscoped_shared_recipe_items
    )
    RETURNING id
)
INSERT INTO stale_unscoped_shared_recipe_wave (event_id)
SELECT id FROM repair_started;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'recipe_id', repair.recipe_id,
            'publication_url', repair.publication_url,
            'recipe_stale_reason', repair.stale_reason,
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'company-scope-relevance.v2',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    stale_unscoped_shared_recipe_items AS repair
    CROSS JOIN stale_unscoped_shared_recipe_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM stale_unscoped_shared_recipe_items AS repair
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
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'recipe_stale_reason', repair.stale_reason,
        'reason', repair.reason,
        'reversible', true,
        'policy', 'company-scope-relevance.v2',
        'repair_wave_event_id', wave.event_id,
        'migration',
            '0070_quarantine_stale_unscoped_shared_recipe_items'
    )
FROM
    stale_unscoped_shared_recipe_items AS repair
    CROSS JOIN stale_unscoped_shared_recipe_wave AS wave;
