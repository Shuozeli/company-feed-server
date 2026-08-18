use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use feed_core::{CandidateStatus, Company, JobSpec, JobType, SourceKind, SourceStatus};
use feed_db::{Database, FeedItemQualityQuarantine, UniverseImportOptions, UniverseWaveOptions};
use feed_jobs::{company_names_share_recipe_issuer, feed_item_has_cross_company_scope};
use feed_universe::ValidatedUniverse;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    name = "feed-admin",
    version,
    about = "Operate Company Feed Server through its durable database contracts"
)]
struct Cli {
    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,
    #[arg(long, env = "DATABASE_MAX_CONNECTIONS", default_value_t = 5)]
    database_max_connections: u32,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Queue discovery through the production job runner.
    Discover {
        /// Queue one company by key or exact name; omit to queue every activated company that is due.
        #[arg(long)]
        company: Option<String>,
    },
    /// Import and activate the broad company universe.
    Companies {
        #[command(subcommand)]
        command: CompanyCommand,
    },
    /// Review and decide discovered source candidates.
    Candidates {
        #[command(subcommand)]
        command: CandidateCommand,
    },
    /// Queue crawling for one approved source or every approved RSS/Atom source.
    /// An explicit HTML/browser source is accepted only when it has an active recipe.
    Crawl {
        #[arg(long)]
        source_id: Option<Uuid>,
    },
    /// Operate the independent article-page content hydration pipeline.
    ContentCrawl {
        #[command(subcommand)]
        command: ContentCrawlCommand,
    },
    /// Manually build and validate durable news/blog crawl recipes.
    #[command(name = "news-import", visible_alias = "news-extract")]
    NewsImport {
        /// Company key or exact company name. Tickers are not accepted.
        #[arg(
            long,
            conflicts_with_all = ["all", "retry_transient_after"],
            required_unless_present_any = ["all", "retry_transient_after"]
        )]
        company: Option<String>,
        /// Queue every active company that is missing a healthy active recipe.
        #[arg(long, conflicts_with_all = ["company", "retry_transient_after"])]
        all: bool,
        /// Retry companies whose latest build since this RFC3339 time had only transient fetch failures.
        #[arg(
            long,
            value_name = "RFC3339",
            conflicts_with_all = ["company", "all"]
        )]
        retry_transient_after: Option<DateTime<Utc>>,
        /// Bound an all-company campaign without changing its resumable selection rule.
        #[arg(long, default_value_t = 10_000)]
        limit: u32,
        /// Space all-company jobs so multiple workers do not burst the private adapter.
        #[arg(long, default_value_t = 1)]
        spacing_seconds: u64,
        #[arg(long, default_value_t = 31)]
        lookback_days: u64,
        #[arg(long, default_value_t = 20)]
        max_articles: usize,
        /// Permit an operator-directed expansion even when an RSS/Atom source is already approved.
        #[arg(long)]
        include_covered: bool,
    },
    /// Review, then approve, rebuilds of stale / rebuild-required recipes.
    ///
    /// Without --approve this only lists the rebuild candidate queue (dry run).
    /// With --approve it enqueues bounded, spaced ExtractCompanyNews rebuild jobs
    /// for the top candidates. This is the human-approval gate of the auto-rebuild
    /// loop (see docs/design/2026-08-18-recipe-rebuild-pipeline.md).
    #[command(name = "recipe-rebuild")]
    RecipeRebuild {
        /// Approve and enqueue rebuild jobs for the listed candidates.
        #[arg(long)]
        approve: bool,
        /// Number of candidates to list, or to approve and enqueue.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Space enqueued rebuild jobs so workers do not burst the private adapter.
        #[arg(long, default_value_t = 2)]
        spacing_seconds: u64,
        #[arg(long, default_value_t = 31)]
        lookback_days: u64,
        #[arg(long, default_value_t = 20)]
        max_articles: usize,
    },
    /// Audit public URL identities shared by distinct issuers and quarantine
    /// associations without company-specific title, path, or host evidence.
    NewsOwnershipAudit {
        /// Apply reversible quality quarantine to unscoped associations.
        #[arg(long)]
        apply: bool,
        /// Exit nonzero when unscoped distinct-issuer associations remain.
        #[arg(long)]
        fail_on_unscoped: bool,
    },
    /// Queue one export target or every enabled target.
    Export {
        #[arg(long)]
        target: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum CandidateCommand {
    /// List discovery candidates as JSON.
    List {
        #[arg(long)]
        company: Option<String>,
        #[arg(long, default_value = "new")]
        status: String,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Queue validation for one candidate or a bounded unvalidated wave.
    Validate {
        #[arg(long)]
        candidate_id: Option<Uuid>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// Include companies that already have an approved source, enabling product/blog expansion.
        #[arg(long)]
        include_covered: bool,
    },
    /// Re-run machine-rejected feed candidates under the current company-scope policy.
    ///
    /// Operator rejections are never reopened.
    ReconsiderAutomaticScope {
        #[arg(long, default_value_t = 500)]
        limit: u32,
    },
    /// Promote a reviewed candidate into an approved source.
    Accept {
        candidate_id: Uuid,
        #[arg(long)]
        source_id: Option<String>,
        #[arg(long, default_value_t = 3600)]
        freshness_seconds: i32,
        #[arg(long)]
        public_export: bool,
    },
    /// Reject a candidate so discovery will not make it crawlable.
    Reject { candidate_id: Uuid },
}

#[derive(Debug, Subcommand)]
enum ContentCrawlCommand {
    /// Queue the next due batch for the content worker.
    Enqueue {
        #[arg(long, default_value_t = 2_592_000)]
        refresh_seconds: i64,
    },
    /// Print durable crawl and article-body coverage as JSON.
    Status {
        #[arg(long, default_value_t = 200)]
        min_content_chars: i32,
    },
}

#[derive(Debug, Subcommand)]
enum CompanyCommand {
    /// Validate and atomically import a company-universe.v2 JSON document.
    Import {
        #[arg(long)]
        file: PathBuf,
        /// Validate the complete document without writing to Postgres.
        #[arg(long)]
        validate_only: bool,
        /// Activate newly inserted companies immediately instead of staging them.
        #[arg(long)]
        activate_new: bool,
        #[arg(long, default_value_t = 604_800)]
        discovery_cadence_seconds: i32,
    },
    /// Activate the next market-cap-ordered wave of staged companies.
    Activate {
        /// Restrict activation to rows from one import run.
        #[arg(long)]
        import_run_id: Option<Uuid>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        /// RFC3339 first-discovery release time; defaults to now.
        #[arg(long)]
        start_at: Option<DateTime<Utc>>,
        /// Delay between consecutive companies in the wave.
        #[arg(long, default_value_t = 60)]
        spacing_seconds: u64,
    },
}

#[derive(Debug, Clone, Copy)]
struct NewsImportOptions<'a> {
    selector: Option<&'a str>,
    all: bool,
    retry_transient_after: Option<DateTime<Utc>>,
    limit: u32,
    spacing_seconds: u64,
    lookback_days: u64,
    max_articles: usize,
    include_covered: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.database_max_connections == 0 {
        bail!("database-max-connections must be positive");
    }
    if let Command::Companies {
        command:
            CompanyCommand::Import {
                file,
                validate_only: true,
                ..
            },
    } = &cli.command
    {
        let universe = ValidatedUniverse::load(file)
            .with_context(|| format!("validate company universe {}", file.display()))?;
        print_universe_validation(&universe)?;
        return Ok(());
    }

    let database_url = cli
        .database_url
        .as_deref()
        .context("--database-url or DATABASE_URL is required for this command")?;
    let database = Database::connect(database_url, cli.database_max_connections)
        .await
        .context("connect to Postgres")?;
    database.ensure_schema().await.context("ensure schema")?;

    match cli.command {
        Command::Discover { company } => queue_discovery(&database, company.as_deref()).await?,
        Command::Companies { command } => handle_companies(&database, command).await?,
        Command::Candidates { command } => handle_candidates(&database, command).await?,
        Command::Crawl { source_id } => queue_crawl(&database, source_id).await?,
        Command::ContentCrawl { command } => handle_content_crawl(&database, command).await?,
        Command::NewsImport {
            company,
            all,
            retry_transient_after,
            limit,
            spacing_seconds,
            lookback_days,
            max_articles,
            include_covered,
        } => {
            queue_news_extraction(
                &database,
                NewsImportOptions {
                    selector: company.as_deref(),
                    all,
                    retry_transient_after,
                    limit,
                    spacing_seconds,
                    lookback_days,
                    max_articles,
                    include_covered,
                },
            )
            .await?
        }
        Command::RecipeRebuild {
            approve,
            limit,
            spacing_seconds,
            lookback_days,
            max_articles,
        } => {
            recipe_rebuild(
                &database,
                approve,
                limit,
                spacing_seconds,
                lookback_days,
                max_articles,
            )
            .await?
        }
        Command::NewsOwnershipAudit {
            apply,
            fail_on_unscoped,
        } => audit_news_item_ownership(&database, apply, fail_on_unscoped).await?,
        Command::Export { target } => queue_export(&database, target.as_deref()).await?,
    }

    database.close().await;
    Ok(())
}

async fn handle_content_crawl(database: &Database, command: ContentCrawlCommand) -> Result<()> {
    match command {
        ContentCrawlCommand::Enqueue { refresh_seconds } => {
            if refresh_seconds <= 0 {
                bail!("refresh-seconds must be positive");
            }
            let now = Utc::now();
            let scheduled = database
                .enqueue_due_content_crawl_job(
                    now,
                    now - chrono::Duration::seconds(refresh_seconds),
                    1,
                )
                .await
                .context("queue article content crawl")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "scheduled_jobs": scheduled,
                    "job_type": JobType::CrawlContent,
                    "refresh_seconds": refresh_seconds,
                }))?
            );
        }
        ContentCrawlCommand::Status { min_content_chars } => {
            if min_content_chars <= 0 {
                bail!("min-content-chars must be positive");
            }
            let coverage = database
                .content_crawl_coverage(min_content_chars)
                .await
                .context("load article content crawl coverage")?;
            println!("{}", serde_json::to_string_pretty(&coverage)?);
        }
    }
    Ok(())
}

