CREATE TEMP TABLE private_quarantine_metadata_repairs ON COMMIT DROP AS
SELECT
    item.id AS feed_item_id,
    item.company_id,
    item.source_id,
    quarantine_event.id AS quarantine_event_id,
    quarantine_event.created_at AS quarantined_at,
    quarantine_event.payload
FROM feed_items AS item
JOIN LATERAL (
    SELECT event.id, event.created_at, event.payload
    FROM event_log AS event
    WHERE
        event.event_type = 'feed_item.quality_quarantined'
        AND event.payload ->> 'feed_item_id' = item.id::text
    ORDER BY event.id DESC
    LIMIT 1
) AS quarantine_event ON true
WHERE
    item.is_private
    AND NOT (item.content_processing ? 'quality_quarantine');

UPDATE feed_items AS item
SET content_processing = item.content_processing || jsonb_build_object(
    'quality_quarantine',
    jsonb_build_object(
        'state', 'quarantined',
        'reason',
            COALESCE(
                repair.payload ->> 'reason',
                'historical_quality_quarantine'
            ),
        'policy',
            COALESCE(
                repair.payload ->> 'policy',
                'historical-quality-quarantine.v1'
            ),
        'reversible',
            COALESCE(repair.payload -> 'reversible', 'false'::jsonb),
        'quarantined_at', to_jsonb(repair.quarantined_at),
        'restored_from_event_id', repair.quarantine_event_id,
        'restored_by_migration', '0086_restore_private_quarantine_metadata'
    )
)
FROM private_quarantine_metadata_repairs AS repair
WHERE item.id = repair.feed_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantine_restored',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'restored_from_event_id', repair.quarantine_event_id,
        'policy',
            COALESCE(
                repair.payload ->> 'policy',
                'historical-quality-quarantine.v1'
            ),
        'reason',
            COALESCE(
                repair.payload ->> 'reason',
                'historical_quality_quarantine'
            ),
        'migration', '0086_restore_private_quarantine_metadata'
    )
FROM private_quarantine_metadata_repairs AS repair;
