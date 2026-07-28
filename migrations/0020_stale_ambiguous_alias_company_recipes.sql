-- Maven's imported historical alias "Lattice" exactly matches another active
-- company's canonical name. Before ambiguous aliases were excluded from
-- research requests, that collision activated two Lattice publications for
-- Maven. Preserve all recipe/item history while removing the wrong-company
-- recipes from active scheduling and public reads.

CREATE TEMP TABLE ambiguous_alias_company_recipes ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url
FROM company_news_recipes AS recipe
JOIN companies AS company ON company.id = recipe.company_id
WHERE
    company.company_key = 'yc-maven'
    AND recipe.status = 'active'
    AND lower(regexp_replace(
        recipe.spec->>'publication_url',
        '^https?://(www\.)?([^/]+).*$',
        '\2'
    )) = 'lattice.com';

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = 'ambiguous_alias_wrong_company_publication'
FROM ambiguous_alias_company_recipes AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures = GREATEST(
        state.consecutive_correctness_failures,
        3
    ),
    reason = 'ambiguous_alias_wrong_company_publication',
    metadata = state.metadata || jsonb_build_object(
        'ownership_repair',
        jsonb_build_object(
            'policy', 'ambiguous-alias-company-ownership.v1',
            'repair_wave_event_id', 102398,
            'conflicting_alias', 'Lattice',
            'correct_company_key', 'yc-lattice',
            'repaired_at', CURRENT_TIMESTAMP
        )
    )
FROM ambiguous_alias_company_recipes AS repair
WHERE state.recipe_id = repair.recipe_id;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled: recipe staled by ambiguous alias ownership repair'
FROM ambiguous_alias_company_recipes AS repair
WHERE
    job.source_id = repair.source_id
    AND job.job_type = 'crawl_source'
    AND job.status = 'pending';

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'reason', 'ambiguous_alias_wrong_company_publication',
        'rebuild_required', true,
        'policy', 'ambiguous-alias-company-ownership.v1',
        'repair_wave_event_id', 102398,
        'migration', '0020_stale_ambiguous_alias_company_recipes'
    )
FROM ambiguous_alias_company_recipes AS repair;
