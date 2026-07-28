-- Before the persistence release path recognized every versioned
-- `recipe-listing-artifact.vN` policy, a successful recrawl could replace the
-- quarantine metadata while correctly leaving the item private. Restore that
-- immutable audit state from the original quarantine event. The crawler now
-- rejects terminal collection paths globally, so the item remains private
-- unless a future crawl independently proves it has become an article.

CREATE TEMP TABLE lost_versioned_listing_quarantines ON COMMIT DROP AS
SELECT DISTINCT ON ((event.payload->>'feed_item_id')::uuid)
    item.id AS feed_item_id,
    item.raw_crawl_item_id,
    item.company_id,
    item.source_id,
    event.payload->>'reason' AS reason,
    event.payload->>'policy' AS policy,
    event.payload->>'repair_wave_event_id' AS repair_wave_event_id,
    event.created_at AS originally_quarantined_at
FROM event_log AS event
JOIN feed_items AS item
  ON item.id = (event.payload->>'feed_item_id')::uuid
WHERE
    event.event_type = 'feed_item.quality_quarantined'
    AND event.payload->>'policy' ~ '^recipe-listing-artifact\.v[0-9]+$'
    AND item.is_private
    AND NOT (item.content_processing ? 'quality_quarantine')
ORDER BY
    (event.payload->>'feed_item_id')::uuid,
    event.created_at DESC,
    event.id DESC;

UPDATE feed_items AS item
SET content_processing = item.content_processing || jsonb_build_object(
    'quality_quarantine',
    jsonb_build_object(
        'state', 'quarantined',
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', repair.repair_wave_event_id,
        'originally_quarantined_at', repair.originally_quarantined_at,
        'restored_at', CURRENT_TIMESTAMP
    )
)
FROM lost_versioned_listing_quarantines AS repair
WHERE item.id = repair.feed_item_id;

UPDATE raw_crawl_items AS raw
SET
    processing_status = 'skipped',
    normalization_error = 'quality quarantine: ' || repair.reason,
    normalized_feed_item_id = NULL
FROM lost_versioned_listing_quarantines AS repair
WHERE raw.id = repair.raw_crawl_item_id;

INSERT INTO event_log (event_type, company_id, source_id, payload)
SELECT
    'feed_item.quality_quarantine_restored',
    repair.company_id,
    repair.source_id,
    jsonb_build_object(
        'feed_item_id', repair.feed_item_id,
        'reason', repair.reason,
        'reversible', true,
        'policy', repair.policy,
        'repair_wave_event_id', repair.repair_wave_event_id,
        'originally_quarantined_at', repair.originally_quarantined_at,
        'migration', '0024_restore_versioned_listing_quarantine'
    )
FROM lost_versioned_listing_quarantines AS repair;
