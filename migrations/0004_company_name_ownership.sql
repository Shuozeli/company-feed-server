ALTER TABLE companies
ADD COLUMN name_source text;

UPDATE companies AS company
SET name_source = CASE
    WHEN EXISTS (
        SELECT 1
        FROM company_import_rows AS import_row
        WHERE
            import_row.company_id = company.id
            AND import_row.action = 'inserted'
    )
    THEN 'universe'
    ELSE 'operator'
END;

ALTER TABLE companies
ALTER COLUMN name_source SET DEFAULT 'operator',
ALTER COLUMN name_source SET NOT NULL;

ALTER TABLE companies
ADD CONSTRAINT companies_name_source_valid
CHECK (name_source IN ('seed_config', 'universe', 'operator'));
