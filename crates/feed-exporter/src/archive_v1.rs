use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Datelike, Utc};
use feed_core::{ExportableFeedItem, ExportedItem};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::ExportError;

pub(super) const SCHEMA_VERSION: &str = "1.0.0";
const SHARD_MAX_RECORDS: usize = 5_000;
const SHARD_TARGET_MAX_BYTES: usize = 1024 * 1024;
const SHARD_MAX_PREFIX_DEPTH: usize = 8;

pub(super) const OWNED_PATHS: [&str; 14] = [
    ".github",
    "ARCHITECTURE.md",
    "CONTENT_RIGHTS.md",
    "HEAD.json",
    "LICENSE.md",
    "README.md",
    "articles",
    "companies",
    "feeds",
    "index",
    "indexes",
    "openapi",
    "schemas",
    "scripts",
];

const STATIC_FILES: [(&str, &[u8]); 14] = [
    (
        ".github/workflows/validate.yml",
        include_bytes!("../assets/github/workflows/validate.yml"),
    ),
    (
        "ARCHITECTURE.md",
        include_bytes!("../assets/ARCHITECTURE.md"),
    ),
    (
        "CONTENT_RIGHTS.md",
        include_bytes!("../assets/CONTENT_RIGHTS.md"),
    ),
    ("LICENSE.md", include_bytes!("../assets/LICENSE.md")),
    (
        "openapi/openapi.json",
        include_bytes!("../assets/openapi/openapi.json"),
    ),
    (
        "schemas/README.md",
        include_bytes!("../assets/schemas/README.md"),
    ),
    (
        "schemas/v1/archive.schema.json",
        include_bytes!("../assets/schemas/v1/archive.schema.json"),
    ),
    (
        "schemas/v1/archive-manifest.schema.json",
        include_bytes!("../assets/schemas/v1/archive-manifest.schema.json"),
    ),
    (
        "schemas/v1/article-record.schema.json",
        include_bytes!("../assets/schemas/v1/article-record.schema.json"),
    ),
    (
        "schemas/v1/company-manifest.schema.json",
        include_bytes!("../assets/schemas/v1/company-manifest.schema.json"),
    ),
    (
        "schemas/v1/head.schema.json",
        include_bytes!("../assets/schemas/v1/head.schema.json"),
    ),
    (
        "schemas/v1/index-document.schema.json",
        include_bytes!("../assets/schemas/v1/index-document.schema.json"),
    ),
    (
        "schemas/v1/partition-manifest.schema.json",
        include_bytes!("../assets/schemas/v1/partition-manifest.schema.json"),
    ),
    (
        "scripts/validate_archive.py",
        include_bytes!("../assets/scripts/validate_archive.py"),
    ),
];

