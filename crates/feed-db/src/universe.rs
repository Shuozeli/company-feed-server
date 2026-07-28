use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use feed_universe::{UniverseCompany, UniverseListing, UniverseSource, ValidatedUniverse};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::{Database, DatabaseError};

const IMPORT_CHUNK_SIZE: usize = 500;
const MAX_WAVE_SIZE: u32 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UniverseImportOptions {
    pub activate_new: bool,
    pub discovery_cadence_seconds: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UniverseWaveOptions {
    pub import_run_id: Option<Uuid>,
    pub limit: u32,
    pub start_at: DateTime<Utc>,
    pub spacing: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanyImportAction {
    Inserted,
    Updated,
    Unchanged,
}

impl CompanyImportAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Updated => "updated",
            Self::Unchanged => "unchanged",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompanyImportSummary {
    pub import_run_id: Uuid,
    pub source_name: String,
    pub source_revision: Option<String>,
    pub input_sha256: String,
    pub input_bytes: i64,
    pub input_rows: i32,
    pub inserted_rows: i32,
    pub updated_rows: i32,
    pub unchanged_rows: i32,
    pub activate_new: bool,
    pub discovery_cadence_seconds: i32,
    pub idempotent_replay: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ActivatedCompany {
    pub company_id: Uuid,
    pub company_key: String,
    pub name: String,
    pub discovery_not_before: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompanyWaveSummary {
    pub import_run_id: Option<Uuid>,
    pub requested: u32,
    pub activated: Vec<ActivatedCompany>,
}

#[derive(Debug, FromRow)]
struct ImportRunRow {
    id: Uuid,
    source_name: String,
    source_revision: Option<String>,
    input_sha256: String,
    input_bytes: i64,
    input_rows: i32,
    inserted_rows: i32,
    updated_rows: i32,
    unchanged_rows: i32,
    activate_new: bool,
    discovery_cadence_seconds: i32,
}

impl ImportRunRow {
    fn into_summary(self, idempotent_replay: bool) -> CompanyImportSummary {
        CompanyImportSummary {
            import_run_id: self.id,
            source_name: self.source_name,
            source_revision: self.source_revision,
            input_sha256: self.input_sha256,
            input_bytes: self.input_bytes,
            input_rows: self.input_rows,
            inserted_rows: self.inserted_rows,
            updated_rows: self.updated_rows,
            unchanged_rows: self.unchanged_rows,
            activate_new: self.activate_new,
            discovery_cadence_seconds: self.discovery_cadence_seconds,
            idempotent_replay,
        }
    }
}

#[derive(Debug, FromRow)]
struct ExistingCompanyRow {
    company_key: String,
    name: String,
    name_source: String,
    aliases: Value,
    ownership_status: String,
    lifecycle_status: String,
    homepage_url: Option<String>,
    investor_relations_url: Option<String>,
    newsroom_url: Option<String>,
    blog_url: Option<String>,
    hints: Value,
    metadata: Value,
}

#[derive(Debug, FromRow)]
struct ExternalIdentityRow {
    source_company_id: String,
    company_key: String,
}

#[derive(Debug)]
struct PreparedCompany {
    row_number: i32,
    source_company_id: String,
    company_key: String,
    name: String,
    aliases: Value,
    ownership_status: &'static str,
    lifecycle_status: &'static str,
    listings: Vec<UniverseListing>,
    homepage_url: Option<String>,
    investor_relations_url: Option<String>,
    newsroom_url: Option<String>,
    blog_url: Option<String>,
    hints: Value,
    metadata_patch: Value,
    source_record: Value,
    action: CompanyImportAction,
}

#[derive(Debug, FromRow)]
struct CompanyIdentityRow {
    id: Uuid,
    company_key: String,
    name: String,
}

impl Database {
    pub async fn import_company_universe(
        &self,
        universe: &ValidatedUniverse,
        options: UniverseImportOptions,
    ) -> Result<CompanyImportSummary, DatabaseError> {
        if options.discovery_cadence_seconds <= 0 {
            return Err(DatabaseError::Invariant(
                "company universe discovery cadence must be positive".to_owned(),
            ));
        }
        let input_bytes =
            i64::try_from(universe.input_bytes).map_err(|_| DatabaseError::NumericRange {
                field: "company_universe_input_bytes",
                value: u64::try_from(universe.input_bytes).unwrap_or(u64::MAX),
            })?;
        let input_rows = i32::try_from(universe.document.companies.len()).map_err(|_| {
            DatabaseError::NumericRange {
                field: "company_universe_input_rows",
                value: u64::try_from(universe.document.companies.len()).unwrap_or(u64::MAX),
            }
        })?;

        let mut transaction = self.pool().begin().await?;
        let lock_key = format!(
            "{}:{}",
            universe.document.source.name, universe.input_sha256
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&lock_key)
            .execute(&mut *transaction)
            .await?;

        if let Some(existing) = load_import_run(
            &mut transaction,
            &universe.document.source.name,
            &universe.input_sha256,
        )
        .await?
        {
            transaction.commit().await?;
            return Ok(existing.into_summary(true));
        }

        let company_keys = universe
            .document
            .companies
            .iter()
            .map(|company| company.company_key.clone())
            .collect::<Vec<_>>();
        let source_ids = universe
            .document
            .companies
            .iter()
            .map(|company| company.source_company_id.clone())
            .collect::<Vec<_>>();

        let external_identities = sqlx::query_as::<_, ExternalIdentityRow>(
            r#"
            SELECT external.source_company_id, company.company_key
            FROM company_external_ids AS external
            INNER JOIN companies AS company ON company.id = external.company_id
            WHERE
                external.source_name = $1
                AND external.source_company_id = ANY($2)
            "#,
        )
        .bind(&universe.document.source.name)
        .bind(&source_ids)
        .fetch_all(&mut *transaction)
        .await?
        .into_iter()
        .map(|identity| (identity.source_company_id, identity.company_key))
        .collect::<HashMap<_, _>>();
        for company in &universe.document.companies {
            if let Some(existing_key) = external_identities.get(&company.source_company_id)
                && existing_key != &company.company_key
            {
                return Err(DatabaseError::Invariant(format!(
                    "source company {} is already mapped to company key {}; refusing remap to {}",
                    company.source_company_id, existing_key, company.company_key
                )));
            }
        }

        let existing_rows = sqlx::query_as::<_, ExistingCompanyRow>(
            r#"
            SELECT
                company_key,
                name,
                name_source,
                aliases,
                ownership_status,
                lifecycle_status,
                homepage_url,
                investor_relations_url,
                newsroom_url,
                blog_url,
                hints,
                metadata
            FROM companies
            WHERE company_key = ANY($1)
            "#,
        )
        .bind(&company_keys)
        .fetch_all(&mut *transaction)
        .await?;
        let existing = existing_rows
            .into_iter()
            .map(|company| (company.company_key.clone(), company))
            .collect::<HashMap<_, _>>();

        let prepared = universe
            .document
            .companies
            .iter()
            .enumerate()
            .map(|(index, company)| {
                prepare_company(
                    i32::try_from(index + 1).expect("validated universe fits in i32"),
                    &universe.document.source,
                    company,
                    existing.get(&company.company_key),
                )
            })
            .collect::<Result<Vec<_>, DatabaseError>>()?;

        for chunk in prepared.chunks(IMPORT_CHUNK_SIZE) {
            upsert_company_chunk(
                &mut transaction,
                chunk,
                options.activate_new,
                options.discovery_cadence_seconds,
            )
            .await?;
        }

        let identities = sqlx::query_as::<_, CompanyIdentityRow>(
            "SELECT id, company_key, name FROM companies WHERE company_key = ANY($1)",
        )
        .bind(&company_keys)
        .fetch_all(&mut *transaction)
        .await?
        .into_iter()
        .map(|company| (company.company_key.clone(), company))
        .collect::<HashMap<_, _>>();

        for chunk in prepared.chunks(IMPORT_CHUNK_SIZE) {
            sync_external_ids(
                &mut transaction,
                &universe.document.source.name,
                chunk,
                &identities,
            )
            .await?;
            sync_listings(&mut transaction, chunk, &identities).await?;
        }

        let inserted_rows = count_actions(&prepared, CompanyImportAction::Inserted)?;
        let updated_rows = count_actions(&prepared, CompanyImportAction::Updated)?;
        let unchanged_rows = count_actions(&prepared, CompanyImportAction::Unchanged)?;
        let import_run_id: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO company_import_runs (
                source_name,
                source_revision,
                input_sha256,
                input_bytes,
                input_rows,
                inserted_rows,
                updated_rows,
                unchanged_rows,
                activate_new,
                discovery_cadence_seconds,
                metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id
            "#,
        )
        .bind(&universe.document.source.name)
        .bind(&universe.document.source.revision)
        .bind(&universe.input_sha256)
        .bind(input_bytes)
        .bind(input_rows)
        .bind(inserted_rows)
        .bind(updated_rows)
        .bind(unchanged_rows)
        .bind(options.activate_new)
        .bind(options.discovery_cadence_seconds)
        .bind(json!({
            "schema_version": universe.document.schema_version,
            "generated_at": universe.document.source.generated_at,
            "source_metadata": universe.document.source.metadata,
        }))
        .fetch_one(&mut *transaction)
        .await?;

        for chunk in prepared.chunks(IMPORT_CHUNK_SIZE) {
            insert_audit_chunk(&mut transaction, import_run_id, chunk, &identities).await?;
        }
        transaction.commit().await?;

        Ok(CompanyImportSummary {
            import_run_id,
            source_name: universe.document.source.name.clone(),
            source_revision: universe.document.source.revision.clone(),
            input_sha256: universe.input_sha256.clone(),
            input_bytes,
            input_rows,
            inserted_rows,
            updated_rows,
            unchanged_rows,
            activate_new: options.activate_new,
            discovery_cadence_seconds: options.discovery_cadence_seconds,
            idempotent_replay: false,
        })
    }

    pub async fn activate_company_wave(
        &self,
        options: UniverseWaveOptions,
    ) -> Result<CompanyWaveSummary, DatabaseError> {
        if options.limit == 0 || options.limit > MAX_WAVE_SIZE {
            return Err(DatabaseError::Invariant(format!(
                "company activation wave must contain 1 to {MAX_WAVE_SIZE} companies"
            )));
        }
        let spacing_seconds = i64::try_from(options.spacing.as_secs())
            .map_err(|_| DatabaseError::InvalidDuration(options.spacing))?;
        let mut transaction = self.pool().begin().await?;
        let rows = if let Some(import_run_id) = options.import_run_id {
            sqlx::query_as::<_, CompanyIdentityRow>(
                r#"
                SELECT company.id, company.company_key, company.name
                FROM companies AS company
                INNER JOIN company_import_rows AS import_row
                    ON import_row.company_id = company.id
                WHERE
                    import_row.import_run_id = $1
                    AND NOT company.discovery_enabled
                ORDER BY
                    NULLIF(company.metadata #>> '{universe,market_cap}', '')::bigint
                        DESC NULLS LAST,
                    company.company_key
                FOR UPDATE OF company SKIP LOCKED
                LIMIT $2
                "#,
            )
            .bind(import_run_id)
            .bind(i64::from(options.limit))
            .fetch_all(&mut *transaction)
            .await?
        } else {
            sqlx::query_as::<_, CompanyIdentityRow>(
                r#"
                SELECT company.id, company.company_key, company.name
                FROM companies AS company
                WHERE NOT company.discovery_enabled
                ORDER BY
                    NULLIF(company.metadata #>> '{universe,market_cap}', '')::bigint
                        DESC NULLS LAST,
                    company.company_key
                FOR UPDATE OF company SKIP LOCKED
                LIMIT $1
                "#,
            )
            .bind(i64::from(options.limit))
            .fetch_all(&mut *transaction)
            .await?
        };

        let activated = rows
            .into_iter()
            .enumerate()
            .map(|(index, company)| {
                let offset = spacing_seconds
                    .checked_mul(i64::try_from(index).expect("wave size fits in i64"))
                    .ok_or_else(|| {
                        DatabaseError::Invariant(
                            "company activation schedule exceeds supported duration".to_owned(),
                        )
                    })?;
                let discovery_not_before = options
                    .start_at
                    .checked_add_signed(chrono::Duration::seconds(offset))
                    .ok_or_else(|| {
                        DatabaseError::Invariant(
                            "company activation time exceeds supported date range".to_owned(),
                        )
                    })?;
                Ok(ActivatedCompany {
                    company_id: company.id,
                    company_key: company.company_key,
                    name: company.name,
                    discovery_not_before,
                })
            })
            .collect::<Result<Vec<_>, DatabaseError>>()?;

        for chunk in activated.chunks(IMPORT_CHUNK_SIZE) {
            let mut builder: QueryBuilder<'_, Postgres> = QueryBuilder::new(
                "UPDATE companies AS company SET discovery_enabled = true, \
                 discovery_not_before = activation.discovery_not_before \
                 FROM (",
            );
            builder.push_values(chunk, |mut row, company| {
                row.push_bind(company.company_id)
                    .push_bind(company.discovery_not_before);
            });
            builder.push(
                ") AS activation(company_id, discovery_not_before) \
                 WHERE company.id = activation.company_id",
            );
            builder.build().execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(CompanyWaveSummary {
            import_run_id: options.import_run_id,
            requested: options.limit,
            activated,
        })
    }
}

async fn load_import_run(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    source_name: &str,
    input_sha256: &str,
) -> Result<Option<ImportRunRow>, DatabaseError> {
    Ok(sqlx::query_as::<_, ImportRunRow>(
        r#"
        SELECT
            id,
            source_name,
            source_revision,
            input_sha256,
            input_bytes,
            input_rows,
            inserted_rows,
            updated_rows,
            unchanged_rows,
            activate_new,
            discovery_cadence_seconds
        FROM company_import_runs
        WHERE source_name = $1 AND input_sha256 = $2
        "#,
    )
    .bind(source_name)
    .bind(input_sha256)
    .fetch_optional(&mut **transaction)
    .await?)
}

fn prepare_company(
    row_number: i32,
    source: &UniverseSource,
    company: &UniverseCompany,
    existing: Option<&ExistingCompanyRow>,
) -> Result<PreparedCompany, DatabaseError> {
    let aliases = serde_json::to_value(&company.aliases)?;
    let hints = serde_json::to_value(&company.hints)?;
    let source_record = serde_json::to_value(company)?;
    let metadata_patch = json!({
        "universe": {
            "source": source.name,
            "source_revision": source.revision,
            "source_generated_at": source.generated_at,
            "market_cap": company.market_cap,
            "country": company.country,
            "ipo_year": company.ipo_year,
            "sector": company.sector,
            "industry": company.industry,
            "identifiers": company.identifiers,
            "source_metadata": company.metadata,
        }
    });
    let homepage_url = company
        .homepage_url
        .as_ref()
        .map(|url| url.as_str().to_owned());
    let investor_relations_url = company
        .investor_relations_url
        .as_ref()
        .map(|url| url.as_str().to_owned());
    let newsroom_url = company
        .newsroom_url
        .as_ref()
        .map(|url| url.as_str().to_owned());
    let blog_url = company.blog_url.as_ref().map(|url| url.as_str().to_owned());
    let action = match existing {
        None => CompanyImportAction::Inserted,
        Some(existing)
            if existing_matches(
                existing,
                company,
                &aliases,
                &homepage_url,
                &investor_relations_url,
                &newsroom_url,
                &blog_url,
                &hints,
                &metadata_patch,
            ) =>
        {
            CompanyImportAction::Unchanged
        }
        Some(_) => CompanyImportAction::Updated,
    };
    Ok(PreparedCompany {
        row_number,
        source_company_id: company.source_company_id.clone(),
        company_key: company.company_key.clone(),
        name: company.name.clone(),
        aliases,
        ownership_status: company.ownership_status.as_str(),
        lifecycle_status: company.lifecycle_status.as_str(),
        listings: company.listings.clone(),
        homepage_url,
        investor_relations_url,
        newsroom_url,
        blog_url,
        hints,
        metadata_patch,
        source_record,
        action,
    })
}

#[allow(clippy::too_many_arguments)]
fn existing_matches(
    existing: &ExistingCompanyRow,
    imported: &UniverseCompany,
    aliases: &Value,
    homepage_url: &Option<String>,
    investor_relations_url: &Option<String>,
    newsroom_url: &Option<String>,
    blog_url: &Option<String>,
    hints: &Value,
    metadata_patch: &Value,
) -> bool {
    let desired_name = if existing.name_source == "universe" {
        imported.name.as_str()
    } else {
        existing.name.as_str()
    };
    let desired_aliases = if existing.aliases == json!([]) {
        aliases
    } else {
        &existing.aliases
    };
    let desired_ownership = if existing.ownership_status == "unknown" {
        imported.ownership_status.as_str()
    } else {
        existing.ownership_status.as_str()
    };
    let desired_lifecycle = if existing.lifecycle_status == "unknown" {
        imported.lifecycle_status.as_str()
    } else {
        existing.lifecycle_status.as_str()
    };
    desired_name == existing.name
        && desired_aliases == &existing.aliases
        && desired_ownership == existing.ownership_status
        && desired_lifecycle == existing.lifecycle_status
        && existing.homepage_url.as_ref().or(homepage_url.as_ref())
            == existing.homepage_url.as_ref()
        && existing
            .investor_relations_url
            .as_ref()
            .or(investor_relations_url.as_ref())
            == existing.investor_relations_url.as_ref()
        && existing.newsroom_url.as_ref().or(newsroom_url.as_ref())
            == existing.newsroom_url.as_ref()
        && existing.blog_url.as_ref().or(blog_url.as_ref()) == existing.blog_url.as_ref()
        && (existing.hints != json!([]) || &existing.hints == hints)
        && value_contains(&existing.metadata, metadata_patch)
}

fn value_contains(existing: &Value, patch: &Value) -> bool {
    let (Some(existing), Some(patch)) = (existing.as_object(), patch.as_object()) else {
        return false;
    };
    patch
        .iter()
        .all(|(key, value)| existing.get(key) == Some(value))
}

async fn upsert_company_chunk(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    companies: &[PreparedCompany],
    activate_new: bool,
    discovery_cadence_seconds: i32,
) -> Result<(), DatabaseError> {
    let mut builder: QueryBuilder<'_, Postgres> = QueryBuilder::new(
        "INSERT INTO companies (\
         company_key, name, name_source, aliases, ownership_status, lifecycle_status, \
         homepage_url, investor_relations_url, newsroom_url, blog_url, hints, \
         discovery_enabled, discovery_not_before, discovery_cadence_seconds, metadata) ",
    );
    builder.push_values(companies, |mut row, company| {
        row.push_bind(&company.company_key)
            .push_bind(&company.name)
            .push_bind("universe")
            .push_bind(&company.aliases)
            .push_bind(company.ownership_status)
            .push_bind(company.lifecycle_status)
            .push_bind(&company.homepage_url)
            .push_bind(&company.investor_relations_url)
            .push_bind(&company.newsroom_url)
            .push_bind(&company.blog_url)
            .push_bind(&company.hints)
            .push_bind(activate_new)
            .push_bind(Utc::now())
            .push_bind(discovery_cadence_seconds)
            .push_bind(&company.metadata_patch);
    });
    builder.push(
        " ON CONFLICT (company_key) DO UPDATE SET \
          name = CASE WHEN companies.name_source = 'universe' \
                      THEN EXCLUDED.name ELSE companies.name END, \
          aliases = CASE WHEN companies.aliases = '[]'::jsonb \
                         THEN EXCLUDED.aliases ELSE companies.aliases END, \
          ownership_status = CASE WHEN companies.ownership_status = 'unknown' \
                                  THEN EXCLUDED.ownership_status \
                                  ELSE companies.ownership_status END, \
          lifecycle_status = CASE WHEN companies.lifecycle_status = 'unknown' \
                                  THEN EXCLUDED.lifecycle_status \
                                  ELSE companies.lifecycle_status END, \
          homepage_url = COALESCE(companies.homepage_url, EXCLUDED.homepage_url), \
          investor_relations_url = COALESCE(\
              companies.investor_relations_url, EXCLUDED.investor_relations_url), \
          newsroom_url = COALESCE(companies.newsroom_url, EXCLUDED.newsroom_url), \
          blog_url = COALESCE(companies.blog_url, EXCLUDED.blog_url), \
          hints = CASE WHEN companies.hints = '[]'::jsonb \
                       THEN EXCLUDED.hints ELSE companies.hints END, \
          metadata = companies.metadata || EXCLUDED.metadata \
          WHERE ROW(\
              companies.name, companies.aliases, companies.ownership_status, \
              companies.lifecycle_status, companies.homepage_url, \
              companies.investor_relations_url, companies.newsroom_url, \
              companies.blog_url, companies.hints, companies.metadata\
          ) IS DISTINCT FROM ROW(\
              CASE WHEN companies.name_source = 'universe' \
                   THEN EXCLUDED.name ELSE companies.name END, \
              CASE WHEN companies.aliases = '[]'::jsonb \
                   THEN EXCLUDED.aliases ELSE companies.aliases END, \
              CASE WHEN companies.ownership_status = 'unknown' \
                   THEN EXCLUDED.ownership_status ELSE companies.ownership_status END, \
              CASE WHEN companies.lifecycle_status = 'unknown' \
                   THEN EXCLUDED.lifecycle_status ELSE companies.lifecycle_status END, \
              COALESCE(companies.homepage_url, EXCLUDED.homepage_url), \
              COALESCE(companies.investor_relations_url, EXCLUDED.investor_relations_url), \
              COALESCE(companies.newsroom_url, EXCLUDED.newsroom_url), \
              COALESCE(companies.blog_url, EXCLUDED.blog_url), \
              CASE WHEN companies.hints = '[]'::jsonb \
                   THEN EXCLUDED.hints ELSE companies.hints END, \
              companies.metadata || EXCLUDED.metadata\
          )",
    );
    builder.build().execute(&mut **transaction).await?;
    Ok(())
}

async fn sync_external_ids(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    source_name: &str,
    companies: &[PreparedCompany],
    identities: &HashMap<String, CompanyIdentityRow>,
) -> Result<(), DatabaseError> {
    let rows = companies
        .iter()
        .map(|company| {
            identities
                .get(&company.company_key)
                .map(|identity| (company, identity.id))
                .ok_or_else(|| missing_identity(company))
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;
    let mut builder: QueryBuilder<'_, Postgres> = QueryBuilder::new(
        "INSERT INTO company_external_ids (\
         source_name, source_company_id, company_id, metadata) ",
    );
    builder.push_values(rows, |mut row, (company, company_id)| {
        row.push_bind(source_name)
            .push_bind(&company.source_company_id)
            .push_bind(company_id)
            .push_bind(json!({ "company_key": company.company_key }));
    });
    builder.push(
        " ON CONFLICT (source_name, source_company_id) DO UPDATE SET \
          metadata = company_external_ids.metadata || EXCLUDED.metadata",
    );
    builder.build().execute(&mut **transaction).await?;
    Ok(())
}

async fn sync_listings(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    companies: &[PreparedCompany],
    identities: &HashMap<String, CompanyIdentityRow>,
) -> Result<(), DatabaseError> {
    let companies_with_primary = companies
        .iter()
        .filter(|company| company.listings.iter().any(|listing| listing.is_primary))
        .map(|company| {
            identities
                .get(&company.company_key)
                .map(|identity| identity.id)
                .ok_or_else(|| missing_identity(company))
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;
    if !companies_with_primary.is_empty() {
        sqlx::query("UPDATE company_listings SET is_primary = false WHERE company_id = ANY($1)")
            .bind(&companies_with_primary)
            .execute(&mut **transaction)
            .await?;
    }

    let listings = companies
        .iter()
        .flat_map(|company| {
            company
                .listings
                .iter()
                .map(move |listing| (company, listing))
        })
        .map(|(company, listing)| {
            identities
                .get(&company.company_key)
                .map(|identity| (identity.id, listing))
                .ok_or_else(|| missing_identity(company))
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;
    if listings.is_empty() {
        return Ok(());
    }
    let mut builder: QueryBuilder<'_, Postgres> = QueryBuilder::new(
        "INSERT INTO company_listings (\
         company_id, ticker, exchange, is_primary, metadata) ",
    );
    builder.push_values(listings, |mut row, (company_id, listing)| {
        row.push_bind(company_id)
            .push_bind(&listing.ticker)
            .push_bind(listing.exchange.as_deref().unwrap_or_default())
            .push_bind(listing.is_primary)
            .push_bind(&listing.metadata);
    });
    builder.push(
        " ON CONFLICT (company_id, ticker, exchange) DO UPDATE SET \
          is_primary = EXCLUDED.is_primary, \
          metadata = company_listings.metadata || EXCLUDED.metadata",
    );
    builder.build().execute(&mut **transaction).await?;
    Ok(())
}

async fn insert_audit_chunk(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    import_run_id: Uuid,
    companies: &[PreparedCompany],
    identities: &HashMap<String, CompanyIdentityRow>,
) -> Result<(), DatabaseError> {
    let audit_rows = companies
        .iter()
        .map(|company| {
            identities
                .get(&company.company_key)
                .map(|identity| (company, identity))
                .ok_or_else(|| missing_identity(company))
        })
        .collect::<Result<Vec<_>, DatabaseError>>()?;
    let mut builder: QueryBuilder<'_, Postgres> = QueryBuilder::new(
        "INSERT INTO company_import_rows (\
         import_run_id, row_number, source_company_id, company_key, company_name, \
         action, company_id, source_record) ",
    );
    builder.push_values(audit_rows, |mut row, (company, identity)| {
        row.push_bind(import_run_id)
            .push_bind(company.row_number)
            .push_bind(&company.source_company_id)
            .push_bind(&company.company_key)
            .push_bind(&identity.name)
            .push_bind(company.action.as_str())
            .push_bind(identity.id)
            .push_bind(&company.source_record);
    });
    builder.build().execute(&mut **transaction).await?;
    Ok(())
}

fn missing_identity(company: &PreparedCompany) -> DatabaseError {
    DatabaseError::Invariant(format!(
        "imported company {} has no persisted identity",
        company.company_key
    ))
}

fn count_actions(
    companies: &[PreparedCompany],
    action: CompanyImportAction,
) -> Result<i32, DatabaseError> {
    i32::try_from(
        companies
            .iter()
            .filter(|company| company.action == action)
            .count(),
    )
    .map_err(|_| DatabaseError::Invariant("company import count exceeds i32".to_owned()))
}
