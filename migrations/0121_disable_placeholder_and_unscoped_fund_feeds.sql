-- Retire two feeds that repeatedly passed syntactic RSS validation but are not
-- valid editorial sources for their assigned name-first company:
--
-- * Alpha Technology's root WordPress feed contains only Test Post 001-003.
-- * Angel Oak Capital's root feed mixes commentaries and utility collections
--   for many distinct funds; it is not scoped to the Financial Strategies
--   Income Term Trust.
--
-- Keep every historical row for audit, reject the accepted candidate link,
-- and quarantine normalized items reversibly. Discovery remains free to find
-- a dedicated press-release page or a later entity-scoped feed.

CREATE TEMP TABLE invalid_approved_feed_sources (
    source_id uuid PRIMARY KEY,
    company_id uuid NOT NULL,
    source_url text NOT NULL,
    reason text NOT NULL,
    policy text NOT NULL
) ON COMMIT DROP;

INSERT INTO invalid_approved_feed_sources (
    source_id,
    company_id,
    source_url,
    reason,
    policy
)
SELECT
    source.id,
    source.company_id,
    source.url,
    expected.reason,
    expected.policy
FROM (
    VALUES
        (
            'alpha-technology-group-limited-class-a-ordinary-shares',
            'https://atgl.io/feed/',
            'cms_placeholder_only_feed',
            'cms-placeholder-feed.v1'
        ),
        (
            'angel-oak-financial-strategies-income-term-trust-common-shares-of-beneficial-interest',
            'https://angeloakcapital.com/feed/',
            'shared_manager_feed_not_entity_scoped',
            'shared-manager-feed-scope.v1'
        )
) AS expected (company_key, source_url, reason, policy)
JOIN companies AS company
    ON company.company_key = expected.company_key
JOIN sources AS source
    ON source.company_id = company.id
    AND source.url = expected.source_url
WHERE
    source.status = 'approved'
    AND source.kind IN ('rss', 'atom');

CREATE TEMP TABLE invalid_approved_feed_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.accepted_source_id AS source_id,
    repair.company_id,
    candidate.candidate_url,
    repair.reason,
    repair.policy
FROM
    invalid_approved_feed_sources AS repair
    JOIN source_candidates AS candidate
        ON candidate.accepted_source_id = repair.source_id
WHERE candidate.status = 'accepted';

CREATE TEMP TABLE invalid_approved_feed_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at,
    repair.reason,
    repair.policy
FROM
    invalid_approved_feed_sources AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id;

CREATE TEMP TABLE invalid_approved_feed_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'source.invalid_approved_feed_repair_started',
        jsonb_build_object(
            'source_count',
                (SELECT count(*) FROM invalid_approved_feed_sources),
            'candidate_count',
                (SELECT count(*) FROM invalid_approved_feed_candidates),
            'item_count',
                (SELECT count(*) FROM invalid_approved_feed_items),
            'reversible', true,
            'migration',
                '0121_disable_placeholder_and_unscoped_fund_feeds'
        )
    WHERE EXISTS (SELECT 1 FROM invalid_approved_feed_sources)
    RETURNING id
)
INSERT INTO invalid_approved_feed_wave (event_id)
SELECT id FROM repair_started;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata = source.metadata || jsonb_build_object(
        'quality_disable',
        jsonb_build_object(
            'reason', repair.reason,
            'reversible', true,
            'policy', repair.policy,
            'repair_wave_event_id', wave.event_id,
            'disabled_at', CURRENT_TIMESTAMP,
            'migration',
                '0121_disable_placeholder_and_unscoped_fund_feeds'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    invalid_approved_feed_sources AS repair
    CROSS JOIN invalid_approved_feed_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_state AS state
SET
    last_error = 'disabled: ' || repair.reason,
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_approved_feed_sources AS repair
WHERE state.source_id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because source was disabled: ' || repair.reason,
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_approved_feed_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running');

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_approved_feed_candidates AS repair
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
    'migration:0121',
    CASE repair.reason
        WHEN 'cms_placeholder_only_feed'
            THEN 'feed contains only CMS placeholder test posts'
        ELSE
            'shared investment-manager feed is not scoped to the assigned fund'
    END,
    jsonb_build_object(
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration', '0121_disable_placeholder_and_unscoped_fund_feeds'
    )
FROM
    invalid_approved_feed_candidates AS repair
    CROSS JOIN invalid_approved_feed_wave AS wave;

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
            'policy', repair.policy,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration',
                '0121_disable_placeholder_and_unscoped_fund_feeds'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    invalid_approved_feed_items AS repair
    CROSS JOIN invalid_approved_feed_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM invalid_approved_feed_sources AS repair
WHERE raw.source_id = repair.source_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.source_url,
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration', '0121_disable_placeholder_and_unscoped_fund_feeds'
    )
FROM
    invalid_approved_feed_sources AS repair
    CROSS JOIN invalid_approved_feed_wave AS wave;

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
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', wave.event_id,
        'migration', '0121_disable_placeholder_and_unscoped_fund_feeds'
    )
FROM
    invalid_approved_feed_items AS repair
    CROSS JOIN invalid_approved_feed_wave AS wave;