pub(super) fn materialize_archive(
    root: &Path,
    items: &[ExportableFeedItem],
) -> Result<Vec<ExportedItem>, ExportError> {
    let projections = project_articles(items)?;
    let mut desired = STATIC_FILES
        .into_iter()
        .map(|(path, bytes)| (PathBuf::from(path), bytes.to_vec()))
        .collect::<BTreeMap<_, _>>();

    let generation = generation_id(&projections)?;
    let generated_at = projections
        .iter()
        .map(|projection| projection.item.item.updated_at)
        .max()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339();

    let mut company_accumulators = BTreeMap::<String, CompanyAccumulator>::new();
    let mut index_partitions = BTreeMap::<String, Vec<IndexLine>>::new();
    let mut exported = Vec::with_capacity(projections.len());

    for projection in &projections {
        let article = markdown_document(projection);
        let article_bytes = article.into_bytes();
        let article_sha256 = sha256_prefixed(&article_bytes);
        let record = article_record(projection, article_bytes.len(), &article_sha256);

        desired.insert(projection.article_path.clone(), article_bytes);
        desired.insert(projection.record_path.clone(), pretty_json(&record)?);

        let index = index_document(projection);
        let mut line = serde_json::to_vec(&index)?;
        line.push(b'\n');
        index_partitions
            .entry(projection.archive_month.clone())
            .or_default()
            .push(IndexLine {
                document_id: projection.document_id.clone(),
                bytes: line,
            });

        company_accumulators
            .entry(projection.item.company_key.clone())
            .or_insert_with(|| CompanyAccumulator::new(projection))
            .record(projection);

        exported.push(ExportedItem {
            feed_item_id: projection.item.item.id,
            exported_path: projection.article_path.clone(),
            exported_content_hash: projection.item.item.content_hash.clone(),
        });
    }

    for accumulator in company_accumulators.values() {
        desired.insert(
            accumulator.manifest_path(),
            pretty_json(&accumulator.manifest())?,
        );
    }

    let mut partition_descriptors = Vec::with_capacity(index_partitions.len());
    for (partition, mut lines) in index_partitions {
        lines.sort_by(|left, right| left.document_id.cmp(&right.document_id));
        let base = partition_base(&partition)?;
        let mut shards = Vec::new();
        build_shards("", &base, &lines, &mut desired, &mut shards)?;
        shards.sort_by(|left, right| left.prefix.cmp(&right.prefix));
        let byte_count = shards.iter().map(|shard| shard.byte_count).sum();
        let shard_count = shards.len();
        let manifest = PartitionManifest {
            schema_version: SCHEMA_VERSION,
            generation: &generation,
            partition: &partition,
            record_count: lines.len(),
            shard_count,
            byte_count,
            shards,
        };
        let manifest_path = base.join("manifest.json");
        let manifest_bytes = pretty_json(&manifest)?;
        partition_descriptors.push(PartitionDescriptor {
            partition: partition.clone(),
            manifest_path: path_string(&manifest_path)?,
            record_count: lines.len(),
            shard_count,
            byte_count,
            sha256: sha256_prefixed(&manifest_bytes),
        });
        desired.insert(manifest_path, manifest_bytes);
    }

    let company_count = company_accumulators.len();
    let first_published_at = projections
        .iter()
        .filter_map(|projection| projection.item.item.published_at)
        .min()
        .map(|value| value.to_rfc3339());
    let last_published_at = projections
        .iter()
        .filter_map(|projection| projection.item.item.published_at)
        .max()
        .map(|value| value.to_rfc3339());
    let manifest_path = PathBuf::from("index/v1/current/manifest.json");
    let manifest = ArchiveManifest {
        schema_version: SCHEMA_VERSION,
        generation: &generation,
        generated_at: &generated_at,
        record_count: projections.len(),
        company_count,
        first_published_at: first_published_at.as_deref(),
        last_published_at: last_published_at.as_deref(),
        partitioning: PartitioningPolicy {
            primary: "archive_month",
            secondary: "document_id_sha256_prefix_trie",
            max_records_per_shard: SHARD_MAX_RECORDS,
            target_max_bytes_per_shard: SHARD_TARGET_MAX_BYTES,
            max_hash_prefix_depth: SHARD_MAX_PREFIX_DEPTH,
        },
        partitions: partition_descriptors,
    };
    desired.insert(manifest_path.clone(), pretty_json(&manifest)?);

    let head = ArchiveHead {
        schema_version: SCHEMA_VERSION,
        generation: &generation,
        generated_at: &generated_at,
        manifest_path: &path_string(&manifest_path)?,
        record_count: projections.len(),
        company_count,
    };
    desired.insert(PathBuf::from("HEAD.json"), pretty_json(&head)?);
    desired.insert(
        PathBuf::from("README.md"),
        readme(projections.len(), company_count, &generation).into_bytes(),
    );

    sync_owned_files(root, &desired)?;
    Ok(exported)
}

fn project_articles<'a>(
    items: &'a [ExportableFeedItem],
) -> Result<Vec<ArticleProjection<'a>>, ExportError> {
    let mut projections = Vec::with_capacity(items.len());
    let mut identities = BTreeSet::new();
    for item in items {
        let document_id = document_id(item);
        if !identities.insert(document_id.clone()) {
            return Err(ExportError::Invariant(format!(
                "multiple exportable records resolve to document ID {document_id}"
            )));
        }
        let archive_date = item.item.published_at.unwrap_or(item.item.created_at);
        let archive_month = format!("{:04}-{:02}", archive_date.year(), archive_date.month());
        let article_path = stable_article_path(item, &document_id, archive_date)?;
        let record_path = article_path.with_file_name("record.json");
        projections.push(ArticleProjection {
            item,
            document_id,
            archive_month,
            article_path,
            record_path,
        });
    }
    projections.sort_by(|left, right| left.document_id.cmp(&right.document_id));
    Ok(projections)
}

fn document_id(item: &ExportableFeedItem) -> String {
    sha256_hex(
        format!(
            "company-news-archive/article/v1\0{}\0{}",
            item.company_key, item.item.canonical_url
        )
        .as_bytes(),
    )
}

