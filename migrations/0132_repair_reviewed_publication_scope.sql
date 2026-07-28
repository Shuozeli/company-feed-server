-- Repair two publication-scope findings from the all-company recipe audit.
--
-- Manhattan Associates publishes first-party newsroom, blog, and investor
-- content on manh.com, but its imported security name had no profile URLs or
-- verified host. Rebuild its previously scope-blocked recipes after enriching
-- the company profile.
--
-- DeepAware identifies Silicon Valley Robotics Center as its commercial arm,
-- but the current roboticscenter.ai news/digest pages publish broad robotics
-- and AI coverage rather than DeepAware company news. Keep the affiliation as
-- reviewed evidence while requiring article-level DeepAware identity on that
-- host, and quarantine the already imported broad-digest rows.
--
-- Primary evidence:
-- https://www.manh.com/
-- https://ir.manh.com/investor-relations
-- https://www.manh.com/about-us/newsroom
-- https://www.deepawareai.com/

CREATE TEMP TABLE reviewed_profile_repairs (
    company_key text PRIMARY KEY,
    canonical_name text NOT NULL,
    homepage_url text NOT NULL,
    investor_relations_url text,
    newsroom_url text,
    blog_url text,
    verified_hosts text[] NOT NULL,
    excluded_hosts text[] NOT NULL,
    reason text NOT NULL,
    source_url text NOT NULL
) ON COMMIT DROP;

INSERT INTO reviewed_profile_repairs (
    company_key,
    canonical_name,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    blog_url,
    verified_hosts,
    excluded_hosts,
    reason,
    source_url
)
VALUES
    (
        'manhattan-associates-inc-common-stock',
        'Manhattan Associates, Inc.',
        'https://www.manh.com/',
        'https://ir.manh.com/investor-relations',
        'https://www.manh.com/about-us/newsroom',
        'https://www.manh.com/our-insights/resources/blog',
        ARRAY['manh.com'],
        ARRAY[]::text[],
        'official_company_and_investor_publication_hosts',
        'https://www.manh.com/'
    ),
    (
        'yc-deepaware-ai',
        'DeepAware AI',
        'https://www.deepawareai.com/',
        NULL,
        NULL,
        NULL,
        ARRAY[]::text[],
        ARRAY['roboticscenter.ai'],
        'affiliated_broad_publication_requires_company_identity_scope',
        'https://www.deepawareai.com/'
    );

CREATE TEMP TABLE reviewed_profile_aliases (
    company_key text NOT NULL,
    alias text NOT NULL,
    PRIMARY KEY (company_key, alias)
) ON COMMIT DROP;

INSERT INTO reviewed_profile_aliases (company_key, alias)
VALUES
    (
        'manhattan-associates-inc-common-stock',
        'Manhattan'
    ),
    (
        'manhattan-associates-inc-common-stock',
        'Manhattan Associates'
    ),
    (
        'manhattan-associates-inc-common-stock',
        'Manhattan Associates Inc.'
    ),
    (
        'manhattan-associates-inc-common-stock',
        'Manhattan Associates Inc. Common Stock'
    ),
    (
        'manhattan-associates-inc-common-stock',
        'Manhattan Associates, Inc.'
    );

