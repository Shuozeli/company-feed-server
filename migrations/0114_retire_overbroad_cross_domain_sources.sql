-- Correct five cross-domain expansion mistakes found by the first 1,000-company
-- campaign audit:
--   * Linum is a Crusoe customer, so its one Linum-specific case study remains
--     valid direct evidence, but Crusoe's complete blog is not Linum's feed.
--   * YC Lofty at lofty.ai is distinct from the real-estate CRM company at
--     lofty.com (formerly Chime Technologies).
--   * YC Luminal is a GPU compiler at luminal.com, not the publisher analytics
--     company at luminalanalytics.com.
--   * Financial-indexing Tilt at tilt.io is distinct from live-shopping
--     tilt.app and from Vox Pop Labs, the company from which Delphia originally
--     spun out.
--   * ZeroDev is owned by Offchain Labs, but one ZeroDev announcement does not
--     make Offchain Labs' complete corporate blog a ZeroDev publication.

CREATE TEMP TABLE company_publication_host_policies (
    company_key text PRIMARY KEY,
    verified_hosts text[] NOT NULL,
    excluded_hosts text[] NOT NULL,
    direct_evidence_excluded_hosts text[] NOT NULL
) ON COMMIT DROP;

INSERT INTO company_publication_host_policies (
    company_key,
    verified_hosts,
    excluded_hosts,
    direct_evidence_excluded_hosts
)
VALUES
    ('yc-activeloop', ARRAY['deeplake.ai'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-afriex', ARRAY['afriex.com'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-asklio', ARRAY['lio.ai'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-astroforge', ARRAY['astroforge.com'], ARRAY[]::text[], ARRAY[]::text[]),
    (
        'yc-autonomous-technologies-group',
        ARRAY['atg.science'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    ('yc-beanstalk', ARRAY['bean.money'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-bemlo', ARRAY['bemlo.se'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-bits-2', ARRAY['klausai.com'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-bolto', ARRAY[]::text[], ARRAY['crewai.com'], ARRAY['crewai.com']),
    ('yc-cambioml', ARRAY['cambioml.com'], ARRAY[]::text[], ARRAY[]::text[]),
    (
        'yc-hannah-life-technologies',
        ARRAY['twoplushealth.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-kapital-bank',
        ARRAY['kapital.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    ('yc-linc', ARRAY['withlinc.com'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-linum', ARRAY[]::text[], ARRAY['crusoe.ai'], ARRAY[]::text[]),
    (
        'yc-lofty',
        ARRAY[]::text[],
        ARRAY['lofty.com'],
        ARRAY['lofty.com']
    ),
    ('yc-lumari', ARRAY['lumari.ai'], ARRAY[]::text[], ARRAY[]::text[]),
    (
        'yc-luminal',
        ARRAY[]::text[],
        ARRAY['luminalanalytics.com'],
        ARRAY['luminalanalytics.com']
    ),
    (
        'yc-lus-brands',
        ARRAY['loveurcurls.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-mark-cuban-cost-plus-drug-company-pbc',
        ARRAY[]::text[],
        ARRAY['paytient.com'],
        ARRAY['paytient.com']
    ),
    (
        'yc-microhealth',
        ARRAY['microhealthllc.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-mindset-health',
        ARRAY['nervahealth.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-modernloop',
        ARRAY['modernloop.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-mudafy',
        ARRAY['mudafy.com.ar', 'mudafy.com.mx'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    ('yc-nash', ARRAY['nash.ai'], ARRAY[]::text[], ARRAY[]::text[]),
    (
        'yc-persephone-biosciences',
        ARRAY['persephone.bio'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-provision',
        ARRAY['provision.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-superunit',
        ARRAY['superunit.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-terrasoft',
        ARRAY['creatio.com'],
        ARRAY[]::text[],
        ARRAY[]::text[]
    ),
    (
        'yc-tilt-fka-delphia',
        ARRAY[]::text[],
        ARRAY['tilt.app', 'voxpoplabs.com'],
        ARRAY['tilt.app', 'voxpoplabs.com']
    ),
    ('yc-vaero', ARRAY['vaero.co'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-verto', ARRAY['verto.co'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-voize', ARRAY['voize.ai'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-vooma', ARRAY['vooma.com'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-wasp', ARRAY['opensaas.sh'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-woz', ARRAY['wozcode.com'], ARRAY[]::text[], ARRAY[]::text[]),
    ('yc-zerodev', ARRAY[]::text[], ARRAY['offchain.io'], ARRAY[]::text[]);

UPDATE companies AS company
SET
    metadata = jsonb_set(
        company.metadata,
        '{publication_host_policy}',
        COALESCE(company.metadata -> 'publication_host_policy', '{}'::jsonb)
            || jsonb_build_object(
                'verified_hosts', to_jsonb(policy.verified_hosts),
                'excluded_hosts', to_jsonb(policy.excluded_hosts),
                'direct_evidence_excluded_hosts',
                    to_jsonb(policy.direct_evidence_excluded_hosts),
                'policy', 'company-publication-host-policy.v1',
                'reviewed_at', CURRENT_TIMESTAMP,
                'migration', '0114_retire_overbroad_cross_domain_sources'
            ),
        true
    ),
    updated_at = CURRENT_TIMESTAMP
FROM company_publication_host_policies AS policy
WHERE company.company_key = policy.company_key;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'company.publication_host_policy_updated',
    company.id,
    jsonb_build_object(
        'verified_hosts', to_jsonb(policy.verified_hosts),
        'excluded_hosts', to_jsonb(policy.excluded_hosts),
        'direct_evidence_excluded_hosts',
            to_jsonb(policy.direct_evidence_excluded_hosts),
        'policy', 'company-publication-host-policy.v1',
        'migration', '0114_retire_overbroad_cross_domain_sources'
    )
FROM
    company_publication_host_policies AS policy
    JOIN companies AS company ON company.company_key = policy.company_key;

CREATE TEMP TABLE wrong_cross_domain_expansion_sources
ON COMMIT DROP AS
WITH rejected_source (
    company_key,
    source_url,
    claiming_company_name
) AS (
    VALUES
        (
            'yc-linum',
            'https://www.crusoe.ai/resources/blog',
            'Crusoe'
        ),
        (
            'yc-lofty',
            'https://lofty.com/blog',
            'Lofty (formerly Chime Technologies)'
        ),
        (
            'yc-lofty',
            'https://official.lofty.com/blog/rss.xml',
            'Lofty (formerly Chime Technologies)'
        ),
        (
            'yc-luminal',
            'https://www.luminalanalytics.com/blog',
            'Luminal Analytics'
        ),
        (
            'yc-tilt-fka-delphia',
            'https://tilt.app/blog',
            'Tilt (live shopping)'
        ),
        (
            'yc-tilt-fka-delphia',
            'https://voxpoplabs.com/news',
            'Vox Pop Labs'
        ),
        (
            'yc-zerodev',
            'https://www.offchain.io/blog',
            'Offchain Labs'
        )
)
SELECT
    source.id AS source_id,
    source.company_id,
    source.url,
    rejected_source.claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM
    rejected_source
    JOIN companies AS company
        ON company.company_key = rejected_source.company_key
    JOIN sources AS source
        ON source.company_id = company.id
WHERE
    source.status = 'approved'
    AND public_url_identity_key(source.url)
        = public_url_identity_key(rejected_source.source_url);

CREATE TEMP TABLE wrong_cross_domain_expansion_recipes
ON COMMIT DROP AS
SELECT
    recipe.id AS recipe_id,
    recipe.company_id,
    recipe.source_id,
    recipe.spec->>'publication_url' AS publication_url,
    repair.claiming_company_name,
    repair.reason
FROM
    wrong_cross_domain_expansion_sources AS repair
    JOIN company_news_recipes AS recipe
        ON recipe.source_id = repair.source_id
WHERE recipe.status IN ('active', 'stale');

CREATE TEMP TABLE wrong_cross_domain_expansion_candidates
ON COMMIT DROP AS
SELECT
    candidate.id AS candidate_id,
    candidate.company_id,
    candidate.accepted_source_id,
    candidate.candidate_url,
    candidate.status AS prior_status,
    CASE
        WHEN company.company_key = 'yc-linum'
            THEN 'Crusoe'
        WHEN company.company_key = 'yc-luminal'
            THEN 'Luminal Analytics'
        WHEN company.company_key = 'yc-tilt-fka-delphia'
            AND lower(candidate.candidate_url)
                ~ '^https?://([^/]+[.])?tilt[.]app(/|$)'
            THEN 'Tilt (live shopping)'
        WHEN company.company_key = 'yc-tilt-fka-delphia'
            THEN 'Vox Pop Labs'
        WHEN company.company_key = 'yc-zerodev'
            THEN 'Offchain Labs'
        ELSE 'Lofty (formerly Chime Technologies)'
    END AS claiming_company_name,
    'publication_owned_by_different_company'::text AS reason
FROM
    companies AS company
    JOIN source_candidates AS candidate
        ON candidate.company_id = company.id
WHERE
    candidate.status <> 'rejected'
    AND (
        (
            company.company_key = 'yc-linum'
            AND lower(candidate.candidate_url)
                ~ '^https?://([^/]+[.])?crusoe[.]ai(/|$)'
        )
        OR (
            company.company_key = 'yc-lofty'
            AND lower(candidate.candidate_url)
                ~ '^https?://([^/]+[.])?lofty[.]com(/|$)'
        )
        OR (
            company.company_key = 'yc-luminal'
            AND lower(candidate.candidate_url)
                ~ '^https?://([^/]+[.])?luminalanalytics[.]com(/|$)'
        )
        OR (
            company.company_key = 'yc-tilt-fka-delphia'
            AND (
                lower(candidate.candidate_url)
                    ~ '^https?://([^/]+[.])?tilt[.]app(/|$)'
                OR lower(candidate.candidate_url)
                    ~ '^https?://([^/]+[.])?voxpoplabs[.]com(/|$)'
            )
        )
        OR (
            company.company_key = 'yc-zerodev'
            AND lower(candidate.candidate_url)
                ~ '^https?://([^/]+[.])?offchain[.]io(/|$)'
        )
    );

CREATE TEMP TABLE wrong_cross_domain_expansion_items
ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    item.canonical_url,
    item.title,
    item.published_at,
    repair.claiming_company_name,
    repair.reason
FROM
    wrong_cross_domain_expansion_sources AS repair
    JOIN feed_items AS item ON item.source_id = repair.source_id
WHERE NOT item.is_private;

CREATE TEMP TABLE wrong_cross_domain_expansion_wave (
    event_id bigint PRIMARY KEY
) ON COMMIT DROP;

WITH repair_started AS (
    INSERT INTO event_log (event_type, payload)
    SELECT
        'company_news.wrong_cross_domain_repair_started',
        jsonb_build_object(
            'source_count',
                (SELECT count(*) FROM wrong_cross_domain_expansion_sources),
            'recipe_count',
                (SELECT count(*) FROM wrong_cross_domain_expansion_recipes),
            'candidate_count',
                (SELECT count(*) FROM wrong_cross_domain_expansion_candidates),
            'item_count',
                (SELECT count(*) FROM wrong_cross_domain_expansion_items),
            'company_count',
                (
                    SELECT count(DISTINCT company_id)
                    FROM wrong_cross_domain_expansion_sources
                ),
            'policy', 'cross-domain-company-ownership.v4',
            'migration', '0114_retire_overbroad_cross_domain_sources'
        )
    WHERE
        EXISTS (SELECT 1 FROM wrong_cross_domain_expansion_sources)
        OR EXISTS (SELECT 1 FROM wrong_cross_domain_expansion_candidates)
    RETURNING id
)
INSERT INTO wrong_cross_domain_expansion_wave (event_id)
SELECT id FROM repair_started;

UPDATE companies
SET
    aliases = aliases - 'Vox Pop Labs',
    metadata = metadata || jsonb_build_object(
        'alias_correction',
        jsonb_build_object(
            'removed_aliases', jsonb_build_array('Vox Pop Labs'),
            'reason', 'historical_parent_is_a_distinct_current_company',
            'claiming_company_name', 'Vox Pop Labs',
            'policy', 'cross-domain-company-ownership.v4',
            'corrected_at', CURRENT_TIMESTAMP,
            'migration', '0114_retire_overbroad_cross_domain_sources'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
WHERE company_key = 'yc-tilt-fka-delphia';

UPDATE company_news_recipes AS recipe
SET
    status = 'stale',
    stale_at = CURRENT_TIMESTAMP,
    stale_reason = repair.reason
FROM wrong_cross_domain_expansion_recipes AS repair
WHERE recipe.id = repair.recipe_id;

UPDATE company_news_recipe_state AS state
SET
    correctness_status = 'failing',
    rebuild_required = true,
    consecutive_correctness_failures = GREATEST(
        state.consecutive_correctness_failures,
        3
    ),
    reason = repair.reason,
    metadata = state.metadata || jsonb_build_object(
        'ownership_repair',
        jsonb_build_object(
            'policy', 'cross-domain-company-ownership.v4',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'repaired_at', CURRENT_TIMESTAMP,
            'migration', '0114_retire_overbroad_cross_domain_sources'
        )
    ),
    updated_at = CURRENT_TIMESTAMP
FROM
    wrong_cross_domain_expansion_recipes AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave
WHERE state.recipe_id = repair.recipe_id;

UPDATE sources AS source
SET
    status = 'disabled',
    public_export_allowed = false,
    metadata =
        source.metadata - 'active_recipe_id' - 'recipe_schema_version'
        || jsonb_build_object(
            'quality_disable',
            jsonb_build_object(
                'reason', repair.reason,
                'reversible', false,
                'policy', 'cross-domain-company-ownership.v4',
                'claiming_company_name', repair.claiming_company_name,
                'repair_wave_event_id', wave.event_id,
                'disabled_at', CURRENT_TIMESTAMP,
                'migration', '0114_retire_overbroad_cross_domain_sources'
            )
        )
FROM
    wrong_cross_domain_expansion_sources AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave
WHERE source.id = repair.source_id;

UPDATE source_candidates AS candidate
SET
    status = 'rejected',
    accepted_source_id = NULL,
    updated_at = CURRENT_TIMESTAMP
FROM wrong_cross_domain_expansion_candidates AS repair
WHERE candidate.id = repair.candidate_id;

INSERT INTO candidate_decisions (
    candidate_id,
    source_id,
    decision,
    decision_mode,
    actor,
    reason,
    metadata
)
SELECT
    repair.candidate_id,
    repair.accepted_source_id,
    'rejected',
    'automatic',
    'migration:0114',
    'candidate expands a different company publication',
    jsonb_build_object(
        'prior_status', repair.prior_status,
        'claiming_company_name', repair.claiming_company_name,
        'policy', 'cross-domain-company-ownership.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0114_retire_overbroad_cross_domain_sources'
    )
FROM
    wrong_cross_domain_expansion_candidates AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave;

UPDATE jobs AS job
SET
    status = 'cancelled',
    completed_at = CURRENT_TIMESTAMP,
    last_error = 'cancelled because publication belongs to a different company',
    locked_by = NULL,
    locked_at = NULL,
    heartbeat_at = NULL,
    lease_until = NULL,
    lease_token = NULL
WHERE
    job.status IN ('pending', 'running')
    AND (
        job.source_id IN (
            SELECT source_id
            FROM wrong_cross_domain_expansion_sources
        )
        OR job.candidate_id IN (
            SELECT candidate_id
            FROM wrong_cross_domain_expansion_candidates
        )
    );

UPDATE feed_items AS item
SET
    is_private = true,
    content_processing = item.content_processing || jsonb_build_object(
        'quality_quarantine',
        jsonb_build_object(
            'state', 'quarantined',
            'reason', repair.reason,
            'original_published_at', repair.published_at,
            'reversible', false,
            'policy', 'cross-domain-company-ownership.v4',
            'claiming_company_name', repair.claiming_company_name,
            'repair_wave_event_id', wave.event_id,
            'quarantined_at', CURRENT_TIMESTAMP
        )
    )
FROM
    wrong_cross_domain_expansion_items AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM wrong_cross_domain_expansion_items AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'company_news.recipe_stale',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'recipe_id', repair.recipe_id,
        'publication_url', repair.publication_url,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'rebuild_required', true,
        'policy', 'cross-domain-company-ownership.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0114_retire_overbroad_cross_domain_sources'
    )
FROM
    wrong_cross_domain_expansion_recipes AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source.quality_disabled',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'url', repair.url,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'reversible', false,
        'policy', 'cross-domain-company-ownership.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0114_retire_overbroad_cross_domain_sources'
    )
FROM
    wrong_cross_domain_expansion_sources AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'source_candidate.rejected',
    repair.company_id,
    repair.accepted_source_id,
    jsonb_build_object(
        'candidate_id', repair.candidate_id,
        'candidate_url', repair.candidate_url,
        'prior_status', repair.prior_status,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'decision_mode', 'automatic',
        'actor', 'migration:0114',
        'policy', 'cross-domain-company-ownership.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0114_retire_overbroad_cross_domain_sources'
    )
FROM
    wrong_cross_domain_expansion_candidates AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantined',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'canonical_url', repair.canonical_url,
        'title', repair.title,
        'published_at', repair.published_at,
        'claiming_company_name', repair.claiming_company_name,
        'reason', repair.reason,
        'reversible', false,
        'policy', 'cross-domain-company-ownership.v4',
        'repair_wave_event_id', wave.event_id,
        'migration', '0114_retire_overbroad_cross_domain_sources'
    )
FROM
    wrong_cross_domain_expansion_items AS repair
    CROSS JOIN wrong_cross_domain_expansion_wave AS wave;
