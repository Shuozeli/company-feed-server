-- Preserve official company identities and publication hosts exposed by the
-- first-pass recipe health canaries. These imported security names did not yet
-- carry company profiles, so a later crawl could not distinguish a verified
-- first-party publication from a third-party collection whose individual
-- titles must continue to name the company.
--
-- Primary evidence:
-- Entergy: https://www.entergy.com/news
-- F.N.B.: https://www.fnb-online.com/about-us/newsroom/press-releases
-- Occidental/Oxy: https://www.oxy.com/news/
-- W. P. Carey: https://www.wpcarey.com/
-- Xos: https://www.xostrucks.com/blog/
-- Envirotech Vehicles: https://www.evtvusa.com/company/news/

CREATE TEMP TABLE recipe_identity_profile_values (
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
            'entergy-corporation-common-stock',
            'https://www.entergy.com/',
            'https://investors.entergy.com/',
            'https://www.entergy.com/news',
            NULL,
            'official_legal_name_domain',
            'https://www.entergy.com/news'
        ),
        (
            'f-n-b-corporation-common-stock',
            'https://www.fnb-online.com/',
            NULL,
            'https://www.fnb-online.com/about-us/newsroom/press-releases',
            NULL,
            'official_company_name_initialism_domain',
            'https://www.fnb-online.com/about-us/newsroom/press-releases'
        ),
        (
            'occidental-petroleum-corporation-common-stock',
            'https://www.oxy.com/',
            'https://investors.oxy.com/',
            'https://www.oxy.com/news/',
            NULL,
            'official_operating_brand_domain',
            'https://www.oxy.com/news/'
        ),
        (
            'w-p-carey-inc-reit',
            'https://www.wpcarey.com/',
            'https://ir.wpcarey.com/',
            NULL,
            'https://www.wpcarey.com/blog',
            'official_legal_name_domain',
            'https://www.wpcarey.com/'
        ),
        (
            'xos-inc-common-stock',
            'https://www.xostrucks.com/',
            NULL,
            NULL,
            'https://www.xostrucks.com/blog/',
            'official_short_brand_domain',
            'https://www.xostrucks.com/blog/'
        ),
        (
            'envirotech-vehicles-inc-common-stock',
            'https://www.evtvusa.com/',
            'https://www.evtvusa.com/company/investor-relations',
            'https://www.evtvusa.com/company/news/',
            NULL,
            'official_company_publication_domain',
            'https://www.evtvusa.com/company/news/'
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
        ('entergy-corporation-common-stock', 'Entergy'),
        ('entergy-corporation-common-stock', 'Entergy Corporation'),
        ('f-n-b-corporation-common-stock', 'F.N.B. Corporation'),
        ('f-n-b-corporation-common-stock', 'First National Bank'),
        (
            'occidental-petroleum-corporation-common-stock',
            'Occidental'
        ),
        (
            'occidental-petroleum-corporation-common-stock',
            'Occidental Petroleum'
        ),
        (
            'occidental-petroleum-corporation-common-stock',
            'Oxy'
        ),
        ('w-p-carey-inc-reit', 'W. P. Carey'),
        ('w-p-carey-inc-reit', 'WP Carey'),
        ('xos-inc-common-stock', 'Xos'),
        (
            'envirotech-vehicles-inc-common-stock',
            'Envirotech'
        ),
        (
            'envirotech-vehicles-inc-common-stock',
            'Envirotech Vehicles'
        )
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
        'migration', '0045_enrich_recipe_identity_profiles'
    )
FROM companies AS company
JOIN recipe_identity_profile_values AS profile
    ON profile.company_key = company.company_key;

-- The broad Entergy newsroom became a complete duplicate of the independently
-- healthy, more specific release publication. Retire only that redundant
-- active recipe; the narrower recipe remains live.
CREATE TEMP TABLE redundant_recipe_supersessions ON COMMIT DROP AS
SELECT
    broad.id AS recipe_id,
    broad.company_id,
    broad.source_id,
    specific.id AS superseded_by_recipe_id
FROM company_news_recipes AS broad
JOIN companies AS company ON company.id = broad.company_id
JOIN company_news_recipes AS specific
    ON specific.company_id = broad.company_id
    AND specific.status = 'active'
    AND specific.id <> broad.id
WHERE
    company.company_key = 'entergy-corporation-common-stock'
    AND broad.status = 'active'
    AND rtrim(broad.spec ->> 'publication_url', '/') =
        'https://www.entergy.com/news'
    AND rtrim(specific.spec ->> 'publication_url', '/') =
        'https://www.entergy.com/news/releases';

UPDATE company_news_recipes AS recipe
SET status = 'superseded'
FROM redundant_recipe_supersessions AS supersession
WHERE recipe.id = supersession.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    rebuild_required = false,
    reason = 'superseded_by_more_specific_active_recipe',
    metadata = state.metadata || jsonb_build_object(
        'supersession',
        jsonb_build_object(
            'policy', 'company-news-publication-overlap.v1',
            'superseded_by_recipe_id',
            supersession.superseded_by_recipe_id,
            'migration',
            '0045_enrich_recipe_identity_profiles'
        )
    )
FROM redundant_recipe_supersessions AS supersession
WHERE state.recipe_id = supersession.recipe_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_superseded',
    supersession.company_id,
    supersession.source_id,
    jsonb_build_object(
        'recipe_id', supersession.recipe_id,
        'superseded_by_recipe_id', supersession.superseded_by_recipe_id,
        'reason', 'overlaps_active_recipe',
        'policy', 'company-news-publication-overlap.v1',
        'migration', '0045_enrich_recipe_identity_profiles'
    )
FROM redundant_recipe_supersessions AS supersession;
