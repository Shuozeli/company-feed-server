//! feed-payload-offloader
//!
//! Moves `raw_crawl_items.payload` (raw crawled content, a write-only archive
//! that is never read back once a row is normalized) out of Postgres into
//! MinIO/S3 object storage, then blanks the DB column and records the object
//! key in `payload_s3_key`.
//!
//! Payloads are packed into gzipped NDJSON **batch objects** (many rows per
//! object) rather than one object per row: the NAS MinIO is fsync-bound on small
//! files (~250 PUT/s), so 15M individual objects would take ~16h, whereas a few
//! thousand batch objects finish in minutes and compress ~5-10x. Because the
//! payload is a cold, never-read archive, per-row addressability has no value;
//! to recover one row's payload, download its batch object and grep by id.
//!
//! One tool, two jobs: run to-dry for the one-time backfill; run
//! `OFFLOAD_CONTINUOUS=true` for steady-state offload of freshly crawled rows.
//! Best-effort: if the store is down, rows keep their payload and are retried.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use aws_credential_types::Credentials;
use aws_sdk_s3::config::{BehaviorVersion, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
struct RawStore {
    client: Client,
    bucket: String,
}

impl RawStore {
    fn new(endpoint: String, region: String, access_key: String, secret_key: String, bucket: String) -> Self {
        let credentials = Credentials::new(access_key, secret_key, None, None, "feed-payload-offloader");
        let s3_config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region))
            .endpoint_url(endpoint)
            .credentials_provider(credentials)
            .force_path_style(true) // REQUIRED for MinIO/RustFS
            .build();
        Self { client: Client::from_conf(s3_config), bucket }
    }

    async fn ensure_bucket(&self) -> Result<()> {
        if self.client.head_bucket().bucket(&self.bucket).send().await.is_err() {
            self.client
                .create_bucket()
                .bucket(&self.bucket)
                .send()
                .await
                .context("failed to create raw-crawl bucket")?;
        }
        Ok(())
    }

    async fn put(&self, key: &str, body: Bytes, content_type: &str) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(body))
            .content_type(content_type)
            .send()
            .await
            .with_context(|| format!("put_object failed for {key}"))?;
        Ok(())
    }
}

