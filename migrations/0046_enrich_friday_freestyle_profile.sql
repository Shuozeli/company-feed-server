-- Friday, Inc. now operates as Freestyle. The registry homepage redirects
-- directly to the new official domain, and the company publication is hosted
-- there.
--
-- Primary evidence:
-- https://www.friday.so/ -> https://freestyle.ai/
-- https://freestyle.ai/blog

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'yc-friday'
),
alias_values(alias) AS (
    VALUES
        ('Friday'),
        ('Friday Inc.'),
        ('Freestyle'),
        ('Freestyle AI')
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
    homepage_url = 'https://freestyle.ai/',
    blog_url = 'https://freestyle.ai/blog',
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_redirect_and_rebrand',
            'source', 'official_company_website',
            'source_url', 'https://www.friday.so/',
            'previous_homepage_url', company.homepage_url
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
        'reason', 'official_redirect_and_rebrand',
        'canonical_name', company.name,
        'aliases', company.aliases,
        'source_url', 'https://www.friday.so/',
        'migration', '0046_enrich_friday_freestyle_profile'
    )
FROM companies AS company
WHERE company.company_key = 'yc-friday';
