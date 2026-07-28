-- Unedited CMS starter posts are deployment artifacts, not company news.
-- Quarantine only items whose short content proves the placeholder template.
-- If every public item from an approved feed is such a placeholder, disable
-- that feed as unusable; mixed feeds remain active for their real articles.

CREATE TEMP TABLE cms_placeholder_items ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title
FROM feed_items AS item
JOIN sources AS source ON source.id = item.source_id
WHERE
    NOT item.is_private
    AND source.status = 'approved'
    AND source.kind IN ('rss', 'atom')
    AND char_length(item.body_text) <= 1000
    AND lower(btrim(regexp_replace(item.title, '[[:space:]]+', ' ', 'g')))
        IN (
            'hello world',
            'hello world!',
            'olá, mundo',
            'olá, mundo!',
            'olá mundo',
            'olá mundo!',
            'ola, mundo',
            'ola, mundo!',
            'ola mundo',
            'ola mundo!',
            '¡hola mundo!',
            'hola mundo!',
            'bonjour tout le monde !',
            'bonjour tout le monde!',
            'hallo welt!',
            'ciao mondo!',
            'witaj, świecie!',
            'hallo wereld!',
            'merhaba dünya!',
            'привет, мир!',
            'こんにちは世界',
            '你好，世界！'
        )
    AND (
        lower(item.body_text) LIKE '%first post%'
        OR lower(item.body_text) LIKE '%primeiro post%'
        OR lower(item.body_text) LIKE '%primer post%'
        OR lower(item.body_text) LIKE '%premier article%'
        OR lower(item.body_text) LIKE '%erster beitrag%'
        OR lower(item.body_text) LIKE '%primo articolo%'
        OR lower(item.body_text) LIKE '%pierwszy wpis%'
        OR lower(item.body_text) LIKE '%первый пост%'
        OR lower(item.body_text) LIKE '%eerste bericht%'
        OR lower(item.body_text) LIKE '%ilk yazı%'
    );

CREATE TEMP TABLE cms_placeholder_only_sources ON COMMIT DROP AS
WITH affected_sources AS (
    SELECT DISTINCT source_id
    FROM cms_placeholder_items
)
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    count(*) AS public_item_count
FROM affected_sources AS affected
JOIN sources AS source ON source.id = affected.source_id
JOIN feed_items AS item
    ON item.source_id = source.id
    AND NOT item.is_private
LEFT JOIN cms_placeholder_items AS placeholder
    ON placeholder.feed_item_id = item.id
GROUP BY source.id
HAVING count(*) = count(placeholder.feed_item_id);

CREATE TEMP TABLE cms_placeholder_candidates ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    source.source_id,
    source.company_id,
    source.url
FROM cms_placeholder_only_sources AS source
JOIN source_candidates AS candidate
    ON candidate.accepted_source_id = source.source_id;

CREATE TEMP TABLE cms_placeholder_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'source.cms_placeholder_repair_started',
        jsonb_build_object(
            'disabled_source_count',
                (SELECT count(*) FROM cms_placeholder_only_sources),
            'candidate_count', (SELECT count(*) FROM cms_placeholder_candidates),
            'item_count', (SELECT count(*) FROM cms_placeholder_items),
            'policy', 'cms-placeholder-article.v1',
            'migration', '0053_quarantine_cms_placeholder_articles'
        )
    WHERE EXISTS (SELECT 1 FROM cms_placeholder_items)
    RETURNING id
)
INSERT INTO cms_placeholder_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', 'cms_placeholder_only_feed',
            'public_item_count', repair.public_item_count,
            'reversible', true,
            'policy', 'cms-placeholder-article.v1',
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP
        )
    )
FROM
    cms_placeholder_only_sources AS repair
    CROSS JOIN cms_placeholder_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL
FROM cms_placeholder_candidates AS repair
WHERE candidate.id = repair.candidate_id;

INSERT INTO candidate_decisions (
    candidate_id,
    source_id,
    decision,
    decision_mode,
    actor,
    reason,
    metadata
)
SELECT
    repair.candidate_id,
    repair.source_id,
    'rejected',
    'automatic',
    'migration:0053',
    'feed contains only unedited CMS placeholder articles',
    jsonb_build_object(
        'policy', 'cms-placeholder-article.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0053_quarantine_cms_placeholder_articles'
    )
FROM
    cms_placeholder_candidates AS repair
    CROSS JOIN cms_placeholder_wave AS wave;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', 'unedited_cms_placeholder_article',
            'reversible', true,
            'policy', 'cms-placeholder-article.v1',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    cms_placeholder_items AS repair
    CROSS JOIN cms_placeholder_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: unedited_cms_placeholder_article',
    normalized_feed_item_id = NULL
FROM cms_placeholder_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: feed disabled because it contains only CMS placeholders'
FROM cms_placeholder_only_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'public_item_count', repair.public_item_count,
        'reason', 'cms_placeholder_only_feed',
        'reversible', true,
        'policy', 'cms-placeholder-article.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0053_quarantine_cms_placeholder_articles'
    )
FROM
    cms_placeholder_only_sources AS repair
    CROSS JOIN cms_placeholder_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source_candidate.rejected',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'candidate_id', repair.candidate_id,
        'disabled_accepted_source', true,
        'reason', 'cms_placeholder_only_feed',
        'decision_mode', 'automatic',
        'actor', 'migration:0053',
        'policy', 'cms-placeholder-article.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0053_quarantine_cms_placeholder_articles'
    )
FROM
    cms_placeholder_candidates AS repair
    CROSS JOIN cms_placeholder_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'reason', 'unedited_cms_placeholder_article',
        'reversible', true,
        'policy', 'cms-placeholder-article.v1',
        'repair_wave_event_id', wave.event_id,
        'migration', '0053_quarantine_cms_placeholder_articles'
    )
FROM
    cms_placeholder_items AS repair
    CROSS JOIN cms_placeholder_wave AS wave;
