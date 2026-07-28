-- Cross-company ownership checks start from an article URL and do not yet know
-- the claiming company. The existing company-prefixed canonical identity index
-- cannot serve those lookups, which otherwise scan every public feed item once
-- per recipe candidate.

CREATE INDEX IF NOT EXISTS feed_items_public_canonical_url_identity_idx
    ON feed_items (public_url_identity_key(canonical_url))
    WHERE NOT is_private;

CREATE INDEX IF NOT EXISTS feed_items_public_url_identity_idx
    ON feed_items (public_url_identity_key(url))
    WHERE NOT is_private;

CREATE INDEX IF NOT EXISTS feed_items_public_external_id_identity_idx
    ON feed_items (public_url_identity_key(external_id))
    WHERE NOT is_private;
