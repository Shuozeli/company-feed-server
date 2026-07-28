-- Preserve four evidence-backed company brands and their official publication
-- hosts. The imported security names are correct, but contain exchange security
-- suffixes or legal names that do not match the short brands used on the sites.
--
-- Primary evidence:
-- Wabtec: https://www.wabteccorp.com/newsroom
-- West: https://investor.westpharma.com/
-- WEX: https://ir.wexinc.com/overview/default.aspx
-- Dole plc: https://www.doleplc.com/investor-relations/news/default.aspx

CREATE TEMP TABLE official_brand_profile_values (
    company_key,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    blog_url,
    reason,
    source_url
) ON COMMIT DROP AS
SELECT *
FROM (
    VALUES
        (
            'westinghouse-air-brake-technologies-corporation-common-stock',
            'https://www.wabteccorp.com/',
            'https://ir.wabteccorp.com/',
            'https://www.wabteccorp.com/press-releases',
            'https://www.wabteccorp.com/trains-of-thought',
            'official_short_brand_and_domain',
            'https://www.wabteccorp.com/newsroom'
        ),
        (
            'west-pharmaceutical-services-inc-common-stock',
            'https://www.westpharma.com/',
            'https://investor.westpharma.com/',
            'https://www.westpharma.com/news-and-events',
            NULL,
            'official_short_brand_and_domain',
            'https://investor.westpharma.com/'
        ),
        (
            'wex-inc-common-stock',
            'https://www.wexinc.com/',
            'https://ir.wexinc.com/overview/default.aspx',
            'https://www.wexinc.com/resources/blog/wex-newsroom',
            'https://www.wexinc.com/resources/blog',
            'official_short_brand_and_domain',
            'https://ir.wexinc.com/overview/default.aspx'
        ),
        (
            'dole-plc-ordinary-shares',
            'https://www.doleplc.com/',
            'https://www.doleplc.com/investor-relations/default.aspx',
            'https://www.doleplc.com/investor-relations/news/default.aspx',
            NULL,
            'official_legal_name_domain',
            'https://www.doleplc.com/investor-relations/news/default.aspx'
        )
) AS values (
    company_key,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    blog_url,
    reason,
    source_url
);

WITH
alias_values (company_key, alias) AS (
    VALUES
        (
            'westinghouse-air-brake-technologies-corporation-common-stock',
            'Westinghouse Air Brake Technologies Corporation'
        ),
        (
            'westinghouse-air-brake-technologies-corporation-common-stock',
            'Wabtec'
        ),
        (
            'westinghouse-air-brake-technologies-corporation-common-stock',
            'Wabtec Corporation'
        ),
        (
            'west-pharmaceutical-services-inc-common-stock',
            'West'
        ),
        (
            'west-pharmaceutical-services-inc-common-stock',
            'West Pharmaceutical Services'
        ),
        (
            'west-pharmaceutical-services-inc-common-stock',
            'West Pharmaceutical Services, Inc.'
        ),
        (
            'wex-inc-common-stock',
            'WEX'
        ),
        (
            'wex-inc-common-stock',
            'WEX Inc.'
        ),
        (
            'wex-inc-common-stock',
            'WEX Incorporated'
        ),
        (
            'dole-plc-ordinary-shares',
            'Dole'
        ),
        (
            'dole-plc-ordinary-shares',
            'Dole plc'
        )
),
merged_aliases AS (
    SELECT
        company.id,
        jsonb_agg(DISTINCT values.alias ORDER BY values.alias) AS aliases
    FROM companies AS company
    JOIN official_brand_profile_values AS profile
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
    blog_url = COALESCE(company.blog_url, profile.blog_url),
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
    official_brand_profile_values AS profile
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
        'migration', '0039_enrich_official_brand_company_profiles'
    )
FROM companies AS company
JOIN official_brand_profile_values AS profile
    ON profile.company_key = company.company_key;
