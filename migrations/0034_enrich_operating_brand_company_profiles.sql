-- Preserve evidence-backed operating brands and official publication hosts on
-- two name-first company profiles. The imported security names are correct,
-- but they do not contain the brands used by these official publications.
--
-- Primary evidence:
-- CSPi/ARIA: https://www.cspi.com/about-us/
-- VF Corporation: https://www.vfc.com/

CREATE TEMP TABLE operating_brand_profile_values (
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
            'csp-inc-common-stock',
            'https://www.cspi.com/',
            'https://investorrelations.cspi.com/',
            'https://blog.ariacybersecurity.com/blog',
            'operating_brand',
            'https://www.cspi.com/about-us/'
        ),
        (
            'v-f-corporation-common-stock',
            'https://www.vfc.com/',
            'https://www.vfc.com/investors',
            'https://www.vfc.com/news',
            'official_brand_and_publication_host',
            'https://www.vfc.com/'
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
        ('csp-inc-common-stock', 'CSP Inc.'),
        ('csp-inc-common-stock', 'CSPi'),
        ('csp-inc-common-stock', 'ARIA Cybersecurity'),
        ('csp-inc-common-stock', 'ARIA Cybersecurity Solutions'),
        ('v-f-corporation-common-stock', 'VF Corporation'),
        ('v-f-corporation-common-stock', 'VF Corp.'),
        ('v-f-corporation-common-stock', 'VF')
),
merged_aliases AS (
    SELECT
        company.id,
        jsonb_agg(DISTINCT values.alias ORDER BY values.alias) AS aliases
    FROM companies AS company
    JOIN operating_brand_profile_values AS profile
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
    operating_brand_profile_values AS profile
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
        'migration', '0034_enrich_operating_brand_company_profiles'
    )
FROM companies AS company
JOIN operating_brand_profile_values AS profile
    ON profile.company_key = company.company_key;
