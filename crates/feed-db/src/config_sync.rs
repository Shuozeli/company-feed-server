use feed_core::{CompaniesConfig, ExportTargetsConfig};
use sqlx::{Postgres, Transaction};

use crate::{Database, DatabaseError};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfigSyncSummary {
    pub companies: usize,
    pub export_targets: usize,
}

impl Database {
    pub async fn sync_seed_config(
        &self,
        companies: &CompaniesConfig,
        export_targets: &ExportTargetsConfig,
    ) -> Result<ConfigSyncSummary, DatabaseError> {
        let mut transaction = self.pool().begin().await?;

        for company in &companies.companies {
            let hints = serde_json::to_value(&company.hints)?;
            let aliases = serde_json::to_value(&company.aliases)?;
            let company_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                INSERT INTO companies (
                    company_key,
                    name,
                    aliases,
                    ownership_status,
                    lifecycle_status,
                    homepage_url,
                    investor_relations_url,
                    newsroom_url,
                    blog_url,
                    hints,
                    name_source
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'seed_config')
                ON CONFLICT (company_key) DO UPDATE
                SET
                    name = EXCLUDED.name,
                    aliases = EXCLUDED.aliases,
                    ownership_status = EXCLUDED.ownership_status,
                    lifecycle_status = EXCLUDED.lifecycle_status,
                    name_source = 'seed_config',
                    homepage_url = EXCLUDED.homepage_url,
                    investor_relations_url = EXCLUDED.investor_relations_url,
                    newsroom_url = EXCLUDED.newsroom_url,
                    blog_url = EXCLUDED.blog_url,
                    hints = EXCLUDED.hints,
                    discovery_enabled = true,
                    discovery_not_before = LEAST(
                        companies.discovery_not_before,
                        CURRENT_TIMESTAMP
                    )
                RETURNING id
                "#,
            )
            .bind(&company.company_key)
            .bind(&company.name)
            .bind(aliases)
            .bind(company.ownership_status.as_str())
            .bind(company.lifecycle_status.as_str())
            .bind(company.homepage_url.as_ref().map(|url| url.as_str()))
            .bind(
                company
                    .investor_relations_url
                    .as_ref()
                    .map(|url| url.as_str()),
            )
            .bind(company.newsroom_url.as_ref().map(|url| url.as_str()))
            .bind(company.blog_url.as_ref().map(|url| url.as_str()))
            .bind(hints)
            .fetch_one(&mut *transaction)
            .await?;

            sqlx::query("DELETE FROM company_listings WHERE company_id = $1")
                .bind(company_id)
                .execute(&mut *transaction)
                .await?;
            for listing in &company.listings {
                sqlx::query(
                    r#"
                    INSERT INTO company_listings (
                        company_id, ticker, exchange, is_primary, metadata
                    )
                    VALUES ($1, $2, $3, $4, '{}'::jsonb)
                    "#,
                )
                .bind(company_id)
                .bind(&listing.ticker)
                .bind(listing.exchange.as_deref().unwrap_or_default())
                .bind(listing.is_primary)
                .execute(&mut *transaction)
                .await?;
            }
            sqlx::query(
                r#"
                INSERT INTO company_external_ids (
                    source_name, source_company_id, company_id, metadata
                )
                VALUES ('seed_config', $1, $2, '{}'::jsonb)
                ON CONFLICT (source_name, source_company_id) DO UPDATE
                SET company_id = EXCLUDED.company_id
                "#,
            )
            .bind(&company.company_key)
            .bind(company_id)
            .execute(&mut *transaction)
            .await?;
        }

        for target in &export_targets.targets {
            upsert_export_target(&mut transaction, target).await?;
        }

        transaction.commit().await?;
        Ok(ConfigSyncSummary {
            companies: companies.companies.len(),
            export_targets: export_targets.targets.len(),
        })
    }
}

async fn upsert_export_target(
    transaction: &mut Transaction<'_, Postgres>,
    target: &feed_core::ExportTargetSeed,
) -> Result<(), DatabaseError> {
    let cadence_seconds =
        i32::try_from(target.cadence_seconds).map_err(|_| DatabaseError::NumericRange {
            field: "cadence_seconds",
            value: target.cadence_seconds,
        })?;
    let local_path = target
        .local_path
        .to_str()
        .ok_or_else(|| DatabaseError::NonUtf8Path {
            target_id: target.target_id.clone(),
        })?;

    sqlx::query(
        r#"
        INSERT INTO export_targets (
            target_id,
            repo_url,
            local_path,
            branch,
            format,
            layout,
            cadence_seconds,
            enabled,
            push_enabled,
            metadata
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        ON CONFLICT (target_id) DO UPDATE
        SET
            repo_url = EXCLUDED.repo_url,
            local_path = EXCLUDED.local_path,
            branch = EXCLUDED.branch,
            format = EXCLUDED.format,
            layout = EXCLUDED.layout,
            cadence_seconds = EXCLUDED.cadence_seconds,
            enabled = EXCLUDED.enabled,
            push_enabled = EXCLUDED.push_enabled,
            metadata = EXCLUDED.metadata
        "#,
    )
    .bind(&target.target_id)
    .bind(&target.repo_url)
    .bind(local_path)
    .bind(&target.branch)
    .bind(target.format.as_str())
    .bind(target.layout.as_str())
    .bind(cadence_seconds)
    .bind(target.enabled)
    .bind(target.push_enabled)
    .bind(&target.metadata)
    .execute(&mut **transaction)
    .await?;

    Ok(())
}