struct NewsOwnershipAuditSnapshot {
    collision_item_count: usize,
    collision_candidate_url_count: usize,
    same_issuer_item_count: usize,
    distinct_issuer_item_count: usize,
    unscoped_item_count: usize,
    quarantines: Vec<FeedItemQualityQuarantine>,
    samples: Vec<serde_json::Value>,
}

impl NewsOwnershipAuditSnapshot {
    fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "policy": "cross-company-item-scope.v1",
            "collision_item_count": self.collision_item_count,
            "collision_candidate_url_count": self.collision_candidate_url_count,
            "same_issuer_item_count": self.same_issuer_item_count,
            "distinct_issuer_item_count": self.distinct_issuer_item_count,
            "unscoped_item_count": self.unscoped_item_count,
            "samples": self.samples,
        })
    }
}

async fn collect_news_item_ownership_audit(
    database: &Database,
) -> Result<NewsOwnershipAuditSnapshot> {
    let collision_item_ids = database
        .list_public_cross_company_feed_item_ids()
        .await
        .context("list public cross-company feed items")?;
    let mut items = Vec::with_capacity(collision_item_ids.len());
    for item_id in collision_item_ids {
        if let Some(item) = database
            .get_feed_item(item_id)
            .await
            .with_context(|| format!("load colliding feed item {item_id}"))?
        {
            items.push(item);
        }
    }
    let candidate_urls = items
        .iter()
        .map(|item| item.canonical_url.as_str().to_owned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let claims = database
        .list_public_feed_item_company_claims(&candidate_urls)
        .await
        .context("list public item company claims")?;
    let company_ids = items
        .iter()
        .map(|item| item.company_id)
        .chain(claims.iter().map(|claim| claim.company_id))
        .collect::<HashSet<_>>();
    let mut companies = BTreeMap::<Uuid, Company>::new();
    for company_id in company_ids {
        let company = database
            .get_company(company_id)
            .await
            .with_context(|| format!("load company {company_id} for ownership audit"))?
            .with_context(|| format!("company {company_id} is missing"))?;
        companies.insert(company_id, company);
    }

    let mut same_issuer_item_count = 0_usize;
    let mut distinct_issuer_item_count = 0_usize;
    let mut quarantines = Vec::new();
    let mut samples = Vec::new();
    for item in &items {
        let company = companies
            .get(&item.company_id)
            .with_context(|| format!("company {} was not loaded", item.company_id))?;
        let mut seen = HashSet::new();
        let mut competing_companies = claims
            .iter()
            .filter(|claim| claim.candidate_url == item.canonical_url.as_str())
            .filter(|claim| claim.company_id != item.company_id)
            .filter(|claim| !company_names_share_recipe_issuer(&company.name, &claim.company_name))
            .filter(|claim| seen.insert(claim.company_id))
            .filter_map(|claim| companies.get(&claim.company_id).cloned())
            .collect::<Vec<_>>();
        competing_companies.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        if competing_companies.is_empty() {
            same_issuer_item_count += 1;
            continue;
        }
        distinct_issuer_item_count += 1;
        if feed_item_has_cross_company_scope(company, item, &competing_companies) {
            continue;
        }

        let competing_company_names = competing_companies
            .iter()
            .map(|company| company.name.clone())
            .collect::<Vec<_>>();
        if samples.len() < 25 {
            samples.push(serde_json::json!({
                "feed_item_id": item.id,
                "company_id": company.id,
                "company_name": company.name,
                "competing_company_names": competing_company_names,
                "title": item.title,
                "canonical_url": item.canonical_url,
                "source_id": item.source_id,
            }));
        }
        quarantines.push(FeedItemQualityQuarantine {
            feed_item_id: item.id,
            reason: "cross_company_item_not_explicitly_scoped".to_owned(),
            policy: "cross-company-item-scope.v1".to_owned(),
            reversible: true,
            metadata: serde_json::json!({
                "company_id": company.id,
                "company_name": company.name,
                "competing_company_names": competing_company_names,
                "audit": "feed-admin news-ownership-audit",
            }),
        });
    }
    Ok(NewsOwnershipAuditSnapshot {
        collision_item_count: items.len(),
        collision_candidate_url_count: candidate_urls.len(),
        same_issuer_item_count,
        distinct_issuer_item_count,
        unscoped_item_count: quarantines.len(),
        quarantines,
        samples,
    })
}

async fn audit_news_item_ownership(
    database: &Database,
    apply: bool,
    fail_on_unscoped: bool,
) -> Result<()> {
    let before = collect_news_item_ownership_audit(database).await?;
    let mut applied_count = 0_u64;
    let after = if apply && !before.quarantines.is_empty() {
        applied_count = database
            .quarantine_feed_items_for_quality(&before.quarantines)
            .await
            .context("quarantine unscoped cross-company feed items")?;
        collect_news_item_ownership_audit(database).await?
    } else {
        collect_news_item_ownership_audit(database).await?
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "apply": apply,
            "applied_count": applied_count,
            "before": before.summary(),
            "after": after.summary(),
        }))?
    );
    if fail_on_unscoped && after.unscoped_item_count > 0 {
        bail!(
            "{} unscoped distinct-issuer public item associations remain",
            after.unscoped_item_count
        );
    }
    Ok(())
}

