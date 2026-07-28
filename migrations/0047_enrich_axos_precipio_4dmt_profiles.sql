-- Preserve official company identities and publication entry points exposed by
-- live recipe health checks. These imported security names had no company
-- profile URLs, so the extraction adapter could not reliably distinguish the
-- official publication from a broad third-party release archive.
--
-- Primary evidence:
-- Axos Financial: https://investors.axosfinancial.com/news-events/press-releases/
-- Precipio: https://www.precipiodx.com/investors/press-releases/
-- 4D Molecular Therapeutics:
-- https://ir.4dmoleculartherapeutics.com/press-releases/
-- Applied Optoelectronics: https://newsroom.ao-inc.com/
-- ProKidney: https://investors.prokidney.com/news-events/news-releases

CREATE TEMP TABLE recipe_identity_profile_values (
    company_key,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    reason,
    source_url
) ON COMMIT DROP AS
SELECT *
FROM (
    VALUES
        (
            'axos-financial-inc-common-stock',
            'https://www.axosbank.com/',
            'https://investors.axosfinancial.com/',
            'https://investors.axosfinancial.com/news-events/press-releases/',
            'official_parent_and_investor_publication_domains',
            'https://investors.axosfinancial.com/news-events/press-releases/'
        ),
        (
            'precipio-inc-common-stock',
            'https://www.precipiodx.com/',
            'https://www.precipiodx.com/investors/',
            'https://www.precipiodx.com/investors/press-releases/',
            'official_company_publication_domain',
            'https://www.precipiodx.com/investors/press-releases/'
        ),
        (
            '4d-molecular-therapeutics-inc-common-stock',
            'https://4dmoleculartherapeutics.com/',
            'https://ir.4dmoleculartherapeutics.com/',
            'https://ir.4dmoleculartherapeutics.com/press-releases/',
            'official_company_and_investor_publication_domains',
            'https://ir.4dmoleculartherapeutics.com/press-releases/'
        ),
        (
            'applied-optoelectronics-inc-common-stock',
            'https://ao-inc.com/',
            'https://investors.ao-inc.com/',
            'https://newsroom.ao-inc.com/',
            'official_company_brand_and_newsroom_domains',
            'https://newsroom.ao-inc.com/'
        ),
        (
            'prokidney-corp-class-a-ordinary-shares',
            'https://prokidney.com/',
            'https://investors.prokidney.com/',
            'https://investors.prokidney.com/news-events/news-releases',
            'official_company_and_investor_publication_domains',
            'https://investors.prokidney.com/news-events/news-releases'
        )
) AS values (
    company_key,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    reason,
    source_url
);

WITH
alias_values (company_key, alias) AS (
    VALUES
        ('axos-financial-inc-common-stock', 'Axos'),
        ('axos-financial-inc-common-stock', 'Axos Bank'),
        ('axos-financial-inc-common-stock', 'Axos Financial'),
        ('precipio-inc-common-stock', 'Precipio'),
        ('precipio-inc-common-stock', 'Precipio Inc.'),
        (
            '4d-molecular-therapeutics-inc-common-stock',
            '4D Molecular Therapeutics'
        ),
        ('4d-molecular-therapeutics-inc-common-stock', '4DMT'),
        (
            'applied-optoelectronics-inc-common-stock',
            'Applied Optoelectronics'
        ),
        ('applied-optoelectronics-inc-common-stock', 'AOI'),
        ('prokidney-corp-class-a-ordinary-shares', 'ProKidney'),
        ('prokidney-corp-class-a-ordinary-shares', 'ProKidney Corp.')
),
merged_aliases AS (
    SELECT
        company.id,
        jsonb_agg(DISTINCT values.alias ORDER BY values.alias) AS aliases
    FROM companies AS company
    JOIN recipe_identity_profile_values AS profile
        ON profile.company_key = company.company_key
    CROSS JOIN LATERAL (
        SELECT jsonb_array_elements_text(company.aliases) AS alias
        UNION ALL
        SELECT alias
        FROM alias_values
        WHERE alias_values.company_key = company.company_key
    ) AS values
    GROUP BY company.id
)
UPDATE companies AS company
SET
    aliases = merged_aliases.aliases,
    homepage_url = COALESCE(company.homepage_url, profile.homepage_url),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        profile.investor_relations_url
    ),
    newsroom_url = COALESCE(company.newsroom_url, profile.newsroom_url),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', profile.reason,
            'source', 'official_company_website',
            'source_url', profile.source_url
        )
    )
FROM
    merged_aliases,
    recipe_identity_profile_values AS profile
WHERE
    company.id = merged_aliases.id
    AND profile.company_key = company.company_key;

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
        'migration', '0047_enrich_axos_precipio_4dmt_profiles'
    )
FROM companies AS company
JOIN recipe_identity_profile_values AS profile
    ON profile.company_key = company.company_key;