fn company_bucket(company_key: &str) -> String {
    sha256_hex(company_key.as_bytes())[..2].to_owned()
}

fn stable_article_path(
    item: &ExportableFeedItem,
    document_id: &str,
    archive_date: DateTime<Utc>,
) -> Result<PathBuf, ExportError> {
    if let Some(previous) = item.previous_exported_path.as_deref()
        && previous.file_name() == Some(OsStr::new("article.md"))
        && previous.parent().and_then(Path::file_name) == Some(OsStr::new(document_id))
        && previous.starts_with("articles/v1")
        && is_safe_owned_relative_path(previous)
    {
        return Ok(previous.to_owned());
    }
    if !is_safe_company_key(&item.company_key) {
        return Err(ExportError::InvalidPath(format!(
            "company key {} cannot form an archive path",
            item.company_key
        )));
    }
    Ok(PathBuf::from("articles")
        .join("v1")
        .join(company_bucket(&item.company_key))
        .join(&item.company_key)
        .join(format!("{:04}", archive_date.year()))
        .join(format!("{:02}", archive_date.month()))
        .join(&document_id[..2])
        .join(document_id)
        .join("article.md"))
}

fn is_safe_company_key(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

fn is_safe_owned_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn generation_id(projections: &[ArticleProjection<'_>]) -> Result<String, ExportError> {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "company-news-archive/generation/{SCHEMA_VERSION}\0"
    ));
    for projection in projections {
        let line = index_document(projection);
        hasher.update(serde_json::to_vec(&line)?);
        hasher.update(b"\n");
    }
    Ok(hex::encode(hasher.finalize()))
}

fn article_record<'a>(
    projection: &'a ArticleProjection<'a>,
    article_bytes: usize,
    article_sha256: &'a str,
) -> ArticleRecord<'a> {
    let item = &projection.item.item;
    ArticleRecord {
        schema_version: SCHEMA_VERSION,
        document_id: &projection.document_id,
        company: CompanyRef {
            key: &projection.item.company_key,
            name: &projection.item.company_name,
        },
        source: SourceRef {
            id: &projection.item.source_key,
            kind: item.source_kind.to_string(),
        },
        urls: UrlSet {
            observed: item.url.as_str(),
            canonical: item.canonical_url.as_str(),
        },
        title: &item.title,
        summary: &item.summary,
        published_at: item.published_at.map(|value| value.to_rfc3339()),
        first_seen_at: item.created_at.to_rfc3339(),
        fetched_at: item.fetched_at.to_rfc3339(),
        last_updated_at: item.updated_at.to_rfc3339(),
        paths: ArchivePaths {
            article: path_string_lossless(&projection.article_path),
            record: path_string_lossless(&projection.record_path),
        },
        content: ContentDescriptor {
            media_type: "text/markdown; charset=utf-8",
            bytes: article_bytes,
            sha256: article_sha256,
            normalized_content_hash: &item.content_hash,
        },
        provenance: Provenance {
            source_item_id: item.id.to_string(),
            external_id: &item.external_id,
        },
    }
}

fn index_document<'a>(projection: &'a ArticleProjection<'a>) -> IndexDocument<'a> {
    let item = &projection.item.item;
    IndexDocument {
        schema_version: SCHEMA_VERSION,
        document_id: &projection.document_id,
        company_key: &projection.item.company_key,
        company_name: &projection.item.company_name,
        source_id: &projection.item.source_key,
        source_kind: item.source_kind.to_string(),
        canonical_url: item.canonical_url.as_str(),
        observed_url: item.url.as_str(),
        title: &item.title,
        summary: &item.summary,
        body_text: &item.body_text,
        published_at: item.published_at.map(|value| value.to_rfc3339()),
        first_seen_at: item.created_at.to_rfc3339(),
        fetched_at: item.fetched_at.to_rfc3339(),
        last_updated_at: item.updated_at.to_rfc3339(),
        content_hash: &item.content_hash,
        article_path: path_string_lossless(&projection.article_path),
        record_path: path_string_lossless(&projection.record_path),
    }
}

