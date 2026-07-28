-- Repair five company profiles exposed by the all-company recipe campaign.
-- The approved feeds for Advanced Energy, ATEC, and Allied are first-party,
-- but the imported security names did not carry the public brands or official
-- publication hosts needed by the generic company-scope gate. Alpha
-- Technology needs its official press-release entry point preserved even
-- though migration 0121 retires its placeholder-only root feed. Angel Oak's
-- fund lives on a shared investment-manager host, so that host remains
-- explicitly company-scoped instead of becoming a blanket verified host.
--
-- Primary evidence:
-- Advanced Energy: https://www.advancedenergy.com/en-us/about/news/
-- ATEC: https://atecspine.com/about/
-- Alpha Technology: https://atgl.io/press-release/
-- Angel Oak FINS: https://angeloakcapital.com/investments/fins/
-- Allied rename:
-- https://www.sec.gov/Archives/edgar/data/1708341/000121390026059288/ea0291647-8k_allinfuture.htm

CREATE TEMP TABLE verified_feed_profile_values (
    company_key text PRIMARY KEY,
    canonical_name text,
    homepage_url text,
    investor_relations_url text,
    newsroom_url text,
    blog_url text,
    verified_hosts text[] NOT NULL,
    excluded_hosts text[] NOT NULL,
    direct_evidence_excluded_hosts text[] NOT NULL,
    reason text NOT NULL,
    source_url text NOT NULL
) ON COMMIT DROP;

INSERT INTO verified_feed_profile_values (
    company_key,
    canonical_name,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    blog_url,
    verified_hosts,
    excluded_hosts,
    direct_evidence_excluded_hosts,
    reason,
    source_url
)
VALUES
    (
        'advanced-energy-industries-inc-common-stock',
        NULL,
        'https://www.advancedenergy.com/',
        'https://ir.advancedenergy.com/',
        'https://www.advancedenergy.com/en-us/about/news/',
        'https://www.advancedenergy.com/en-us/about/news/blog/',
        ARRAY['advancedenergy.com'],
        ARRAY[]::text[],
        ARRAY[]::text[],
        'official_legal_name_and_brand_domain',
        'https://www.advancedenergy.com/en-us/about/news/'
    ),
    (
        'allied-gaming-entertainment-inc-common-stock',
        'All In FutureTech Alliance, Inc.',
        'https://alliedgaming.gg/',
        'https://ir.alliedgaming.gg/',
        'https://ir.alliedgaming.gg/news-events/press-releases',
        NULL,
        ARRAY['alliedgaming.gg'],
        ARRAY[]::text[],
        ARRAY[]::text[],
        'verified_corporate_rename_and_legacy_official_domain',
        'https://www.sec.gov/Archives/edgar/data/1708341/000121390026059288/ea0291647-8k_allinfuture.htm'
    ),
    (
        'alpha-technology-group-limited-class-a-ordinary-shares',
        NULL,
        'https://atgl.io/',
        NULL,
        'https://atgl.io/press-release/',
        NULL,
        ARRAY['atgl.io'],
        ARRAY[]::text[],
        ARRAY[]::text[],
        'official_legal_name_domain',
        'https://atgl.io/press-release/'
    ),
    (
        'alphatec-holdings-inc-common-stock',
        NULL,
        'https://atecspine.com/',
        'https://investors.alphatecspine.com/',
        'https://investors.alphatecspine.com/press-releases/default.aspx',
        NULL,
        ARRAY['atecspine.com', 'alphatecspine.com'],
        ARRAY[]::text[],
        ARRAY[]::text[],
        'official_operating_brand_and_investor_domain',
        'https://atecspine.com/about/'
    ),
    (
        'angel-oak-financial-strategies-income-term-trust-common-shares-of-beneficial-interest',
        NULL,
        'https://angeloakcapital.com/investments/fins/',
        NULL,
        NULL,
        NULL,
        ARRAY[]::text[],
        ARRAY['angeloakcapital.com'],
        ARRAY[]::text[],
        'shared_manager_host_requires_fund_identity_scope',
        'https://angeloakcapital.com/investments/fins/'
    );

