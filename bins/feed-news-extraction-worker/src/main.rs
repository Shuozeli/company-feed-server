use std::sync::Arc;

use anyhow::{Context, Result, bail};
use feed_api::{ApiState, router};
use feed_core::{AppSettings, CompaniesConfig, ExportTargetsConfig};
use feed_db::Database;
use feed_jobs::build_news_extraction_job_registry;
use feed_scheduler::{JobHandler, JobRunner, JobRunnerConfig};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let settings = AppSettings::from_env().context("load application settings")?;
    if !settings.news_extraction_enabled {
        bail!("NEWS_EXTRACTION_ENABLED must be true for this worker");
    }
    let companies = CompaniesConfig::load(&settings.companies_config_path)
        .context("load company seed configuration")?;
    let export_targets = ExportTargetsConfig::load(&settings.export_targets_config_path)
        .context("load export target configuration")?;
    let database = Database::connect(&settings.database_url, settings.database_max_connections)
        .await
        .context("connect to Postgres")?;
    database
        .migrate()
        .await
        .context("run database migrations")?;
    database
        .sync_seed_config(&companies, &export_targets)
        .await
        .context("synchronize seed configuration")?;

    let handler = Arc::new(
        build_news_extraction_job_registry(database.clone(), &settings)
            .context("build manual company news import handler")?,
    );
    let supported_job_types = handler.supported_job_types().to_vec();
    let shutdown = CancellationToken::new();
    let mut runner_tasks = Vec::new();
    if settings.news_extraction_run_jobs {
        for lane in 0..settings.news_extraction_job_concurrency {
            let worker_id = if settings.news_extraction_job_concurrency == 1 {
                settings.worker_id.clone()
            } else {
                format!("{}-lane-{}", settings.worker_id, lane + 1)
            };
            let runner = JobRunner::new(
                database.clone(),
                worker_id,
                handler.clone(),
                JobRunnerConfig::from_settings(&settings),
            )
            .context("configure manual company news import runner")?;
            let runner_shutdown = shutdown.clone();
            runner_tasks.push(tokio::spawn(async move {
                runner.run_until_cancelled(runner_shutdown).await
            }));
        }
    }
    let state = ApiState::new(
        database.clone(),
        "feed-news-extraction-worker",
        settings.news_extraction_run_jobs,
        supported_job_types,
    );
    let listener = tokio::net::TcpListener::bind(settings.news_extraction_worker_bind_addr)
        .await
        .with_context(|| {
            format!(
                "bind manual company news import health server to {}",
                settings.news_extraction_worker_bind_addr
            )
        })?;
    info!(
        address = %settings.news_extraction_worker_bind_addr,
        run_jobs = settings.news_extraction_run_jobs,
        job_concurrency = settings.news_extraction_job_concurrency,
        "manual company news import worker listening"
    );

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal(shutdown.clone()))
        .await
        .context("serve manual company news import health API")?;

    shutdown.cancel();
    for task in runner_tasks {
        task.await
            .context("news extraction runner task panicked")??;
    }
    database.close().await;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,feed_news_extraction_worker=debug"));
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
                warn!(%error, "failed to install termination signal handler");
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