fn markdown_document(projection: &ArticleProjection<'_>) -> String {
    let item = &projection.item.item;
    let quoted = |value: &str| serde_json::to_string(value).expect("strings serialize to JSON");
    let title = item.title.replace(['\r', '\n'], " ");
    format!(
        "---\nschema_version: {schema}\ndocument_id: {document_id}\ncompany_key: {company_key}\ncompany: {company}\nsource_id: {source_id}\ncanonical_url: {canonical_url}\npublished_at: {published_at}\nfirst_seen_at: {first_seen_at}\nfetched_at: {fetched_at}\ncontent_hash: {content_hash}\n---\n\n# {title}\n\n{body}\n",
        schema = quoted(SCHEMA_VERSION),
        document_id = quoted(&projection.document_id),
        company_key = quoted(&projection.item.company_key),
        company = quoted(&projection.item.company_name),
        source_id = quoted(&projection.item.source_key),
        canonical_url = quoted(item.canonical_url.as_str()),
        published_at = item
            .published_at
            .map(|value| quoted(&value.to_rfc3339()))
            .unwrap_or_else(|| "null".to_owned()),
        first_seen_at = quoted(&item.created_at.to_rfc3339()),
        fetched_at = quoted(&item.fetched_at.to_rfc3339()),
        content_hash = quoted(&item.content_hash),
        body = item.body_markdown,
    )
}

fn build_shards(
    prefix: &str,
    base: &Path,
    lines: &[IndexLine],
    desired: &mut BTreeMap<PathBuf, Vec<u8>>,
    descriptors: &mut Vec<ShardDescriptor>,
) -> Result<(), ExportError> {
    let byte_count = lines.iter().map(|line| line.bytes.len()).sum::<usize>();
    let should_split = (lines.len() > SHARD_MAX_RECORDS || byte_count > SHARD_TARGET_MAX_BYTES)
        && prefix.len() < SHARD_MAX_PREFIX_DEPTH
        && lines.len() > 1;
    if should_split {
        let mut children = BTreeMap::<u8, Vec<IndexLine>>::new();
        for line in lines {
            let next = *line
                .document_id
                .as_bytes()
                .get(prefix.len())
                .ok_or_else(|| ExportError::Invariant("short document ID".to_owned()))?;
            children.entry(next).or_default().push(line.clone());
        }
        for (next, child_lines) in children {
            let mut child_prefix = prefix.to_owned();
            child_prefix.push(char::from(next));
            build_shards(&child_prefix, base, &child_lines, desired, descriptors)?;
        }
        return Ok(());
    }

    let name = if prefix.is_empty() { "root" } else { prefix };
    let path = base.join("shards").join(format!("{name}.jsonl"));
    let mut bytes = Vec::with_capacity(byte_count);
    for line in lines {
        bytes.extend_from_slice(&line.bytes);
    }
    let first = lines
        .first()
        .ok_or_else(|| ExportError::Invariant("cannot write an empty shard".to_owned()))?;
    let last = lines
        .last()
        .ok_or_else(|| ExportError::Invariant("cannot write an empty shard".to_owned()))?;
    descriptors.push(ShardDescriptor {
        prefix: prefix.to_owned(),
        path: path_string(&path)?,
        record_count: lines.len(),
        byte_count: bytes.len(),
        sha256: sha256_prefixed(&bytes),
        min_document_id: first.document_id.clone(),
        max_document_id: last.document_id.clone(),
    });
    desired.insert(path, bytes);
    Ok(())
}

fn partition_base(partition: &str) -> Result<PathBuf, ExportError> {
    let (year, month) = partition
        .split_once('-')
        .ok_or_else(|| ExportError::Invariant(format!("invalid archive partition {partition}")))?;
    Ok(PathBuf::from("index")
        .join("v1")
        .join("current")
        .join("partitions")
        .join(year)
        .join(month))
}

fn sync_owned_files(root: &Path, desired: &BTreeMap<PathBuf, Vec<u8>>) -> Result<(), ExportError> {
    for owned in OWNED_PATHS {
        let path = root.join(owned);
        if path.is_dir() {
            for existing in files_under(&path)? {
                let relative = existing.strip_prefix(root).map_err(|_| {
                    ExportError::InvalidPath(format!(
                        "{} is outside {}",
                        existing.display(),
                        root.display()
                    ))
                })?;
                if !desired.contains_key(relative) {
                    fs::remove_file(existing)?;
                }
            }
        } else if path.is_file() && !desired.contains_key(Path::new(owned)) {
            fs::remove_file(path)?;
        }
    }
    for (relative, bytes) in desired {
        write_if_changed(&root.join(relative), bytes)?;
    }
    Ok(())
}

