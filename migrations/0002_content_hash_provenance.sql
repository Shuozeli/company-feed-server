ALTER TABLE feed_items
DROP CONSTRAINT feed_items_content_hash_key;

CREATE INDEX feed_items_content_hash_idx
ON feed_items (content_hash);
