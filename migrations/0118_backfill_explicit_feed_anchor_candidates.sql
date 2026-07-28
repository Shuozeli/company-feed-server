-- Discovery historically classified every ordinary HTML anchor as an HTML
-- source, even when the anchor explicitly named an RSS/Atom subscription.
-- Re-emit those durable observations with the feed kind now used by the
-- runtime classifier. Validation still fetches and parses every candidate, so
-- an RSS directory or mislabeled link is rejected rather than activated.

CREATE TEMP TABLE explicit_feed_anchor_repairs
ON COMMIT DROP AS
WITH inferred AS (
    SELECT
        candidate.company_id,
        candidate.discovery_run_id,
        candidate.candidate_url,
        CASE
            WHEN EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    COALESCE(
                        candidate.evidence -> 'observations',
                        '[]'::jsonb
                    )
                ) AS observation
                WHERE COALESCE(observation ->> 'link_text', '')
                    ~* '(^|[^[:alnum:]])atom([^[:alnum:]]|$)'
            )
                OR candidate.candidate_url
                    ~* '(^|[/?&._=-])atom([/?&._=-]|$)'
                THEN 'atom'
            ELSE 'rss'
        END AS candidate_kind,
        GREATEST(candidate.confidence, 0.90) AS confidence,
        candidate.evidence || jsonb_build_object(
            'classification_repair',
            jsonb_build_object(
                'from_kind', 'html',
                'reason', 'explicit_feed_token_in_anchor_text_or_url',
                'policy', 'explicit-feed-anchor.v1',
                'migration',
                    '0118_backfill_explicit_feed_anchor_candidates',
                'reclassified_at', CURRENT_TIMESTAMP
            )
        ) AS evidence
    FROM source_candidates AS candidate
    WHERE
        candidate.candidate_kind = 'html'
        AND candidate.status = 'new'
        AND (
            EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    COALESCE(
                        candidate.evidence -> 'observations',
                        '[]'::jsonb
                    )
                ) AS observation
                WHERE COALESCE(observation ->> 'link_text', '')
                    ~* '(^|[^[:alnum:]])(rss|atom)([^[:alnum:]]|$)'
            )
            OR candidate.candidate_url
                ~* '(^|[/?&._=-])(rss|atom)([/?&._=-]|$)'
        )
)
SELECT inferred.*
FROM inferred
WHERE NOT EXISTS (
    SELECT 1
    FROM source_candidates AS existing
    WHERE
        existing.company_id = inferred.company_id
        AND existing.candidate_url = inferred.candidate_url
        AND existing.candidate_kind = inferred.candidate_kind
);

INSERT INTO source_candidates (
    company_id,
    discovery_run_id,
    candidate_url,
    candidate_kind,
    confidence,
    evidence,
    status
)
SELECT
    repair.company_id,
    repair.discovery_run_id,
    repair.candidate_url,
    repair.candidate_kind,
    repair.confidence,
    repair.evidence,
    'new'
FROM explicit_feed_anchor_repairs AS repair
ON CONFLICT (company_id, candidate_url, candidate_kind) DO NOTHING;

INSERT INTO event_log (event_type, company_id, payload)
SELECT
    'source_candidate.feed_kind_reclassified',
    repair.company_id,
    jsonb_build_object(
        'candidate_id', candidate.id,
        'candidate_url', repair.candidate_url,
        'from_kind', 'html',
        'to_kind', repair.candidate_kind,
        'reason', 'explicit_feed_token_in_anchor_text_or_url',
        'policy', 'explicit-feed-anchor.v1',
        'migration', '0118_backfill_explicit_feed_anchor_candidates'
    )
FROM
    explicit_feed_anchor_repairs AS repair
    JOIN source_candidates AS candidate
        ON candidate.company_id = repair.company_id
        AND candidate.candidate_url = repair.candidate_url
        AND candidate.candidate_kind = repair.candidate_kind;
