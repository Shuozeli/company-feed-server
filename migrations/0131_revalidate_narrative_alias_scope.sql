-- Revalidate company-identity publications that passed exactly at the
-- minimum relevance threshold because narrative alias annotations contributed
-- connector words such as "in", "to", or "due" as company identity terms.
--
-- The runtime vocabulary now excludes annotation prose and non-distinctive
-- short words. This repair is intentionally cohort-based: it applies only to
-- active inferred recipes or approved feeds that both:
--   * belong to a company with a narrative alias annotation;
--   * required article-level company scope;
--   * passed at exactly 50% relevance while rejecting other sampled items.
--
-- Existing public rows from those sources are quarantined until a corrected
-- crawl accepts them again. Direct article evidence on another source remains
-- untouched.

CREATE TEMP TABLE narrative_alias_scope_companies
ON COMMIT DROP AS
SELECT DISTINCT
    company.id AS company_id
FROM
    companies AS company
    CROSS JOIN LATERAL jsonb_array_elements_text(company.aliases) AS alias(value)
WHERE
    lower(alias.value) ~
        '(^|[^a-z])(aka|brand|dba|due|fka|formerly|in|incorporating|informally|known|maker|makers|name|named|prev|previous|previously|process|product|pronounced|renamed|tbd|to|trademark|was)([^a-z]|$)';

CREATE TEMP TABLE narrative_alias_scope_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url,
    'narrative_alias_scope_collision'::text AS reason
FROM
    narrative_alias_scope_companies AS target
    JOIN company_news_recipes AS recipe
        ON recipe.company_id = target.company_id
    JOIN company_news_recipe_state AS state
        ON state.recipe_id = recipe.id
WHERE
    recipe.status = 'active'
    AND recipe.spec ->> 'item_scope' = 'company_identity'
    AND CASE
        WHEN COALESCE(
            state.metadata ->> 'company_scope_relevance_ratio_bps',
            ''
        ) ~ '^[0-9]+$'
            THEN (
                state.metadata ->> 'company_scope_relevance_ratio_bps'
            )::integer
        ELSE -1
    END = 5000
    AND CASE
        WHEN COALESCE(
            state.metadata ->> 'company_scope_rejected_item_count',
            ''
        ) ~ '^[0-9]+$'
            THEN (
                state.metadata ->> 'company_scope_rejected_item_count'
            )::integer
        ELSE 0
    END > 0;

CREATE TEMP TABLE narrative_alias_scope_feeds
ON COMMIT DROP AS
SELECT
    source.company_id,
    source.id AS source_id,
    source.url AS publication_url,
    'narrative_alias_scope_collision'::text AS reason
FROM
    narrative_alias_scope_companies AS target
    JOIN sources AS source
        ON source.company_id = target.company_id
WHERE
    source.status = 'approved'
    AND source.kind IN ('rss', 'atom')
    AND source.metadata
        #>> '{activation,metadata,company_scope_required}' = 'true'
    AND CASE
        WHEN COALESCE(
            source.metadata
                #>> '{activation,metadata,company_scope_relevance_ratio_bps}',
            ''
        ) ~ '^[0-9]+$'
            THEN (
                source.metadata
                    #>> '{activation,metadata,company_scope_relevance_ratio_bps}'
            )::integer
        ELSE -1
    END = 5000
    AND CASE
        WHEN COALESCE(
            source.metadata
                #>> '{activation,metadata,company_scope_relevant_item_count}',
            ''
        ) ~ '^[0-9]+$'
            THEN (
                source.metadata
                    #>> '{activation,metadata,company_scope_relevant_item_count}'
            )::integer
        ELSE 0
    END
        < CASE
            WHEN COALESCE(
                source.metadata
                    #>> '{activation,metadata,company_scope_total_item_count}',
                ''
            ) ~ '^[0-9]+$'
                THEN (
                    source.metadata
                        #>> '{activation,metadata,company_scope_total_item_count}'
                )::integer
            ELSE 0
        END;

CREATE TEMP TABLE narrative_alias_scope_sources
ON COMMIT DROP AS
SELECT DISTINCT ON (source_id)
    source_id,
    company_id,
    publication_url,
    reason
