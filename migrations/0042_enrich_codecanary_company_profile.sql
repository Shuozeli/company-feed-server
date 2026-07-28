-- Preserve the evidence-backed Atonomo-to-CodeCanary rebrand for the imported
-- YC company record. The official announcement states that atonomo.com moved
-- to codecanary.ai with the same product and company under a new name.
--
-- Primary evidence:
-- https://www.codecanary.ai/blog
-- https://atonomo.com/ (redirects to the current CodeCanary homepage)

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'yc-atonomo'
),
alias_values(alias) AS (
    VALUES
        ('Atonomo'),
        ('Bunting Labs'),
        ('CodeCanary')
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
    homepage_url = 'https://www.codecanary.ai/',
    blog_url = COALESCE(company.blog_url, 'https://www.codecanary.ai/blog'),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_company_rebrand',
            'source', 'official_company_announcement',
            'source_url', 'https://www.codecanary.ai/blog',
            'legacy_homepage_url', 'https://atonomo.com/'
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
        'reason', 'official_company_rebrand',
        'canonical_name', company.name,
        'aliases', company.aliases,
        'source_url', 'https://www.codecanary.ai/blog',
        'migration', '0042_enrich_codecanary_company_profile'
    )
FROM companies AS company
WHERE company.company_key = 'yc-atonomo';
