ALTER TABLE companies
ADD COLUMN discovery_enabled boolean NOT NULL DEFAULT true,
ADD COLUMN discovery_not_before timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP;

CREATE INDEX companies_discovery_schedule_idx
ON companies (discovery_not_before, ticker)
WHERE discovery_enabled;

CREATE TABLE company_import_runs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_name text NOT NULL,
    source_revision text,
    input_sha256 text NOT NULL,
    input_bytes bigint NOT NULL,
    input_rows integer NOT NULL,
    inserted_rows integer NOT NULL,
    updated_rows integer NOT NULL,
    unchanged_rows integer NOT NULL,
    activate_new boolean NOT NULL,
    discovery_cadence_seconds integer NOT NULL,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    completed_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT company_import_runs_source_not_blank
        CHECK (btrim(source_name) <> ''),
    CONSTRAINT company_import_runs_sha256_valid
        CHECK (input_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT company_import_runs_input_bytes_positive
        CHECK (input_bytes > 0),
    CONSTRAINT company_import_runs_input_rows_positive
        CHECK (input_rows > 0),
    CONSTRAINT company_import_runs_inserted_rows_nonnegative
        CHECK (inserted_rows >= 0),
    CONSTRAINT company_import_runs_updated_rows_nonnegative
        CHECK (updated_rows >= 0),
    CONSTRAINT company_import_runs_unchanged_rows_nonnegative
        CHECK (unchanged_rows >= 0),
    CONSTRAINT company_import_runs_row_counts_consistent
        CHECK (input_rows = inserted_rows + updated_rows + unchanged_rows),
    CONSTRAINT company_import_runs_cadence_positive
        CHECK (discovery_cadence_seconds > 0),
    UNIQUE (source_name, input_sha256)
);

CREATE INDEX company_import_runs_created_idx
ON company_import_runs (created_at DESC);

CREATE TABLE company_import_rows (
    import_run_id uuid NOT NULL
        REFERENCES company_import_runs(id) ON DELETE CASCADE,
    row_number integer NOT NULL,
    ticker text NOT NULL,
    action text NOT NULL,
    company_id uuid NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    source_record jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (import_run_id, row_number),
    CONSTRAINT company_import_rows_number_positive CHECK (row_number > 0),
    CONSTRAINT company_import_rows_ticker_not_blank CHECK (btrim(ticker) <> ''),
    CONSTRAINT company_import_rows_action_valid
        CHECK (action IN ('inserted', 'updated', 'unchanged')),
    UNIQUE (import_run_id, ticker)
);

CREATE INDEX company_import_rows_company_idx
ON company_import_rows (company_id, import_run_id);