fn required_env(key: &str) -> Result<String> {
    std::env::var(key).map_err(|_| anyhow!("missing required env var {key}"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// One worker: repeatedly claims a disjoint batch with FOR UPDATE SKIP LOCKED
/// (so N workers read+detoast in parallel across Postgres backends without
/// colliding), packs it into a gzipped NDJSON object, uploads it, blanks the
/// payloads + stamps the key, and commits. Returns total rows migrated.
async fn run_worker(
    store: RawStore,
    pool: PgPool,
    rows_per_object: usize,
    continuous: bool,
    idle_sleep: Duration,
    shutdown: Arc<AtomicBool>,
    migrated: Arc<std::sync::atomic::AtomicU64>,
    errors: Arc<std::sync::atomic::AtomicU64>,
) {
    while !shutdown.load(Ordering::Relaxed) {
        match offload_one_batch(&store, &pool, rows_per_object).await {
            Ok(0) => {
                if continuous {
                    tokio::time::sleep(idle_sleep).await;
                } else {
                    break;
                }
            }
            Ok(n) => {
                migrated.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(error) => {
                errors.fetch_add(1, Ordering::Relaxed);
                warn!(%error, "batch failed; rows retried next pass");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn offload_one_batch(
    store: &RawStore,
    pool: &PgPool,
    rows_per_object: usize,
) -> Result<usize> {
    let mut tx = pool.begin().await.context("begin tx")?;
    // Filter only on payload_s3_key IS NULL (backed by the partial index) so the
    // planner never detoasts payload just to test it; payload is detoasted once,
    // for the ::text projection. The handful of rows whose payload is already
    // '{}' upload a tiny object and get stamped, which is harmless and cheap.
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, payload::text \
         FROM raw_crawl_items \
         WHERE payload_s3_key IS NULL \
         ORDER BY id \
         LIMIT $1 \
         FOR UPDATE SKIP LOCKED",
    )
    .bind(rows_per_object as i64)
    .fetch_all(&mut *tx)
    .await
    .context("claim batch")?;

    if rows.is_empty() {
        return Ok(0);
    }

    let key = format!("raw/batch/{}.ndjson.gz", Uuid::new_v4());
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    for (id, payload_text) in &rows {
        encoder
            .write_all(format!("{{\"id\":\"{id}\",\"payload\":{payload_text}}}\n").as_bytes())
            .context("gzip write")?;
    }
    let compressed = encoder.finish().context("gzip finish")?;

    store
        .put(&key, Bytes::from(compressed), "application/gzip")
        .await?;

    let ids: Vec<Uuid> = rows.iter().map(|(id, _)| *id).collect();
    sqlx::query(
        "UPDATE raw_crawl_items \
         SET payload = '{}'::jsonb, payload_s3_key = $2 \
         WHERE id = ANY($1)",
    )
    .bind(&ids)
    .bind(&key)
    .execute(&mut *tx)
    .await
    .context("db update after upload failed")?;

    tx.commit().await.context("commit tx")?;
    Ok(rows.len())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let database_url = required_env("DATABASE_URL")?;
    let store = RawStore::new(
        required_env("RUSTFS_ENDPOINT")?,
        env_or("RUSTFS_REGION", "us-east-1"),
        required_env("RUSTFS_ACCESS_KEY")?,
        required_env("RUSTFS_SECRET_KEY")?,
        env_or("RAW_CRAWL_BUCKET", "company-feed-crawl-raw"),
    );
    // Rows packed per gzipped object, and number of objects built concurrently.
    let rows_per_object: usize = parse_env("OFFLOAD_BATCH_SIZE", 2000);
    let object_concurrency: usize = parse_env("OFFLOAD_CONCURRENCY", 8);
    let db_pool: u32 = parse_env("OFFLOAD_DB_POOL", 16);
    let continuous: bool = parse_env("OFFLOAD_CONTINUOUS", false);
    let idle_sleep = Duration::from_secs(parse_env("OFFLOAD_IDLE_SLEEP_SECS", 30));

    store.ensure_bucket().await?;
    let pool = PgPoolOptions::new()
        .max_connections(db_pool.max(object_concurrency as u32 + 2))
        .acquire_timeout(Duration::from_secs(15))
        .connect(&database_url)
        .await
        .context("connect to Postgres")?;

    info!(
        bucket = %store.bucket,
        rows_per_object,
        object_concurrency,
        "payload offloader starting (batch mode)"
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("received ctrl-c, will stop after in-flight batches");
            shutdown.store(true, Ordering::Relaxed);
        });
    }

    let migrated = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let errors = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let start = Instant::now();

    // Periodic progress reporter.
    {
        let migrated = migrated.clone();
        let errors = errors.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                let done = migrated.load(Ordering::Relaxed);
                let elapsed = start.elapsed().as_secs_f64().max(0.001);
                info!(
                    migrated = done,
                    batch_errors = errors.load(Ordering::Relaxed),
                    rate_per_sec = format!("{:.0}", done as f64 / elapsed),
                    "progress"
                );
            }
        });
    }

    let workers: Vec<_> = (0..object_concurrency)
        .map(|_| {
            tokio::spawn(run_worker(
                store.clone(),
                pool.clone(),
                rows_per_object,
                continuous,
                idle_sleep,
                shutdown.clone(),
                migrated.clone(),
                errors.clone(),
            ))
        })
        .collect();

    for worker in workers {
        let _ = worker.await;
    }

    info!(
        migrated = migrated.load(Ordering::Relaxed),
        batch_errors = errors.load(Ordering::Relaxed),
        "offloader stopped"
    );
    Ok(())
}
