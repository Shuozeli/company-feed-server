-- Preserve evidence-backed legacy and operating brands on two name-first
-- company profiles. Both current legal names were already imported correctly,
-- but their official publications continue to use established brand names.
--
-- Primary evidence:
-- NWPX: https://www.sec.gov/Archives/edgar/data/1001385/000143774926005861/nwpx20251231_10k.htm
-- Nvni: https://www.sec.gov/Archives/edgar/data/1965143/000121390026007445/ea0272361-f3_nvni.htm

CREATE TEMP TABLE legacy_brand_profile_values (
    company_key,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    previous_name,
    source_url
) ON COMMIT DROP AS
SELECT *
FROM (
    VALUES
        (
            'nwpx-infrastructure-inc-common-stock',
            'https://www.nwpipe.com/',
            NULL,
            'https://www.nwpipe.com/about/news',
            'Northwest Pipe Company',
            'https://www.sec.gov/Archives/edgar/data/1001385/000143774926005861/nwpx20251231_10k.htm'
        ),
        (
            'nvni-group-limited-ordinary-shares',
            'https://nuvini.ai/',
            'https://ir.nuvini.ai/',
            'https://ir.nuvini.ai/news-events/press-releases',
            NULL,
            'https://www.sec.gov/Archives/edgar/data/1965143/000121390026007445/ea0272361-f3_nvni.htm'
        )
) AS values (
    company_key,
    homepage_url,
    investor_relations_url,
    newsroom_url,
    previous_name,
    source_url
);

WITH
alias_values (company_key, alias) AS (
    VALUES
        ('nwpx-infrastructure-inc-common-stock', 'Northwest Pipe Company'),
        ('nwpx-infrastructure-inc-common-stock', 'Northwest Pipe'),
        ('nwpx-infrastructure-inc-common-stock', 'NWPX Infrastructure'),
        ('nvni-group-limited-ordinary-shares', 'Nuvini'),
        ('nvni-group-limited-ordinary-shares', 'Nuvini Group'),
        ('nvni-group-limited-ordinary-shares', 'Nuvini S.A.')
),
merged_aliases AS (
    SELECT
        company.id,
        jsonb_agg(DISTINCT values.alias ORDER BY values.alias) AS aliases
    FROM companies AS company
    JOIN legacy_brand_profile_values AS profile
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
            'reason', 'legacy_or_operating_brand',
            'previous_name', profile.previous_name,
            'source', 'sec_filing',
            'source_url', profile.source_url
        )
    )
FROM
    merged_aliases,
    legacy_brand_profile_values AS profile
WHERE
    company.id = merged_aliases.id
    AND profile.company_key = company.company_key;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'company.profile_enriched',
    company.id,
    jsonb_build_object(
        'policy', 'company-profile-enrichment.v1',
        'reason', 'legacy_or_operating_brand',
        'canonical_name', company.name,
        'aliases', company.aliases,
        'source_url', profile.source_url,
        'migration', '0033_enrich_legacy_brand_company_profiles'
    )
FROM companies AS company
JOIN legacy_brand_profile_values AS profile
    ON profile.company_key = company.company_key;
