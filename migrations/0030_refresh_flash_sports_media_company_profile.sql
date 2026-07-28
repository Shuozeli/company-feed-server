-- urban-gro completed a corporate name change after the imported universe
-- snapshot. Refresh the name-first company profile so recipe validation can
-- recognize both the historical issuer name and the current publication name.
--
-- Primary evidence:
-- https://www.sec.gov/Archives/edgar/data/1706524/000121390026068776/ea0294769-8k_flash.htm

WITH target AS (
    SELECT id
    FROM companies
    WHERE company_key = 'urban-gro-inc-common-stock'
),
alias_values(alias) AS (
    VALUES
        ('urban-gro, Inc.'),
        ('urban-gro Inc. Common Stock'),
        ('Flash Sports and Media, Inc.'),
        ('Flash Sport & Media Inc'),
        ('Flash Sports & Media Holdings')
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
    name = 'Flash Sports & Media Holdings, Inc.',
    name_source = 'operator',
    aliases = merged_aliases.aliases,
    homepage_url = COALESCE(
        company.homepage_url,
        'https://flashsportsandmedia.com/'
    ),
    metadata = company.metadata || jsonb_build_object(
        'profile_refresh',
        jsonb_build_object(
            'effective_date', '2026-06-12',
            'previous_name', 'urban-gro, Inc.',
            'reason', 'corporate_name_change',
            'source', 'sec_form_8_k',
            'source_url',
                'https://www.sec.gov/Archives/edgar/data/1706524/000121390026068776/ea0294769-8k_flash.htm'
        )
    )
FROM merged_aliases
WHERE company.id = merged_aliases.id;

UPDATE company_listings AS listing
SET
    is_primary = false,
    metadata = listing.metadata || jsonb_build_object(
        'valid_to', '2026-06-12',
        'superseded_by', 'FLZH',
        'source', 'sec_form_8_k'
    )
WHERE
    listing.company_id = (
        SELECT id
        FROM companies
        WHERE company_key = 'urban-gro-inc-common-stock'
    )
    AND listing.ticker = 'UGRO';

INSERT INTO company_listings (
    company_id,
    ticker,
    exchange,
    is_primary,
    metadata
)
SELECT
    company.id,
    'FLZH',
    COALESCE(
        (
            SELECT listing.exchange
            FROM company_listings AS listing
            WHERE listing.company_id = company.id
              AND listing.ticker = 'UGRO'
            ORDER BY listing.created_at
            LIMIT 1
        ),
        ''
    ),
    true,
    jsonb_build_object(
        'valid_from', '2026-06-12',
        'previous_ticker', 'UGRO',
        'source', 'sec_form_8_k',
        'source_url',
            'https://www.sec.gov/Archives/edgar/data/1706524/000121390026068776/ea0294769-8k_flash.htm'
    )
FROM companies AS company
WHERE company.company_key = 'urban-gro-inc-common-stock'
ON CONFLICT (company_id, ticker, exchange)
DO UPDATE SET
    is_primary = EXCLUDED.is_primary,
    metadata = company_listings.metadata || EXCLUDED.metadata;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'company.profile_refreshed',
    company.id,
    jsonb_build_object(
        'policy', 'company-profile-refresh.v1',
        'previous_name', 'urban-gro Inc. Common Stock',
        'current_name', company.name,
        'previous_primary_listing', 'UGRO',
        'current_primary_listing', 'FLZH',
        'effective_date', '2026-06-12',
        'source_url',
            'https://www.sec.gov/Archives/edgar/data/1706524/000121390026068776/ea0294769-8k_flash.htm'
    )
FROM companies AS company
WHERE company.company_key = 'urban-gro-inc-common-stock';