fn files_under(root: &Path) -> Result<Vec<PathBuf>, ExportError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<(), ExportError> {
    if fs::read(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        ExportError::InvalidPath(format!("{} has no parent directory", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(OsStr::to_str).unwrap_or("file")
    ));
    fs::write(&temporary, contents)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn pretty_json(value: &impl Serialize) -> Result<Vec<u8>, ExportError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn path_string(path: &Path) -> Result<String, ExportError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ExportError::InvalidPath(format!("{} is not UTF-8", path.display())))
}

fn path_string_lossless(path: &Path) -> &str {
    path.to_str()
        .expect("export paths are constructed from ASCII components")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

fn readme(record_count: usize, company_count: usize, generation: &str) -> String {
    format!(
        "# Company News Archive\n\n\
Version-controlled company news records generated by \
[company-feed-server](https://github.com/Shuozeli/company-feed-server).\n\n\
Current snapshot: **{record_count} records** across **{company_count} companies**.  \n\
Generation: `{generation}`\n\n\
Start with [`HEAD.json`](HEAD.json), then follow its root manifest to bounded \
JSONL shards. Browse readable articles under [`articles/v1/`](articles/v1/) \
and the machine contract under [`schemas/v1/`](schemas/v1/).\n\n\
- [`ARCHITECTURE.md`](ARCHITECTURE.md) explains identity, trees, shards, and compatibility.\n\
- [`openapi/openapi.json`](openapi/openapi.json) defines the OpenAPI 3.1 read contract.\n\
- [`CONTENT_RIGHTS.md`](CONTENT_RIGHTS.md) explains provenance and third-party rights.\n\
- `python3 scripts/validate_archive.py` verifies hashes and cross-references.\n"
    )
}

#[derive(Clone)]
struct IndexLine {
    document_id: String,
    bytes: Vec<u8>,
}

struct ArticleProjection<'a> {
    item: &'a ExportableFeedItem,
    document_id: String,
    archive_month: String,
    article_path: PathBuf,
    record_path: PathBuf,
}

struct CompanyAccumulator {
    company_key: String,
    company_name: String,
    record_count: usize,
    first_published_at: Option<DateTime<Utc>>,
    last_published_at: Option<DateTime<Utc>>,
    partitions: BTreeMap<String, usize>,
}

impl CompanyAccumulator {
    fn new(projection: &ArticleProjection<'_>) -> Self {
        Self {
            company_key: projection.item.company_key.clone(),
            company_name: projection.item.company_name.clone(),
            record_count: 0,
            first_published_at: None,
            last_published_at: None,
            partitions: BTreeMap::new(),
        }
    }

    fn record(&mut self, projection: &ArticleProjection<'_>) {
        self.record_count += 1;
        *self
            .partitions
            .entry(projection.archive_month.clone())
            .or_default() += 1;
        if let Some(published_at) = projection.item.item.published_at {
            self.first_published_at = Some(
                self.first_published_at
                    .map_or(published_at, |current| current.min(published_at)),
            );
            self.last_published_at = Some(
                self.last_published_at
                    .map_or(published_at, |current| current.max(published_at)),
            );
        }
    }

    fn manifest_path(&self) -> PathBuf {
        PathBuf::from("articles")
            .join("v1")
            .join(company_bucket(&self.company_key))
            .join(&self.company_key)
            .join("company.json")
    }

    fn manifest(&self) -> CompanyManifest<'_> {
        CompanyManifest {
            schema_version: SCHEMA_VERSION,
            company: CompanyRef {
                key: &self.company_key,
                name: &self.company_name,
            },
            record_count: self.record_count,
            first_published_at: self.first_published_at.map(|value| value.to_rfc3339()),
            last_published_at: self.last_published_at.map(|value| value.to_rfc3339()),
            partitions: self
                .partitions
                .iter()
                .map(|(partition, record_count)| CompanyPartition {
                    partition,
                    record_count: *record_count,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct CompanyRef<'a> {
    key: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct SourceRef<'a> {
    id: &'a str,
    kind: String,
}

#[derive(Serialize)]
struct UrlSet<'a> {
    observed: &'a str,
    canonical: &'a str,
}

#[derive(Serialize)]
struct ArchivePaths<'a> {
    article: &'a str,
    record: &'a str,
}

#[derive(Serialize)]
struct ContentDescriptor<'a> {
    media_type: &'a str,
    bytes: usize,
    sha256: &'a str,
    normalized_content_hash: &'a str,
}

#[derive(Serialize)]
struct Provenance<'a> {
    source_item_id: String,
    external_id: &'a str,
}

