# Data and Content Policy

The MIT license covers the source code and project-authored documentation in
this repository. It does not grant rights to third-party articles, logos,
trademarks, provider output, or datasets.

## Included

- three small public-company configuration examples;
- schemas and import contracts;
- deterministic crawler, normalization, and export code; and
- test fixtures created for this project.

## Not Included

- the maintainers' live PostgreSQL database;
- crawled article bodies or publisher archives;
- the production company-universe input dataset;
- private provider prompts, credentials, responses, or implementation; and
- operational logs and dated deployment handoffs.

Company names and URLs may be factual, while company and product names remain
the trademarks of their respective owners.

## Operator Responsibility

Operators choose their data sources and export policy. They are responsible
for applicable website terms, robots directives, copyright, database rights,
privacy obligations, retention, and redistribution permissions.

An enabled export target uses one of two record-selection scopes:

- By default, a source must have `public_export_allowed=true`.
- A target may explicitly set
  `metadata.publication_scope=approved_public` to select non-private records
  from every approved, currently valid source.

The checked-in [`configs/export_targets.yaml`](../configs/export_targets.yaml)
sample uses the broader `approved_public` selection scope. It keeps
`push_enabled=false`, so the sample materializes locally but does not push.

Both settings are operational publication controls only. A record being
publicly accessible, approved as a company source, selected by an export
target, or already present in an archive does not establish ownership,
permission, a license, or a legal basis for redistribution. Operators must
perform and document their own rights review before publishing generated
material.
