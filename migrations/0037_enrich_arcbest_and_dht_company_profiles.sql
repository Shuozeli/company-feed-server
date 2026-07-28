-- Preserve the official compact ArcBest domain and DHT tanker brand domain.
-- Both imported issuer names are correct, but neither company had durable web
-- profile fields, so company-owned editorial pages were treated as unrelated.
--
-- Primary evidence:
-- https://arcb.com/about-arcbest
-- https://arcb.com/privacy-policy
-- https://www.dhtankers.com/
-- https://www.dhtankers.com/investor-relations/press-releases/

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'arcbest-corporation-common-stock'
),
alias_values(alias) AS (
    VALUES
        ('ArcBest'),
        ('ArcBest Corporation')
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
    homepage_url = COALESCE(company.homepage_url, 'https://arcb.com/'),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        'https://investors.arcb.com/'
    ),
    newsroom_url = COALESCE(
        company.newsroom_url,
        'https://investors.arcb.com/news-events/news/default.aspx'
    ),
    blog_url = COALESCE(company.blog_url, 'https://arcb.com/blog'),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_compact_company_domain',
            'source', 'official_company_website',
            'source_url', 'https://arcb.com/privacy-policy'
        )
    )
FROM merged_aliases
WHERE company.id = merged_aliases.id;

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'dht-holdings-inc'
),
alias_values(alias) AS (
    VALUES
        ('DHT'),
        ('DHT Holdings'),
        ('DHT Holdings, Inc.')
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
        'https://www.dhtankers.com/'
    ),
    investor_relations_url = COALESCE(
        company.investor_relations_url,
        'https://www.dhtankers.com/investor-relations/'
    ),
    newsroom_url = COALESCE(
        company.newsroom_url,
        'https://www.dhtankers.com/investor-relations/press-releases/'
    ),
    metadata = company.metadata || jsonb_build_object(
        'profile_enrichment',
        jsonb_build_object(
            'reason', 'official_tanker_brand_domain',
            'source', 'official_company_website',
            'source_url', 'https://www.dhtankers.com/'
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
        'migration', '0037_enrich_arcbest_and_dht_company_profiles'
    )
FROM companies AS company
JOIN (
    VALUES
        (
            'arcbest-corporation-common-stock',
            'official_compact_company_domain',
            'https://arcb.com/privacy-policy'
        ),
        (
            'dht-holdings-inc',
            'official_tanker_brand_domain',
            'https://www.dhtankers.com/'
        )
) AS profile(company_key, reason, source_url)
    ON profile.company_key = company.company_key;