async fn queue_discovery(database: &Database, selector: Option<&str>) -> Result<()> {
    let companies = if let Some(selector) = selector {
        vec![resolve_company(database, selector).await?]
    } else {
        let now = Utc::now();
        database
            .list_companies(100_000, 0)
            .await?
            .into_iter()
            .filter(|company| company.discovery_enabled && company.discovery_not_before <= now)
            .collect()
    };

    for company in &companies {
        let mut job = JobSpec::new(
            JobType::DiscoverCompany,
            format!("company:{}", company.id),
            Utc::now(),
        );
        job.company_id = Some(company.id);
        let queued = database.enqueue_job(&job).await?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "job_id": queued.id,
                "company_key": company.company_key,
                "company_name": company.name,
                "status": queued.status,
                "run_after": queued.run_after,
            }))?
        );
    }
    Ok(())
}

async fn handle_companies(database: &Database, command: CompanyCommand) -> Result<()> {
    match command {
        CompanyCommand::Import {
            file,
            validate_only,
            activate_new,
            discovery_cadence_seconds,
        } => {
            let universe = ValidatedUniverse::load(&file)
                .with_context(|| format!("validate company universe {}", file.display()))?;
            if validate_only {
                print_universe_validation(&universe)?;
                return Ok(());
            }
            let summary = database
                .import_company_universe(
                    &universe,
                    UniverseImportOptions {
                        activate_new,
                        discovery_cadence_seconds,
                    },
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        CompanyCommand::Activate {
            import_run_id,
            limit,
            start_at,
            spacing_seconds,
        } => {
            let summary = database
                .activate_company_wave(UniverseWaveOptions {
                    import_run_id,
                    limit,
                    start_at: start_at.unwrap_or_else(Utc::now),
                    spacing: Duration::from_secs(spacing_seconds),
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
    }
    Ok(())
}

fn print_universe_validation(universe: &ValidatedUniverse) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "valid",
            "schema_version": universe.document.schema_version,
            "source": universe.document.source,
            "input_sha256": universe.input_sha256,
            "input_bytes": universe.input_bytes,
            "company_count": universe.document.companies.len(),
        }))?
    );
    Ok(())
}

