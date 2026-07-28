# Declarative PostgreSQL Schema

[`schema/postgres.sql`](../schema/postgres.sql) is the sole structural database
source of truth. The repository does not carry or execute a sequence of
PostgreSQL migration files.

Blank databases initialize directly from the schema. For an existing
database, the operator generates and reviews a live diff:

```bash
export DATABASE_URL=postgresql://company_feed:company_feed@localhost:55432/company_feed
scripts/schema-plan.sh
```

Apply the generated plan:

```bash
SCHEMA_AUTO_APPROVE=true scripts/schema-apply.sh
```

The scripts pin Stripe `pg-schema-diff` `v1.0.8`. They use an installed binary
when available or `go run` as a development fallback. Production images carry
the pinned binary, and the one-shot Compose `schema` service must complete
before application services start.

Hazardous operations fail closed. Set `SCHEMA_ALLOW_HAZARDS` only after
reviewing the exact plan, for example:

```bash
SCHEMA_AUTO_APPROVE=true \
SCHEMA_ALLOW_HAZARDS=INDEX_BUILD \
scripts/schema-apply.sh
```

Application startup never mutates an existing schema. It verifies the required
tables and returns a drift error directing the operator to the apply script.
This keeps DDL review and deployment separate from API/worker process startup.

For a schema change:

1. edit `schema/postgres.sql`;
2. initialize a disposable blank database and run the integration suite;
3. inspect `scripts/schema-plan.sh` against a representative existing
   database;
4. document any hazards and operational impact;
5. apply the generated plan, then rerun the plan and require zero statements.

Data correction and crawling policy belong in audited application operations,
configuration, or durable jobs—not structural schema history.