WITH merged_aliases AS (
    SELECT
        company.id,
        COALESCE(
            jsonb_agg(DISTINCT value.alias ORDER BY value.alias)
                FILTER (WHERE value.alias IS NOT NULL),
            '[]'::jsonb
        ) AS aliases
    FROM
        companies AS company
        JOIN reviewed_profile_repairs AS repair
            ON repair.company_key = company.company_key
        LEFT JOIN LATERAL (
            SELECT jsonb_array_elements_text(company.aliases) AS alias
            UNION ALL
            SELECT alias
            FROM reviewed_profile_aliases
            WHERE
                reviewed_profile_aliases.company_key =
                    company.company_key
        ) AS value ON true
    GROUP BY company.id
)
UPDATE companies AS company
SET
    name = repair.canonical_name,
    name_source = 'operator',
    aliases = COALESCE(merged_aliases.aliases, company.aliases),
    homepage_url = COALESCE(company.homepage_url, repair.homepage_url),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        repair.investor_relations_url
    ),
    newsroom_url = COALESCE(company.newsroom_url, repair.newsroom_url),
    blog_url = COALESCE(company.blog_url, repair.blog_url),
    metadata = jsonb_set(
        company.metadata || jsonb_build_object(
            'profile_enrichment',
            jsonb_build_object(
                'reason', repair.reason,
                'source', 'reviewed_official_company_evidence',
                'source_url', repair.source_url,
                'migration',
                    '0132_repair_reviewed_publication_scope'
            )
        ),
        '{publication_host_policy}',
        COALESCE(
            company.metadata -> 'publication_host_policy',
            '{}'::jsonb
        ) || jsonb_build_object(
            'verified_hosts',
                (
                    SELECT COALESCE(
                        jsonb_agg(DISTINCT host ORDER BY host),
                        '[]'::jsonb
                    )
                    FROM (
                        SELECT jsonb_array_elements_text(
                            COALESCE(
                                company.metadata
                                    #> '{publication_host_policy,verified_hosts}',
                                '[]'::jsonb
                            )
                        ) AS host
                        UNION ALL
                        SELECT unnest(repair.verified_hosts)
                    ) AS hosts
                ),
            'excluded_hosts',
                (
                    SELECT COALESCE(
                        jsonb_agg(DISTINCT host ORDER BY host),
                        '[]'::jsonb
                    )
                    FROM (
                        SELECT jsonb_array_elements_text(
                            COALESCE(
                                company.metadata
                                    #> '{publication_host_policy,excluded_hosts}',
                                '[]'::jsonb
                            )
                        ) AS host
                        UNION ALL
                        SELECT unnest(repair.excluded_hosts)
                    ) AS hosts
                ),
            'direct_evidence_excluded_hosts',
                COALESCE(
                    company.metadata
                        #> '{publication_host_policy,direct_evidence_excluded_hosts}',
                    '[]'::jsonb
                ),
            'policy', 'company-publication-host-policy.v6',
            'reviewed_at', CURRENT_TIMESTAMP,
            'migration', '0132_repair_reviewed_publication_scope'
        ),
        true
    ),
    discovery_not_before = LEAST(
        company.discovery_not_before,
        CURRENT_TIMESTAMP
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    reviewed_profile_repairs AS repair
    JOIN merged_aliases ON true
WHERE
    company.company_key = repair.company_key
    AND merged_aliases.id = company.id;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'company.profile_refreshed',
    company.id,
    jsonb_build_object(
        'policy', 'company-publication-host-policy.v6',
        'reason', repair.reason,
        'canonical_name', company.name,
        'aliases', company.aliases,
        'verified_hosts', to_jsonb(repair.verified_hosts),
        'excluded_hosts', to_jsonb(repair.excluded_hosts),
        'source_url', repair.source_url,
        'migration', '0132_repair_reviewed_publication_scope'
    )
FROM
    reviewed_profile_repairs AS repair
    JOIN companies AS company ON company.company_key = repair.company_key;

-- Turn Manhattan's conclusive scope failures back into explicit rebuild
-- inputs. The extraction worker must crawl them again before any recipe can
-- become active; profile verification alone never publishes content.
CREATE TEMP TABLE manhattan_recipe_rebuilds
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    source.url AS publication_url
FROM
    companies AS company
    JOIN company_news_recipes AS recipe
        ON recipe.company_id = company.id
    JOIN sources AS source ON source.id = recipe.source_id
WHERE
    company.company_key =
        'manhattan-associates-inc-common-stock'
    AND recipe.status IN ('stale', 'superseded')
    AND lower(source.url)
        ~ '^https?://([^/]+[.])?manh[.]com(/|$)';

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = 'verified_company_host_profile_refresh',
    updated_at = CURRENT_TIMESTAMP
FROM manhattan_recipe_rebuilds AS rebuild
WHERE recipe.id = rebuild.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    rebuild_required = true,
    reason = 'verified_company_host_profile_refresh',
    metadata = state.metadata || jsonb_build_object(
        'profile_repair',
        jsonb_build_object(
            'policy', 'company-publication-host-policy.v6',
            'publication_host', 'manh.com',
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0132_repair_reviewed_publication_scope'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM manhattan_recipe_rebuilds AS rebuild
WHERE state.recipe_id = rebuild.recipe_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_rebuild_requested',
    rebuild.company_id,
    rebuild.source_id,
    jsonb_build_object(
        'recipe_id', rebuild.recipe_id,
        'publication_url', rebuild.publication_url,
        'reason', 'verified_company_host_profile_refresh',
        'policy', 'company-publication-host-policy.v6',
        'migration', '0132_repair_reviewed_publication_scope'
    )
FROM manhattan_recipe_rebuilds AS rebuild;

-- Queue one include-covered build because Manhattan already has a healthy RSS
-- feed. The regular no-feed campaign would otherwise correctly skip it.
INSERT INTO jobs (
    job_type,
    job_key,
    status,
    priority,
    run_after,
    max_attempts,
    company_id,
    payload
)
SELECT
    'extract_company_news',
    'company:' || company.id::text || ':manual-news-import',
    'pending',
    16384,
    CURRENT_TIMESTAMP,
    3,
    company.id,
    jsonb_build_object(
        'schema_version', 'company-news-extraction-job.v1',
        'window_start', CURRENT_TIMESTAMP - interval '93 days',
        'window_end', CURRENT_TIMESTAMP,
        'max_articles', 20,
        'include_covered', true
    )
FROM companies AS company
WHERE
    company.company_key =
        'manhattan-associates-inc-common-stock'
    AND NOT EXISTS (
        SELECT 1
        FROM jobs AS active_job
        WHERE
            active_job.job_type = 'extract_company_news'
            AND active_job.job_key =
                'company:' || company.id::text || ':manual-news-import'
            AND active_job.status IN ('pending', 'running')
    )
ON CONFLICT (job_type, job_key)
    WHERE status IN ('pending', 'running')
DO NOTHING;

-- The current Silicon Valley Robotics Center publication is affiliated, not
-- independently owned by another company. Disable only the broad publication
-- association and retain a reversible, identity-scoped path for future
-- DeepAware-specific articles if the site changes.
CREATE TEMP TABLE deepaware_broad_sources
ON COMMIT DROP AS
SELECT
    source.id AS source_id,
    source.company_id,
    source.url AS publication_url
FROM
    companies AS company
    JOIN sources AS source ON source.company_id = company.id
WHERE
    company.company_key = 'yc-deepaware-ai'
    AND lower(source.url)
        ~ '^https?://([^/]+[.])?roboticscenter[.]ai(/|$)';

CREATE TEMP TABLE deepaware_broad_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec ->> 'publication_url' AS publication_url
FROM
    deepaware_broad_sources AS broad
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = broad.source_id
WHERE recipe.status <> 'disabled';

CREATE TEMP TABLE deepaware_broad_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.url,
    item.canonical_url,
    item.title,
    item.published_at
FROM
    deepaware_broad_sources AS broad
    JOIN feed_items AS item ON item.source_id = broad.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE deepaware_broad_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.company_id,
    candidate.accepted_source_id,
    candidate.candidate_url,
    candidate.status AS prior_status
FROM
    companies AS company
    JOIN source_candidates AS candidate
        ON candidate.company_id = company.id
WHERE
    company.company_key = 'yc-deepaware-ai'
    AND candidate.status <> 'rejected'
    AND lower(candidate.candidate_url)
        ~ '^https?://([^/]+[.])?roboticscenter[.]ai(/|$)';

CREATE TEMP TABLE deepaware_scope_repair_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.reviewed_publication_scope_repair_started',
        jsonb_build_object(
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM deepaware_broad_sources
                ),
            'source_count', (SELECT count(*) FROM deepaware_broad_sources),
            'recipe_count', (SELECT count(*) FROM deepaware_broad_recipes),
            'candidate_count',
                (SELECT count(*) FROM deepaware_broad_candidates),
            'item_count', (SELECT count(*) FROM deepaware_broad_items),
            'reversible', true,
            'policy', 'company-scope-relevance.v4',
            'migration', '0132_repair_reviewed_publication_scope'
        )
    WHERE EXISTS (SELECT 1 FROM deepaware_broad_sources)
    RETURNING id
)
INSERT INTO deepaware_scope_repair_wave (event_id)
SELECT id FROM repair_started;

UPDATE company_news_recipes AS recipe
SET
    status = 'disabled',
    updated_at = CURRENT_TIMESTAMP
FROM deepaware_broad_recipes AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = false,
    consecutive_correctness_failures = GREATEST(
        state.consecutive_correctness_failures,
        3
    ),
    reason = 'broad_affiliated_publication_not_company_scoped',
    metadata = state.metadata || jsonb_build_object(
        'scope_repair',
        jsonb_build_object(
            'policy', 'company-scope-relevance.v4',
            'affiliation', 'official_commercial_arm',
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0132_repair_reviewed_publication_scope'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    deepaware_broad_recipes AS repair
    CROSS JOIN deepaware_scope_repair_wave AS wave
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
                'reason',
                    'broad_affiliated_publication_not_company_scoped',
                'reversible', true,
                'policy', 'company-scope-relevance.v4',
                'affiliation', 'official_commercial_arm',
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration',
                    '0132_repair_reviewed_publication_scope'
            )
        ),
    updated_at = CURRENT_TIMESTAMP
FROM
    deepaware_broad_sources AS repair
    CROSS JOIN deepaware_scope_repair_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_state AS state
SET
    last_error =
        'disabled: broad_affiliated_publication_not_company_scoped',
    backoff_until = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM deepaware_broad_sources AS repair
WHERE state.source_id = repair.source_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error =
        'cancelled because affiliated publication is not company scoped',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM deepaware_broad_sources AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running');

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM deepaware_broad_candidates AS repair
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
    repair.accepted_source_id,
    'rejected',
    'operator',
    'migration:0132',
    'affiliated publication currently lacks DeepAware-scoped company news',
    jsonb_build_object(
        'prior_status', repair.prior_status,
        'reversible', true,
        'policy', 'company-scope-relevance.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0132_repair_reviewed_publication_scope'
    )
FROM
    deepaware_broad_candidates AS repair
    CROSS JOIN deepaware_scope_repair_wave AS wave;

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
            'reason',
                'broad_affiliated_publication_not_company_scoped',
            'original_published_at', repair.published_at,
            'reversible', true,
            'policy', 'company-scope-relevance.v4',
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP,
            'migration', '0132_repair_reviewed_publication_scope'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    deepaware_broad_items AS repair
    CROSS JOIN deepaware_scope_repair_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error =
        'quality quarantine: '
        || 'broad_affiliated_publication_not_company_scoped',
    normalized_feed_item_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM deepaware_broad_items AS repair
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
        'reason', 'broad_affiliated_publication_not_company_scoped',
        'reversible', true,
        'policy', 'company-scope-relevance.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0132_repair_reviewed_publication_scope'
    )
FROM
    deepaware_broad_items AS repair
    CROSS JOIN deepaware_scope_repair_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'publication_url', repair.publication_url,
        'reason', 'broad_affiliated_publication_not_company_scoped',
        'affiliation', 'official_commercial_arm',
        'reversible', true,
        'policy', 'company-scope-relevance.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0132_repair_reviewed_publication_scope'
    )
FROM
    deepaware_broad_sources AS repair
    CROSS JOIN deepaware_scope_repair_wave AS wave;
