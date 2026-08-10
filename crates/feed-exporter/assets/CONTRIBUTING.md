# Contributing

Thank you for helping improve Company News Data.

## Start with the right repository

This repository is a generated publication artifact. Do not manually edit
files under `articles/`, `index/`, `companies/`, `feeds/`, or `indexes`, and do
not hand-edit `HEAD.json` or `index.json`. Those files are projections of the
source database and will be reconciled by the next export.

Crawler, normalization, exporter, schema, and validation implementation changes
belong in
[company-feed-server](https://github.com/Shuozeli/company-feed-server).
Reader changes belong in
[company-news-ui](https://github.com/Shuozeli/company-news-ui).

The exporter also owns `README.md`, `ARCHITECTURE.md`, `CONTENT_RIGHTS.md`,
`LICENSE.md`, `openapi/`, `schemas/`, `scripts/`, and `.github/`. Durable
changes to those paths must be synchronized with the exporter templates before
the next generated snapshot.

## Report data problems

Use the
[data-correction form](https://github.com/datayuacx26/company-news-data/issues/new?template=data-correction.yml)
for:

- an incorrect company name, identity, or category;
- a wrong canonical URL, title, timestamp, or provenance field;
- a duplicate record or company;
- missing, malformed, or boilerplate-heavy extracted content; or
- a source that no longer resolves to the expected publisher.

Include the affected repository path, current generation from `index.json`,
canonical publisher URL, expected value, and public evidence when possible.
Corrections are made in the source system and appear in a later generated
snapshot.

## Report rights concerns

Do not use a general data issue for copyright, licensing, permission,
attribution, trademark, or removal concerns. Use the dedicated
[rights and removal form](https://github.com/datayuacx26/company-news-data/issues/new?template=rights.yml)
and read [`CONTENT_RIGHTS.md`](CONTENT_RIGHTS.md).

Both issue routes are public. Do not include confidential material, credentials,
or unnecessary personal information.

## Report archive bugs

Use the
[archive bug form](https://github.com/datayuacx26/company-news-data/issues/new?template=archive-bug.yml)
for a reproducible manifest, schema, hash, path, or validator failure. Include
the smallest failing read sequence and sanitized validator output.

## Validate a full materialization

The repository is large, so most consumers should use its lazy HTTP indexes
instead of cloning it. Maintainers who already have a full checkout can run:

```bash
python3 scripts/validate_archive.py
```

The validator checks hashes, counts, ordering, identities, bounded indexes, and
article cross-references. Do not propose a generated snapshot that fails this
command.

For security concerns, follow [`SECURITY.md`](SECURITY.md).
