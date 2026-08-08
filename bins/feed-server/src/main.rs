use anyhow::{Context, Result};
use feed_api::{ApiState, router};
use feed_core::{AppSettings, CompaniesConfig, ExportTargetsConfig};
use feed_db::Database;
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
    let sync = database
        .sync_seed_config(&companies, &export_targets)
        .await
        .context("synchronize seed configuration")?;
    info!(
        companies = sync.companies,
        export_targets = sync.export_targets,
        "seed configuration synchronized"
    );

    let shutdown = CancellationToken::new();
    let state = ApiState::new(database.clone(), "feed-server", false, Vec::new())
        .with_operator_api_token(settings.operator_api_token);
    let listener = tokio::net::TcpListener::bind(settings.bind_addr)
        .await
        .with_context(|| format!("bind HTTP server to {}", settings.bind_addr))?;
    info!(address = %settings.bind_addr, "feed server listening");

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal(shutdown.clone()))
        .await
        .context("serve HTTP API")?;

    shutdown.cancel();
    database.close().await;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,feed_server=debug"));
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
