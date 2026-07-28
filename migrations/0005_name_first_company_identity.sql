-- Companies are organizations, not exchange listings.
--
-- `company_key` is a stable, human-readable operational key. The required
-- `name` remains the input used for web discovery. Public-market symbols move
-- to `company_listings`, where a company may have zero, one, or many rows.

ALTER TABLE companies
ADD COLUMN company_key text,
ADD COLUMN aliases jsonb NOT NULL DEFAULT '[]'::jsonb,
ADD COLUMN ownership_status text NOT NULL DEFAULT 'unknown',
ADD COLUMN lifecycle_status text NOT NULL DEFAULT 'unknown';

WITH normalized AS (
    SELECT
        id,
        COALESCE(
            NULLIF(
                trim(
                    BOTH '-'
                    FROM regexp_replace(lower(name), '[^a-z0-9]+', '-', 'g')
                ),
                ''
            ),
            'company'
        ) AS base_key,
        created_at
    FROM companies
),
ranked AS (
    SELECT
        id,
        base_key,
        row_number() OVER (
            PARTITION BY base_key
            ORDER BY created_at, id
        ) AS collision_index
    FROM normalized
)
UPDATE companies AS company
SET company_key = CASE
    WHEN ranked.collision_index = 1 THEN ranked.base_key
    ELSE ranked.base_key || '-' || left(replace(company.id::text, '-', ''), 8)
END
FROM ranked
WHERE ranked.id = company.id;

ALTER TABLE companies
ALTER COLUMN company_key SET NOT NULL,
ALTER COLUMN ticker DROP NOT NULL;

ALTER TABLE companies
ADD CONSTRAINT companies_company_key_unique UNIQUE (company_key),
ADD CONSTRAINT companies_company_key_not_blank CHECK (btrim(company_key) <> ''),
ADD CONSTRAINT companies_company_key_format CHECK (
    company_key ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
),
ADD CONSTRAINT companies_aliases_array CHECK (jsonb_typeof(aliases) = 'array'),
ADD CONSTRAINT companies_ownership_status_valid CHECK (
    ownership_status IN ('public', 'private', 'state_owned', 'cooperative', 'unknown')
),
ADD CONSTRAINT companies_lifecycle_status_valid CHECK (
    lifecycle_status IN ('active', 'acquired', 'inactive', 'unknown')
);

COMMENT ON COLUMN companies.ticker IS
    'Deprecated compatibility field. Use company_listings; discovery uses companies.name.';

DROP INDEX companies_discovery_schedule_idx;

CREATE INDEX companies_discovery_schedule_idx
ON companies (discovery_not_before, company_key)
WHERE discovery_enabled;

CREATE INDEX companies_name_lower_idx
ON companies (lower(name));

CREATE TABLE company_listings (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    ticker text NOT NULL,
    exchange text NOT NULL DEFAULT '',
    is_primary boolean NOT NULL DEFAULT false,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT company_listings_ticker_not_blank CHECK (btrim(ticker) <> ''),
    CONSTRAINT company_listings_ticker_uppercase CHECK (ticker = upper(ticker)),
    CONSTRAINT company_listings_metadata_object CHECK (
        jsonb_typeof(metadata) = 'object'
    ),
    UNIQUE (company_id, ticker, exchange)
);

CREATE INDEX company_listings_company_idx
ON company_listings (company_id, is_primary DESC, ticker);

CREATE INDEX company_listings_ticker_idx
ON company_listings (ticker);

CREATE UNIQUE INDEX company_listings_one_primary_idx
ON company_listings (company_id)
WHERE is_primary;

CREATE TRIGGER company_listings_set_updated_at
BEFORE UPDATE ON company_listings
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

INSERT INTO company_listings (
    company_id,
    ticker,
    exchange,
    is_primary,
    metadata
)
SELECT
    id,
    ticker,
    COALESCE(metadata #>> '{universe,exchange}', ''),
    true,
    jsonb_build_object('migrated_from', 'companies.ticker')
FROM companies
WHERE ticker IS NOT NULL
ON CONFLICT (company_id, ticker, exchange) DO NOTHING;

CREATE TABLE company_external_ids (
    source_name text NOT NULL,
    source_company_id text NOT NULL,
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (source_name, source_company_id),
    CONSTRAINT company_external_ids_source_not_blank CHECK (
        btrim(source_name) <> ''
    ),
    CONSTRAINT company_external_ids_value_not_blank CHECK (
        btrim(source_company_id) <> ''
    ),
    CONSTRAINT company_external_ids_metadata_object CHECK (
        jsonb_typeof(metadata) = 'object'
    )
);

CREATE INDEX company_external_ids_company_idx
ON company_external_ids (company_id);

CREATE TRIGGER company_external_ids_set_updated_at
BEFORE UPDATE ON company_external_ids
FOR EACH ROW EXECUTE FUNCTION set_updated_at();

ALTER TABLE company_import_rows
ALTER COLUMN ticker DROP NOT NULL,
ADD COLUMN source_company_id text,
ADD COLUMN company_key text,
ADD COLUMN company_name text;

ALTER TABLE company_import_rows
DROP CONSTRAINT company_import_rows_import_run_id_ticker_key;

UPDATE company_import_rows AS import_row
SET
    source_company_id = import_row.ticker,
    company_key = company.company_key,
    company_name = company.name
FROM companies AS company
WHERE company.id = import_row.company_id;

ALTER TABLE company_import_rows
ALTER COLUMN source_company_id SET NOT NULL,
ALTER COLUMN company_key SET NOT NULL,
ALTER COLUMN company_name SET NOT NULL,
ADD CONSTRAINT company_import_rows_source_id_not_blank CHECK (
    btrim(source_company_id) <> ''
),
ADD CONSTRAINT company_import_rows_company_key_not_blank CHECK (
    btrim(company_key) <> ''
),
ADD CONSTRAINT company_import_rows_company_name_not_blank CHECK (
    btrim(company_name) <> ''
),
ADD CONSTRAINT company_import_rows_source_id_unique UNIQUE (
    import_run_id,
    source_company_id
);

INSERT INTO company_external_ids (
    source_name,
    source_company_id,
    company_id,
    metadata
)
SELECT
    import_run.source_name,
    import_row.source_company_id,
    min(import_row.company_id::text)::uuid,
    jsonb_build_object('migrated_from', 'company_import_rows')
FROM company_import_rows AS import_row
INNER JOIN company_import_runs AS import_run
    ON import_run.id = import_row.import_run_id
GROUP BY import_run.source_name, import_row.source_company_id
HAVING count(DISTINCT import_row.company_id) = 1
ON CONFLICT (source_name, source_company_id) DO NOTHING;