#[derive(Serialize)]
struct ArticleRecord<'a> {
    schema_version: &'a str,
    document_id: &'a str,
    company: CompanyRef<'a>,
    source: SourceRef<'a>,
    urls: UrlSet<'a>,
    title: &'a str,
    summary: &'a str,
    published_at: Option<String>,
    first_seen_at: String,
    fetched_at: String,
    last_updated_at: String,
    paths: ArchivePaths<'a>,
    content: ContentDescriptor<'a>,
    provenance: Provenance<'a>,
}

#[derive(Serialize)]
struct IndexDocument<'a> {
    schema_version: &'a str,
    document_id: &'a str,
    company_key: &'a str,
    company_name: &'a str,
    source_id: &'a str,
    source_kind: String,
    canonical_url: &'a str,
    observed_url: &'a str,
    title: &'a str,
    summary: &'a str,
    body_text: &'a str,
    published_at: Option<String>,
    first_seen_at: String,
    fetched_at: String,
    last_updated_at: String,
    content_hash: &'a str,
    article_path: &'a str,
    record_path: &'a str,
}

#[derive(Serialize)]
struct CompanyPartition<'a> {
    partition: &'a str,
    record_count: usize,
}

#[derive(Serialize)]
struct CompanyManifest<'a> {
    schema_version: &'a str,
    company: CompanyRef<'a>,
    record_count: usize,
    first_published_at: Option<String>,
    last_published_at: Option<String>,
    partitions: Vec<CompanyPartition<'a>>,
}

#[derive(Serialize)]
struct ShardDescriptor {
    prefix: String,
    path: String,
    record_count: usize,
    byte_count: usize,
    sha256: String,
    min_document_id: String,
    max_document_id: String,
}

#[derive(Serialize)]
struct PartitionManifest<'a> {
    schema_version: &'a str,
    generation: &'a str,
    partition: &'a str,
    record_count: usize,
    shard_count: usize,
    byte_count: usize,
    shards: Vec<ShardDescriptor>,
}

#[derive(Serialize)]
struct PartitionDescriptor {
    partition: String,
    manifest_path: String,
    record_count: usize,
    shard_count: usize,
    byte_count: usize,
    sha256: String,
}

#[derive(Serialize)]
struct PartitioningPolicy<'a> {
    primary: &'a str,
    secondary: &'a str,
    max_records_per_shard: usize,
    target_max_bytes_per_shard: usize,
    max_hash_prefix_depth: usize,
}

#[derive(Serialize)]
struct ArchiveManifest<'a> {
    schema_version: &'a str,
    generation: &'a str,
    generated_at: &'a str,
    record_count: usize,
    company_count: usize,
    first_published_at: Option<&'a str>,
    last_published_at: Option<&'a str>,
    partitioning: PartitioningPolicy<'a>,
    partitions: Vec<PartitionDescriptor>,
}

#[derive(Serialize)]
struct ArchiveHead<'a> {
    schema_version: &'a str,
    generation: &'a str,
    generated_at: &'a str,
    manifest_path: &'a str,
    record_count: usize,
    company_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_trie_splits_by_successive_hex_characters() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let base = PathBuf::from("index/v1/current/partitions/2026/07");
        let mut lines = (0..=SHARD_MAX_RECORDS)
            .map(|index| {
                let document_id = format!("{:x}{index:063x}", index % 16);
                IndexLine {
                    document_id: document_id.clone(),
                    bytes: format!("{{\"document_id\":\"{document_id}\"}}\n").into_bytes(),
                }
            })
            .collect::<Vec<_>>();
        lines.sort_by(|left, right| left.document_id.cmp(&right.document_id));
        let mut desired = BTreeMap::new();
        let mut descriptors = Vec::new();
        build_shards("", &base, &lines, &mut desired, &mut descriptors)
            .expect("shards materialize");
        assert!(descriptors.len() > 1);
        assert!(
            descriptors
                .iter()
                .all(|descriptor| !descriptor.prefix.is_empty())
        );
        assert_eq!(
            descriptors
                .iter()
                .map(|descriptor| descriptor.record_count)
                .sum::<usize>(),
            lines.len()
        );
        assert!(!temporary.path().join("unused").exists());
    }

    #[test]
    fn path_safety_rejects_parent_segments() {
        assert!(is_safe_owned_relative_path(Path::new(
            "articles/v1/aa/acme/2026/07/bb/id/article.md"
        )));
        assert!(!is_safe_owned_relative_path(Path::new("../article.md")));
        assert!(!is_safe_owned_relative_path(Path::new("/article.md")));
    }
}
