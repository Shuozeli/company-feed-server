-- The migrated security universe represents active public companies at the
-- snapshot date. Preserve symbols in company_listings, then retire the legacy
-- one-symbol-per-company field so no runtime path can treat it as identity.

UPDATE companies AS company
SET
    ownership_status = CASE
        WHEN company.ownership_status = 'unknown' THEN 'public'
        ELSE company.ownership_status
    END,
    lifecycle_status = CASE
        WHEN company.lifecycle_status = 'unknown' THEN 'active'
        ELSE company.lifecycle_status
    END
WHERE EXISTS (
    SELECT 1
    FROM company_listings AS listing
    WHERE listing.company_id = company.id
);

UPDATE companies
SET ticker = NULL
WHERE ticker IS NOT NULL;