async fn queue_crawl(database: &Database, source_id: Option<Uuid>) -> Result<()> {
    let explicit_source = source_id.is_some();
    let sources = if let Some(source_id) = source_id {
        vec![
            database
                .get_source(source_id)
                .await?
                .with_context(|| format!("source {source_id} was not found"))?,
        ]
    } else {
        database
            .list_sources(None, Some(SourceStatus::Approved), 10_000, 0)
            .await?
    };

    for source in sources {
        let supported = match source.kind {
            SourceKind::Rss | SourceKind::Atom => true,
            SourceKind::Html | SourceKind::Browser if explicit_source => database
                .get_active_company_news_recipe_for_source(source.id)
                .await?
                .is_some(),
            SourceKind::Html | SourceKind::Browser => false,
        };
        if !supported {
            eprintln!(
                "skipping source {} because {} requires an active recipe and an explicit --source-id",
                source.source_id, source.kind
            );
            continue;
        }
        let mut job = JobSpec::new(
            JobType::CrawlSource,
            format!("source:{}", source.id),
            Utc::now(),
        );
        if explicit_source {
            job.priority = i16::MAX;
        }
        job.company_id = Some(source.company_id);
        job.source_id = Some(source.id);
        job.payload = serde_json::json!({
            "source_id": source.id,
            "trigger": if explicit_source {
                "manual_source_recrawl"
            } else {
                "manual_feed_crawl"
            },
        });
        let queued = database.enqueue_job(&job).await?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "job_id": queued.id,
                "source_id": source.id,
                "source_key": source.source_id,
                "status": queued.status,
                "run_after": queued.run_after,
            }))?
        );
    }
    Ok(())
}

