-- Tickers are optional listing metadata, never company identity.
--
-- Migration 0005 copied every historical symbol into company_listings. Remove
-- the nullable compatibility columns now that runtime and import paths are
-- name/company-key based.

ALTER TABLE companies
DROP COLUMN ticker;

ALTER TABLE company_import_rows
DROP COLUMN ticker;
