# Schemas

The canonical file schemas use JSON Schema Draft 2020-12. `archive.schema.json`
contains the shared definitions; the other files are stable entry points for
individual document types.

`data-index.schema.json` is the lightweight browser bootstrap. The recent,
company-directory, category-directory, browse-page, and article-summary
schemas describe the bounded navigation files used by static clients; they
never embed article body text. Category pages contain at most 100 company
directory rows. The `1.1.0` data-index contract and category documents expose a
dataset generation and a separate taxonomy generation so clients can
invalidate category caches without treating article identity as changed.
Per-document schemas remain `1.0.0`.

The OpenAPI 3.1 contract in `openapi/openapi.json` references these schemas
instead of duplicating them.