async fn queue_export(database: &Database, target_id: Option<&str>) -> Result<()> {
    let targets = if let Some(target_id) = target_id {
        vec![
            database
                .get_export_target_by_key(target_id)
                .await?
                .with_context(|| format!("export target {target_id} was not found"))?,
        ]
    } else {
        database.list_export_targets(10_000, 0).await?
    };

    for target in targets.into_iter().filter(|target| target.enabled) {
        let mut job = JobSpec::new(
            JobType::ExportTarget,
            format!("target:{}", target.id),
            Utc::now(),
        );
        job.export_target_id = Some(target.id);
        let queued = database.enqueue_job(&job).await?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "job_id": queued.id,
                "export_target_id": target.id,
                "target": target.target_id,
                "status": queued.status,
                "run_after": queued.run_after,
            }))?
        );
    }
    Ok(())
}

/// Approval-gated recipe rebuild: list the rebuild candidate queue, and (with
/// --approve) enqueue bounded, spaced rebuild jobs for the top candidates.
async fn recipe_rebuild(
    database: &Database,
    approve: bool,
    limit: u32,
    spacing_seconds: u64,
    lookback_days: u64,
    max_articles: usize,
) -> Result<()> {
    let bounded = i64::from(limit.clamp(1, 10_000));
    let candidates = database.list_recipe_rebuild_candidates(bounded).await?;

    if !approve {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mode": "list",
                "candidate_count": candidates.len(),
                "candidates": candidates.iter().map(|c| serde_json::json!({
                    "company_key": c.company_key,
                    "company_name": c.company_name,
                    "reason": c.reason,
                    "consecutive_failures": c.consecutive_failures,
                    "stale_at": c.stale_at,
                    "last_attempt_at": c.last_attempt_at,
                })).collect::<Vec<_>>(),
                "hint": "re-run with --approve to enqueue rebuild jobs for these candidates",
            }))?
        );
        return Ok(());
    }

    if lookback_days == 0 || max_articles == 0 {
        bail!("lookback-days and max-articles must be positive");
    }
    let window_end = Utc::now();
    let lookback = Duration::from_secs(
        lookback_days
            .checked_mul(24 * 60 * 60)
            .context("lookback-days is too large")?,
    );
    let window_start = window_end
        - chrono::Duration::from_std(lookback).context("lookback is outside chrono range")?;
    let company_ids: Vec<Uuid> = candidates.iter().map(|c| c.company_id).collect();
    let (queued_count, first_run_after, last_run_after) = queue_news_extraction_campaign_jobs(
        database,
        company_ids,
        window_start,
        window_end,
        max_articles,
        false,
        spacing_seconds,
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "mode": "approve",
            "trigger": "operator_approved_recipe_rebuild",
            "candidates": candidates.len(),
            "queued": queued_count,
            "spacing_seconds": spacing_seconds,
            "first_run_after": first_run_after,
            "last_run_after": last_run_after,
            "window_start": window_start,
            "window_end": window_end,
            "max_articles": max_articles,
        }))?
    );
    Ok(())
}