CREATE TEMP TABLE verified_feed_alias_values (
    company_key text NOT NULL,
    alias text NOT NULL,
    PRIMARY KEY (company_key, alias)
) ON COMMIT DROP;

INSERT INTO verified_feed_alias_values (company_key, alias)
VALUES
    (
        'advanced-energy-industries-inc-common-stock',
        'Advanced Energy'
    ),
    (
        'advanced-energy-industries-inc-common-stock',
        'Advanced Energy Industries'
    ),
    (
        'advanced-energy-industries-inc-common-stock',
        'Advanced Energy Industries, Inc.'
    ),
    (
        'allied-gaming-entertainment-inc-common-stock',
        'All In FutureTech Alliance'
    ),
    (
        'allied-gaming-entertainment-inc-common-stock',
        'All In FutureTech Alliance, Inc.'
    ),
    (
        'allied-gaming-entertainment-inc-common-stock',
        'Allied Gaming & Entertainment'
    ),
    (
        'allied-gaming-entertainment-inc-common-stock',
        'Allied Gaming & Entertainment Inc.'
    ),
    (
        'allied-gaming-entertainment-inc-common-stock',
        'Allied Gaming & Entertainment Inc. Common Stock'
    ),
    (
        'alpha-technology-group-limited-class-a-ordinary-shares',
        'Alpha Technology Group'
    ),
    (
        'alpha-technology-group-limited-class-a-ordinary-shares',
        'Alpha Technology Group Limited'
    ),
    ('alphatec-holdings-inc-common-stock', 'Alphatec'),
    (
        'alphatec-holdings-inc-common-stock',
        'Alphatec Holdings'
    ),
    (
        'alphatec-holdings-inc-common-stock',
        'Alphatec Holdings, Inc.'
    ),
    (
        'alphatec-holdings-inc-common-stock',
        'Alphatec Spine'
    ),
    (
        'alphatec-holdings-inc-common-stock',
        'Alphatec Spine, Inc.'
    ),
    ('alphatec-holdings-inc-common-stock', 'ATEC'),
    (
        'angel-oak-financial-strategies-income-term-trust-common-shares-of-beneficial-interest',
        'Angel Oak Financial Strategies Income Term Trust'
    ),
    (
        'angel-oak-financial-strategies-income-term-trust-common-shares-of-beneficial-interest',
        'Financial Strategies Income Term Trust'
    );

