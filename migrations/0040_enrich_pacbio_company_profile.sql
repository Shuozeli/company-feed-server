-- Preserve the PacBio brand and official publication hosts for the imported
-- legal security name, Pacific Biosciences of California Inc. Common Stock.
--
-- Primary evidence:
-- https://www.pacb.com/about-us/
-- https://investor.pacificbiosciences.com/
-- https://www.pacb.com/blog/

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'pacific-biosciences-of-california-inc-common-stock'
),
alias_values(alias) AS (
    VALUES
        ('PacBio'),
        ('Pacific Biosciences'),
        ('Pacific Biosciences of California, Inc.')
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
    homepage_url = COALESCE(company.homepage_url, 'https://www.pacb.com/'),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        'https://investor.pacificbiosciences.com/'
    ),
    newsroom_url = COALESCE(
        company.newsroom_url,
        'https://investor.pacificbiosciences.com/press-releases'
    ),
    blog_url = COALESCE(company.blog_url, 'https://www.pacb.com/blog/'),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_short_brand_and_domain',
            'source', 'official_company_website',
            'source_url', 'https://www.pacb.com/about-us/'
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
        'reason', 'official_short_brand_and_domain',
        'canonical_name', company.name,
        'aliases', company.aliases,
        'source_url', 'https://www.pacb.com/about-us/',
        'migration', '0040_enrich_pacbio_company_profile'
    )
FROM companies AS company
WHERE
    company.company_key =
        'pacific-biosciences-of-california-inc-common-stock';
