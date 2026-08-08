use std::sync::Arc;

use anyhow::{Context, Result};
use feed_api::{ApiState, router};
use feed_core::{AppSettings, CompaniesConfig, ExportTargetsConfig};
use feed_db::Database;
use feed_jobs::{DiscoveryJobProducer, build_discovery_job_registry};
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
        build_discovery_job_registry(database.clone(), &settings)
            .context("build discovery job handler")?,
    );
    let supported_job_types = handler.supported_job_types().to_vec();
    let shutdown = CancellationToken::new();
    let runner_task = if settings.run_jobs {
        let runner = JobRunner::new(
            database.clone(),
            settings.worker_id.clone(),
            handler,
            JobRunnerConfig::from_settings(&settings),
        )
        .context("configure discovery job runner")?;
        let runner_shutdown = shutdown.clone();
        Some(tokio::spawn(async move {
            runner.run_until_cancelled(runner_shutdown).await
        }))
    } else {
        None
    };
    let producer_task = if settings.run_jobs && settings.schedule_jobs {
        let producer = DiscoveryJobProducer::new(
            database.clone(),
            settings.scheduler_scan_interval,
            settings.discovery_queue_target,
        );
        let producer_shutdown = shutdown.clone();
        Some(tokio::spawn(async move {
            producer.run_until_cancelled(producer_shutdown).await;
        }))
    } else {
        None
    };

    let state = ApiState::new(
        database.clone(),
        "feed-discovery-worker",
        settings.run_jobs,
        supported_job_types,
    )
    .with_operator_api_token(settings.operator_api_token);
    let listener = tokio::net::TcpListener::bind(settings.discovery_worker_bind_addr)
        .await
        .with_context(|| {
            format!(
                "bind discovery worker health server to {}",
                settings.discovery_worker_bind_addr
            )
        })?;
    info!(
        address = %settings.discovery_worker_bind_addr,
        "discovery worker health server listening"
    );

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal(shutdown.clone()))
        .await
        .context("serve discovery worker health API")?;

    shutdown.cancel();
    if let Some(task) = runner_task {
        task.await.context("discovery job runner task panicked")??;
    }
    if let Some(task) = producer_task {
        task.await.context("discovery job producer task panicked")?;
    }
    database.close().await;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,feed_discovery_worker=debug"));
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
