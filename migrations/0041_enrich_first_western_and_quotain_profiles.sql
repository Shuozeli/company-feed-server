-- Preserve the official First Western operating brand and the evidence-backed
-- Eden-to-Quotain rebrand. The imported names predate or differ from the
-- brands displayed by their current publication hosts.
--
-- Primary evidence:
-- https://myfw.com/company/
-- https://myfw.gcs-web.com/
-- https://www.quotain.com/
-- https://tryeden.ai/ (redirects to the current Quotain homepage)

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'first-western-financial-inc-common-stock'
),
alias_values(alias) AS (
    VALUES
        ('First Western'),
        ('First Western Financial'),
        ('First Western Financial, Inc.'),
        ('First Western Trust'),
        ('First Western Trust Bank')
),
merged_aliases AS (
    SELECT
        target.id,
        jsonb_agg(DISTINCT alias ORDER BY alias) AS aliases
    FROM target
    CROSS JOIN LATERAL (
        SELECT jsonb_array_elements_text(company.aliases) AS alias
        FROM companies AS company
        WHERE company.id = target.id
        UNION ALL
        SELECT alias
        FROM alias_values
    ) AS values
    GROUP BY target.id
)
UPDATE companies AS company
SET
    aliases = merged_aliases.aliases,
    homepage_url = COALESCE(company.homepage_url, 'https://myfw.com/'),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        'https://myfw.gcs-web.com/'
    ),
    newsroom_url = COALESCE(
        company.newsroom_url,
        'https://myfw.gcs-web.com/news-and-events/press-releases'
    ),
    blog_url = COALESCE(company.blog_url, 'https://myfw.com/articles/'),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_operating_brand_and_domain',
            'source', 'official_company_website',
            'source_url', 'https://myfw.com/company/'
        )
    )
FROM merged_aliases
WHERE company.id = merged_aliases.id;

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'yc-tryeden'
),
alias_values(alias) AS (
    VALUES
        ('Eden'),
        ('Quotain')
),
merged_aliases AS (
    SELECT
        target.id,
        jsonb_agg(DISTINCT alias ORDER BY alias) AS aliases
    FROM target
    CROSS JOIN LATERAL (
        SELECT jsonb_array_elements_text(company.aliases) AS alias
        FROM companies AS company
        WHERE company.id = target.id
        UNION ALL
        SELECT alias
        FROM alias_values
    ) AS values
    GROUP BY target.id
)
UPDATE companies AS company
SET
    aliases = merged_aliases.aliases,
    homepage_url = 'https://www.quotain.com/',
    blog_url = COALESCE(company.blog_url, 'https://www.quotain.com/blog'),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_company_rebrand',
            'source', 'official_redirect_and_company_website',
            'source_url', 'https://tryeden.ai/',
            'legacy_homepage_url', 'https://tryeden.ai/'
        )
    )
FROM merged_aliases
WHERE company.id = merged_aliases.id;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'company.profile_enriched',
    company.id,
    jsonb_build_object(
        'policy', 'company-profile-enrichment.v1',
        'reason', profile.reason,
        'canonical_name', company.name,
        'aliases', company.aliases,
        'source_url', profile.source_url,
        'migration', '0041_enrich_first_western_and_quotain_profiles'
    )
FROM companies AS company
JOIN (
    VALUES
        (
            'first-western-financial-inc-common-stock',
            'official_operating_brand_and_domain',
            'https://myfw.com/company/'
        ),
        (
            'yc-tryeden',
            'official_company_rebrand',
            'https://tryeden.ai/'
        )
) AS profile(company_key, reason, source_url)
    ON profile.company_key = company.company_key;