async fn queue_news_extraction(database: &Database, options: NewsImportOptions<'_>) -> Result<()> {
    let NewsImportOptions {
        selector,
        all,
        retry_transient_after,
        limit,
        spacing_seconds,
        lookback_days,
        max_articles,
        include_covered,
    } = options;
    if lookback_days == 0 || max_articles == 0 {
        bail!("lookback-days and max-articles must be positive");
    }
    if max_articles > u32::MAX as usize {
        bail!("max-articles must fit in an unsigned 32-bit integer");
    }
    let lookback = Duration::from_secs(
        lookback_days
            .checked_mul(24 * 60 * 60)
            .context("lookback-days is too large")?,
    );
    let window_end = Utc::now();
    let window_start = window_end
        - chrono::Duration::from_std(lookback).context("lookback is outside chrono range")?;
    if let Some(retry_transient_after) = retry_transient_after {
        let total_eligible = database
            .count_companies_needing_transient_news_retry(retry_transient_after, include_covered)
            .await?;
        let company_ids = database
            .list_company_ids_needing_transient_news_retry(
                retry_transient_after,
                include_covered,
                i64::from(limit.clamp(1, 10_000)),
                0,
            )
            .await?;
        let (queued_count, first_run_after, last_run_after) = queue_news_extraction_campaign_jobs(
            database,
            company_ids,
            window_start,
            window_end,
            max_articles,
            include_covered,
            spacing_seconds,
        )
        .await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "trigger": "manual_transient_company_recipe_retry_campaign",
                "eligible": total_eligible,
                "queued": queued_count,
                "limit": limit.clamp(1, 10_000),
                "resumable": true,
                "retry_transient_after": retry_transient_after,
                "include_covered": include_covered,
                "spacing_seconds": spacing_seconds,
                "first_run_after": first_run_after,
                "last_run_after": last_run_after,
                "window_start": window_start,
                "window_end": window_end,
                "max_articles": max_articles,
            }))?
        );
        return Ok(());
    }
    if all {
        let total_eligible = database
            .count_companies_needing_news_recipes(include_covered)
            .await?;
        let company_ids = database
            .list_company_ids_needing_news_recipes(
                include_covered,
                i64::from(limit.clamp(1, 10_000)),
                0,
            )
            .await?;
        let (queued_count, first_run_after, last_run_after) = queue_news_extraction_campaign_jobs(
            database,
            company_ids,
            window_start,
            window_end,
            max_articles,
            include_covered,
            spacing_seconds,
        )
        .await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "trigger": "manual_all_company_recipe_campaign",
                "eligible": total_eligible,
                "queued": queued_count,
                "limit": limit.clamp(1, 10_000),
                "resumable": true,
                "include_covered": include_covered,
                "spacing_seconds": spacing_seconds,
                "first_run_after": first_run_after,
                "last_run_after": last_run_after,
                "window_start": window_start,
                "window_end": window_end,
                "max_articles": max_articles,
            }))?
        );
        return Ok(());
    }

    let selector = selector.context("--company is required unless --all is used")?;
    let company = resolve_company(database, selector).await?;
    if !include_covered
        && database
            .company_has_healthy_approved_feed(company.id)
            .await?
    {
        bail!(
            "company {} already has a healthy approved RSS/Atom source; pass --include-covered only when you intentionally want to discover additional product or engineering publications",
            company.company_key
        );
    }
    let queued = queue_news_extraction_job(
        database,
        &company,
        NewsExtractionJobOptions {
            window_start,
            window_end,
            max_articles,
            include_covered,
            run_after: window_end,
            priority: i16::MAX,
        },
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "job_id": queued.id,
            "company_key": company.company_key,
            "company_name": company.name,
            "trigger": "manual",
            "window_start": window_start,
            "window_end": window_end,
            "max_articles": max_articles,
            "include_covered": include_covered,
            "priority": queued.priority,
            "status": queued.status,
        }))?
    );
    Ok(())
}

