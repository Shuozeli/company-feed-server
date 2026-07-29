# Schemas

The canonical file schemas use JSON Schema Draft 2020-12. `archive.schema.json`
contains the shared definitions; the other files are stable entry points for
individual document types.

`data-index.schema.json` is the lightweight browser bootstrap. The recent,
company-directory, browse-page, and article-summary schemas describe the
bounded navigation files used by static clients; they never embed article
body text.

The OpenAPI 3.1 contract in `openapi/openapi.json` references these schemas
instead of duplicating them.
