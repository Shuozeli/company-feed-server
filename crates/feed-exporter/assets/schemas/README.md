# Schemas

The canonical file schemas use JSON Schema Draft 2020-12. `archive.schema.json`
contains the shared definitions; the other files are stable entry points for
individual document types.

The OpenAPI 3.1 contract in `openapi/openapi.json` references these schemas
instead of duplicating them.
