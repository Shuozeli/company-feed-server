CREATE INDEX feed_items_company_canonical_idx
ON feed_items (company_id, canonical_url);
