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

Public Git export requires both an enabled export target and
`public_export_allowed=true` on the source. The sample configuration keeps
push and public export disabled.
