use std::sync::Arc;

use anyhow::{Context, Result};
use feed_api::{ApiState, router};
use feed_core::{AppSettings, CompaniesConfig, ExportTargetsConfig};
use feed_db::Database;
use feed_jobs::{ContentCrawlJobProducer, build_content_crawl_job_registry};
use feed_scheduler::{JobHandler, JobRunner, JobRunnerConfig};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let settings = AppSettings::from_env().context("load application settings")?;
    let companies = CompaniesConfig::load(&settings.companies_config_path)
        .context("load company seed configuration")?;
    let export_targets = ExportTargetsConfig::load(&settings.export_targets_config_path)
        .context("load export target configuration")?;
    let database = Database::connect(&settings.database_url, settings.database_max_connections)
        .await
        .context("connect to Postgres")?;
    database
        .ensure_schema()
        .await
        .context("ensure database schema")?;
    database
        .sync_seed_config(&companies, &export_targets)
        .await
        .context("synchronize seed configuration")?;

    let handler = Arc::new(
        build_content_crawl_job_registry(database.clone(), &settings)
            .context("build article content crawl handler")?,
    );
    let supported_job_types = handler.supported_job_types().to_vec();
    let shutdown = CancellationToken::new();
    let jobs_enabled = settings.content_crawl_enabled && settings.content_crawl_run_jobs;
    let mut runner_tasks = Vec::new();
    if jobs_enabled {
        for runner_index in 0..settings.content_crawl_job_concurrency {
            let runner = JobRunner::new(
                database.clone(),
                format!("{}-content-{runner_index}", settings.worker_id),
                handler.clone(),
                JobRunnerConfig::from_settings(&settings),
            )
            .context("configure article content crawl runner")?;
            let runner_shutdown = shutdown.clone();
            runner_tasks.push(tokio::spawn(async move {
                runner.run_until_cancelled(runner_shutdown).await
            }));
        }
    }
    let producer_task = if jobs_enabled {
        let producer = ContentCrawlJobProducer::new(
            database.clone(),
            settings.job_poll_interval,
            settings.content_crawl_refresh,
            settings.content_crawl_min_content_chars,
            settings.content_crawl_job_concurrency,
        )
        .context("configure article content crawl producer")?;
        let producer_shutdown = shutdown.clone();
        Some(tokio::spawn(async move {
            producer.run_until_cancelled(producer_shutdown).await;
        }))
    } else {
        None
    };

    let state = ApiState::new(
        database.clone(),
        "feed-content-worker",
        jobs_enabled,
        supported_job_types,
    );
    let listener = tokio::net::TcpListener::bind(settings.content_crawl_worker_bind_addr)
        .await
        .with_context(|| {
            format!(
                "bind article content crawl health server to {}",
                settings.content_crawl_worker_bind_addr
            )
        })?;
    info!(
        address = %settings.content_crawl_worker_bind_addr,
        jobs_enabled,
        job_concurrency = settings.content_crawl_job_concurrency,
        batch_size = settings.content_crawl_batch_size,
        max_concurrency = settings.content_crawl_max_concurrency,
        max_per_host_concurrency = settings.content_crawl_max_per_host_concurrency,
        "article content crawl worker listening"
    );

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal(shutdown.clone()))
        .await
        .context("serve article content crawl health API")?;

    shutdown.cancel();
    for task in runner_tasks {
        task.await.context("content crawl runner task panicked")??;
    }
    if let Some(task) = producer_task {
        task.await.context("content crawl producer task panicked")?;
    }
    database.close().await;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,feed_content_worker=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal(shutdown: CancellationToken) {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            warn!(%error, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                warn!(%error, "failed to install SIGTERM handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    info!("shutdown requested");
    shutdown.cancel();
}