async fn queue_news_extraction_campaign_jobs(
    database: &Database,
    company_ids: Vec<Uuid>,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    max_articles: usize,
    include_covered: bool,
    spacing_seconds: u64,
) -> Result<(u64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> {
    let mut queued_count = 0_u64;
    let mut first_run_after = None;
    let mut last_run_after = None;
    for (index, company_id) in company_ids.into_iter().enumerate() {
        let company = database
            .get_company(company_id)
            .await?
            .with_context(|| format!("company {company_id} was not found"))?;
        let delay_seconds = spacing_seconds
            .checked_mul(u64::try_from(index).unwrap_or(u64::MAX))
            .context("campaign spacing exceeds u64")?;
        let run_after = window_end
            + chrono::Duration::from_std(Duration::from_secs(delay_seconds))
                .context("campaign spacing is outside chrono range")?;
        queue_news_extraction_job(
            database,
            &company,
            NewsExtractionJobOptions {
                window_start,
                window_end,
                max_articles,
                include_covered,
                run_after,
                priority: 0,
            },
        )
        .await?;
        first_run_after.get_or_insert(run_after);
        last_run_after = Some(run_after);
        queued_count += 1;
    }
    Ok((queued_count, first_run_after, last_run_after))
}

async fn queue_news_extraction_job(
    database: &Database,
    company: &Company,
    options: NewsExtractionJobOptions,
) -> Result<feed_core::Job> {
    let NewsExtractionJobOptions {
        window_start,
        window_end,
        max_articles,
        include_covered,
        run_after,
        priority,
    } = options;
    let mut job = JobSpec::new(
        JobType::ExtractCompanyNews,
        format!("company:{}:manual-news-import", company.id),
        run_after,
    );
    job.company_id = Some(company.id);
    job.priority = priority;
    job.max_attempts = 3;
    job.payload = serde_json::json!({
        "schema_version": "company-news-extraction-job.v1",
        "window_start": window_start,
        "window_end": window_end,
        "max_articles": max_articles,
        "include_covered": include_covered,
    });
    Ok(database.enqueue_job(&job).await?)
}

#[derive(Clone, Copy)]
struct NewsExtractionJobOptions {
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    max_articles: usize,
    include_covered: bool,
    run_after: DateTime<Utc>,
    priority: i16,
}

async fn handle_candidates(database: &Database, command: CandidateCommand) -> Result<()> {
    match command {
        CandidateCommand::List {
            company,
            status,
            limit,
            offset,
        } => {
            let company_id = if let Some(selector) = company {
                Some(resolve_company(database, &selector).await?.id)
            } else {
                None
            };
            let status = if status.eq_ignore_ascii_case("all") {
                None
            } else {
                Some(CandidateStatus::from_str(&status)?)
            };
            let candidates = database
                .list_source_candidates(
                    company_id,
                    status,
                    i64::from(limit.min(1_000)),
                    i64::try_from(offset).unwrap_or(i64::MAX),
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&candidates)?);
        }
        CandidateCommand::Validate {
            candidate_id,
            limit,
            include_covered,
        } => {
            let queued = if let Some(candidate_id) = candidate_id {
                database
                    .enqueue_candidate_validation(candidate_id, Utc::now(), i16::MAX)
                    .await?;
                1
            } else if include_covered {
                database
                    .enqueue_unvalidated_candidate_jobs_including_covered(
                        Utc::now(),
                        limit.clamp(1, 5_000),
                    )
                    .await?
            } else {
                database
                    .enqueue_unvalidated_candidate_jobs(Utc::now(), limit.clamp(1, 5_000))
                    .await?
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "queued": queued,
                    "include_covered": include_covered,
                }))?
            );
        }
        CandidateCommand::ReconsiderAutomaticScope { limit } => {
            let queued = database
                .reconsider_automatically_rejected_scope_candidates(
                    Utc::now(),
                    limit.clamp(1, 5_000),
                )
                .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "queued": queued,
                    "reason": "feed_title_company_scope_policy_v1",
                    "operator_rejections_reopened": 0,
                }))?
            );
        }
        CandidateCommand::Accept {
            candidate_id,
            source_id,
            freshness_seconds,
            public_export,
        } => {
            let candidate = database
                .get_source_candidate(candidate_id)
                .await?
                .with_context(|| format!("candidate {candidate_id} was not found"))?;
            if !matches!(candidate.candidate_kind, SourceKind::Rss | SourceKind::Atom) {
                bail!(
                    "candidate {candidate_id} is {}; this release only crawls RSS/Atom sources",
                    candidate.candidate_kind
                );
            }
            let company = database
                .get_company(candidate.company_id)
                .await?
                .with_context(|| format!("company {} was not found", candidate.company_id))?;
            let source_id = source_id.unwrap_or_else(|| {
                default_source_id(
                    &company.company_key,
                    candidate.candidate_kind.as_str(),
                    candidate.id,
                )
            });
            let source = database
                .accept_source_candidate(candidate_id, &source_id, freshness_seconds, public_export)
                .await?;
            println!("{}", serde_json::to_string_pretty(&source)?);
        }
        CandidateCommand::Reject { candidate_id } => {
            database.reject_source_candidate(candidate_id).await?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "candidate_id": candidate_id,
                    "status": "rejected",
                }))?
            );
        }
    }
    Ok(())
}

async fn resolve_company(database: &Database, selector: &str) -> Result<Company> {
    if let Some(company) = database.get_company_by_key(selector).await? {
        return Ok(company);
    }
    let matches = database.find_companies_by_name(selector, 3).await?;
    match matches.as_slice() {
        [] => bail!("company {selector} was not found by key or exact name"),
        [company] => Ok(company.clone()),
        companies => {
            let keys = companies
                .iter()
                .map(|company| company.company_key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("company name {selector} is ambiguous; use one of these keys: {keys}")
        }
    }
}

fn default_source_id(company_key: &str, kind: &str, candidate_id: Uuid) -> String {
    let company_key = company_key
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let suffix = candidate_id.simple().to_string();
    format!("co-{company_key}-{kind}-{}", &suffix[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_source_ids_are_stable_and_readable() {
        let id = Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").expect("valid UUID");
        assert_eq!(
            default_source_id("berkshire-hathaway", "rss", id),
            "co-berkshire-hathaway-rss-01234567"
        );
    }
}