FROM (
    SELECT
        source_id,
        company_id,
        publication_url,
        reason
    FROM narrative_alias_scope_recipes
    UNION ALL
    SELECT
        source_id,
        company_id,
        publication_url,
        reason
    FROM narrative_alias_scope_feeds
) AS target
ORDER BY source_id;

CREATE TEMP TABLE narrative_alias_scope_items
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
    target.reason
FROM
    narrative_alias_scope_sources AS target
    JOIN feed_items AS item
        ON item.source_id = target.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE narrative_alias_scope_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.accepted_source_id AS source_id,
    candidate.company_id,
    candidate.status AS prior_status
FROM
    source_candidates AS candidate
    JOIN narrative_alias_scope_sources AS target
        ON target.source_id = candidate.accepted_source_id;

CREATE TEMP TABLE narrative_alias_scope_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.narrative_alias_scope_repair_started',
        jsonb_build_object(
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM narrative_alias_scope_sources
                ),
            'source_count',
                (SELECT count(*) FROM narrative_alias_scope_sources),
            'recipe_count',
                (SELECT count(*) FROM narrative_alias_scope_recipes),
            'feed_count',
                (SELECT count(*) FROM narrative_alias_scope_feeds),
            'item_count',
                (SELECT count(*) FROM narrative_alias_scope_items),
            'policy', 'company-scope-relevance.v4',
            'migration', '0131_revalidate_narrative_alias_scope'
        )
    WHERE EXISTS (SELECT 1 FROM narrative_alias_scope_sources)
    RETURNING id
)
INSERT INTO narrative_alias_scope_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason,
    updated_at = CURRENT_TIMESTAMP
FROM narrative_alias_scope_recipes AS repair
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
        'scope_repair',
        jsonb_build_object(
            'policy', 'company-scope-relevance.v4',
            'repair_wave_event_id', wave.event_id,
            'publication_url', repair.publication_url,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0131_revalidate_narrative_alias_scope'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    narrative_alias_scope_recipes AS repair
    CROSS JOIN narrative_alias_scope_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata =
        source.metadata - 'active_recipe_id' - 'recipe_schema_version'
        || jsonb_build_object(
            'quality_disable',
            jsonb_build_object(
                'reason', repair.reason,
                'reversible', true,
                'policy', 'company-scope-relevance.v4',
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration', '0131_revalidate_narrative_alias_scope'
            )
        ),
    updated_at = CURRENT_TIMESTAMP
FROM
    narrative_alias_scope_sources AS repair
    CROSS JOIN narrative_alias_scope_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_state AS state
SET
    last_error = 'disabled: ' || repair.reason,
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM narrative_alias_scope_sources AS repair
WHERE state.source_id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because source was disabled: '
        || repair.reason,
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM narrative_alias_scope_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running');

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM narrative_alias_scope_candidates AS repair
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
    'migration:0131',
    'narrative alias annotation created false company-scope evidence',
    jsonb_build_object(
        'prior_status', repair.prior_status,
        'reason', 'narrative_alias_scope_collision',
        'reversible', true,
        'policy', 'company-scope-relevance.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0131_revalidate_narrative_alias_scope'
    )
FROM
    narrative_alias_scope_candidates AS repair
    CROSS JOIN narrative_alias_scope_wave AS wave;

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = COALESCE(
        item.content_processing,
        '{}'::jsonb
    ) || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'company-scope-relevance.v4',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0131_revalidate_narrative_alias_scope'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    narrative_alias_scope_items AS repair
    CROSS JOIN narrative_alias_scope_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: narrative_alias_scope_collision',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM narrative_alias_scope_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

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
        'policy', 'company-scope-relevance.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0131_revalidate_narrative_alias_scope'
    )
FROM
    narrative_alias_scope_items AS repair
    CROSS JOIN narrative_alias_scope_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'reason', repair.reason,
        'policy', 'company-scope-relevance.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0131_revalidate_narrative_alias_scope'
    )
FROM
    narrative_alias_scope_recipes AS repair
    CROSS JOIN narrative_alias_scope_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'publication_url', repair.publication_url,
        'reason', repair.reason,
        'reversible', true,
        'policy', 'company-scope-relevance.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0131_revalidate_narrative_alias_scope'
    )
FROM
    narrative_alias_scope_sources AS repair
    CROSS JOIN narrative_alias_scope_wave AS wave;