WITH merged_aliases AS (
    SELECT
        company.id,
        jsonb_agg(DISTINCT value.alias ORDER BY value.alias) AS aliases
    FROM
        companies AS company
        JOIN verified_feed_profile_values AS profile
            ON profile.company_key = company.company_key
        CROSS JOIN LATERAL (
            SELECT jsonb_array_elements_text(company.aliases) AS alias
            UNION ALL
            SELECT alias
            FROM verified_feed_alias_values
            WHERE
                verified_feed_alias_values.company_key =
                    company.company_key
        ) AS value
    GROUP BY company.id
)
UPDATE companies AS company
SET
    name = COALESCE(profile.canonical_name, company.name),
    name_source = CASE
        WHEN profile.canonical_name IS NOT NULL THEN 'operator'
        ELSE company.name_source
    END,
    aliases = merged_aliases.aliases,
    homepage_url = COALESCE(company.homepage_url, profile.homepage_url),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        profile.investor_relations_url
    ),
    newsroom_url = COALESCE(company.newsroom_url, profile.newsroom_url),
    blog_url = COALESCE(company.blog_url, profile.blog_url),
    metadata = jsonb_set(
        company.metadata || jsonb_build_object(
            'profile_enrichment',
            jsonb_build_object(
                'reason', profile.reason,
                'source', 'reviewed_official_company_evidence',
                'source_url', profile.source_url,
                'migration',
                    '0120_refresh_verified_feed_company_profiles'
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
                        SELECT unnest(profile.verified_hosts)
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
                        SELECT unnest(profile.excluded_hosts)
                    ) AS hosts
                ),
            'direct_evidence_excluded_hosts',
                (
                    SELECT COALESCE(
                        jsonb_agg(DISTINCT host ORDER BY host),
                        '[]'::jsonb
                    )
                    FROM (
                        SELECT jsonb_array_elements_text(
                            COALESCE(
                                company.metadata
                                    #> '{publication_host_policy,direct_evidence_excluded_hosts}',
                                '[]'::jsonb
                            )
                        ) AS host
                        UNION ALL
                        SELECT unnest(
                            profile.direct_evidence_excluded_hosts
                        )
                    ) AS hosts
                ),
            'policy', 'company-publication-host-policy.v3',
            'reviewed_at', CURRENT_TIMESTAMP,
            'migration', '0120_refresh_verified_feed_company_profiles'
        ),
        true
    ),
    discovery_not_before = LEAST(
        company.discovery_not_before,
        CURRENT_TIMESTAMP
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    verified_feed_profile_values AS profile,
    merged_aliases
WHERE
    company.company_key = profile.company_key
    AND company.id = merged_aliases.id;

-- Preserve the former and current public listings as historical attributes;
-- neither listing is used as the company identity or manual import selector.
UPDATE company_listings AS listing
SET
    is_primary = false,
    metadata = listing.metadata || jsonb_build_object(
        'valid_to', '2026-05-26',
        'superseded_by', 'AIFA',
        'source', 'official_company_announcement',
        'source_url',
            'https://ir.alliedgaming.gg/news-events/press-releases/detail/202/all-in-futuretech-alliance-inc-announces-nasdaq-approval'
    ),
    updated_at = CURRENT_TIMESTAMP
WHERE
    listing.company_id = (
        SELECT id
        FROM companies
        WHERE
            company_key =
                'allied-gaming-entertainment-inc-common-stock'
    )
    AND listing.ticker = 'AGAE';

INSERT INTO company_listings (
    company_id,
    ticker,
    exchange,
    is_primary,
    metadata
)
SELECT
    company.id,
    'AIFA',
    COALESCE(
        (
            SELECT listing.exchange
            FROM company_listings AS listing
            WHERE
                listing.company_id = company.id
                AND listing.ticker = 'AGAE'
            ORDER BY listing.created_at
            LIMIT 1
        ),
        ''
    ),
    true,
    jsonb_build_object(
        'valid_from', '2026-05-27',
        'previous_ticker', 'AGAE',
        'source', 'official_company_announcement',
        'source_url',
            'https://ir.alliedgaming.gg/news-events/press-releases/detail/202/all-in-futuretech-alliance-inc-announces-nasdaq-approval'
    )
FROM companies AS company
WHERE
    company.company_key =
        'allied-gaming-entertainment-inc-common-stock'
ON CONFLICT (company_id, ticker, exchange)
DO UPDATE SET
    is_primary = EXCLUDED.is_primary,
    metadata = company_listings.metadata || EXCLUDED.metadata,
    updated_at = CURRENT_TIMESTAMP;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    CASE
        WHEN profile.canonical_name IS NOT NULL
            THEN 'company.profile_refreshed'
        ELSE 'company.profile_enriched'
    END,
    company.id,
    jsonb_build_object(
        'policy', 'company-profile-enrichment.v2',
        'reason', profile.reason,
        'canonical_name', company.name,
        'aliases', company.aliases,
        'verified_hosts', to_jsonb(profile.verified_hosts),
        'excluded_hosts', to_jsonb(profile.excluded_hosts),
        'source_url', profile.source_url,
        'migration', '0120_refresh_verified_feed_company_profiles'
    )
FROM
    verified_feed_profile_values AS profile
    JOIN companies AS company ON company.company_key = profile.company_key;

-- Re-run discovery through the dedicated component now that the authoritative
-- entry points are present. This is especially important for ATEC's current
-- investor feed at investors.alphatecspine.com/rss/pressrelease.aspx.
UPDATE jobs AS job
SET
    priority = GREATEST(job.priority, 4096),
    run_after = LEAST(job.run_after, CURRENT_TIMESTAMP),
    updated_at = CURRENT_TIMESTAMP
WHERE
    job.job_type = 'discover_company'
    AND job.status IN ('pending', 'running')
    AND job.company_id IN (
        SELECT company.id
        FROM
            companies AS company
            JOIN verified_feed_profile_values AS profile
                ON profile.company_key = company.company_key
    );

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
    'discover_company',
    'company:' || company.id::text,
    'pending',
    4096,
    CURRENT_TIMESTAMP,
    5,
    company.id,
    jsonb_build_object(
        'company_id', company.id,
        'trigger', 'verified_company_profile_refresh',
        'migration', '0120_refresh_verified_feed_company_profiles'
    )
FROM
    companies AS company
    JOIN verified_feed_profile_values AS profile
        ON profile.company_key = company.company_key
WHERE NOT EXISTS (
    SELECT 1
    FROM jobs AS active_job
    WHERE
        active_job.job_type = 'discover_company'
        AND active_job.job_key = 'company:' || company.id::text
        AND active_job.status IN ('pending', 'running')
)
ON CONFLICT (job_type, job_key)
    WHERE status IN ('pending', 'running')
DO NOTHING;

CREATE TEMP TABLE verified_feed_recrawl_targets
ON COMMIT DROP AS
WITH expected (company_key, source_url) AS (
    VALUES
        (
            'advanced-energy-industries-inc-common-stock',
            'https://www.advancedenergy.com/rss'
        ),
        (
            'allied-gaming-entertainment-inc-common-stock',
            'https://ir.alliedgaming.gg/news-events/press-releases/rss'
        ),
        (
            'alphatec-holdings-inc-common-stock',
            'https://atecspine.com/feed/'
        )
)
SELECT
    source.id AS source_id,
    source.company_id,
    source.url
FROM
    expected
    JOIN companies AS company
        ON company.company_key = expected.company_key
    JOIN sources AS source
        ON source.company_id = company.id
        AND source.url = expected.source_url
WHERE
    source.status = 'approved'
    AND source.kind IN ('rss', 'atom');

UPDATE jobs AS job
SET
    priority = GREATEST(job.priority, 8192),
    run_after = LEAST(job.run_after, CURRENT_TIMESTAMP),
    updated_at = CURRENT_TIMESTAMP
WHERE
    job.job_type = 'crawl_source'
    AND job.status IN ('pending', 'running')
    AND job.source_id IN (
        SELECT source_id
        FROM verified_feed_recrawl_targets
    );

INSERT INTO jobs (
    job_type,
    job_key,
    status,
    priority,
    run_after,
    max_attempts,
    company_id,
    source_id,
    payload
)
SELECT
    'crawl_source',
    'source:' || target.source_id::text,
    'pending',
    8192,
    CURRENT_TIMESTAMP,
    5,
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'source_id', target.source_id,
        'trigger', 'verified_company_profile_refresh',
        'policy', 'company-publication-host-policy.v3',
        'migration', '0120_refresh_verified_feed_company_profiles'
    )
FROM verified_feed_recrawl_targets AS target
WHERE NOT EXISTS (
    SELECT 1
    FROM jobs AS active_job
    WHERE
        active_job.job_type = 'crawl_source'
        AND active_job.job_key = 'source:' || target.source_id::text
        AND active_job.status IN ('pending', 'running')
)
ON CONFLICT (job_type, job_key)
    WHERE status IN ('pending', 'running')
DO NOTHING;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.company_profile_recrawl_queued',
    target.company_id,
    target.source_id,
    jsonb_build_object(
        'url', target.url,
        'reason', 'verified_company_profile_refresh',
        'policy', 'company-publication-host-policy.v3',
        'migration', '0120_refresh_verified_feed_company_profiles'
    )
FROM verified_feed_recrawl_targets AS target;
