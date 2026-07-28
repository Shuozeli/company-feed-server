-- Public URLs that differ only by scheme, a conventional `www.` host alias,
-- a trailing slash, or a fragment identify the same article for overlap and
-- read-time deduplication. Preserve path case and query parameters: both may
-- carry resource identity on legacy publishers.

CREATE FUNCTION public_url_identity_key(value text)
RETURNS text
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
STRICT
AS $$
WITH fragmentless AS (
    SELECT split_part(value, '#', 1) AS value
), parts AS (
    SELECT
        split_part(value, '?', 1) AS resource,
        CASE
            WHEN position('?' IN value) > 0
                THEN substring(value FROM position('?' IN value))
            ELSE ''
        END AS query
    FROM fragmentless
), normalized_resource AS (
    SELECT
        rtrim(
            regexp_replace(resource, '^https?://(www\.)?', '', 'i'),
            '/'
        ) AS resource,
        query
    FROM parts
)
SELECT
    lower(split_part(resource, '/', 1))
        || substring(resource FROM length(split_part(resource, '/', 1)) + 1)
        || query
FROM normalized_resource
$$;

CREATE INDEX feed_items_company_public_url_identity_idx
    ON feed_items (company_id, public_url_identity_key(canonical_url))
    WHERE NOT is_private;
