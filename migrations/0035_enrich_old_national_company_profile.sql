-- Preserve the bank brand and official publication host used by Old National
-- Bancorp. The imported security name is correct, but the consumer publication
-- uses the shorter Old National / Old National Bank identity.
--
-- Primary evidence:
-- https://www.oldnational.com/terms-of-use/

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'old-national-bancorp-common-stock'
),
alias_values(alias) AS (
    VALUES
        ('Old National'),
        ('Old National Bank'),
        ('Old National Bancorp'),
        ('Old National Bancorp, Inc.')
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
    homepage_url = COALESCE(
        company.homepage_url,
        'https://www.oldnational.com/'
    ),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        'https://ir.oldnational.com/'
    ),
    newsroom_url = COALESCE(
        company.newsroom_url,
        'https://www.oldnational.com/resources/insights'
    ),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'holding_company_bank_brand',
            'source', 'official_company_website',
            'source_url', 'https://www.oldnational.com/terms-of-use/'
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
        'reason', 'holding_company_bank_brand',
        'canonical_name', company.name,
        'aliases', company.aliases,
        'source_url', 'https://www.oldnational.com/terms-of-use/',
        'migration', '0035_enrich_old_national_company_profile'
    )
FROM companies AS company
WHERE company.company_key = 'old-national-bancorp-common-stock';
