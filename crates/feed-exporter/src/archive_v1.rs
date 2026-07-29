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
const DATA_INDEX_VERSION: &str = "1.1.0";
const SHARD_MAX_RECORDS: usize = 5_000;
const SHARD_TARGET_MAX_BYTES: usize = 1024 * 1024;
const SHARD_MAX_PREFIX_DEPTH: usize = 8;
const BROWSE_PAGE_SIZE: usize = 50;
const CATEGORY_PAGE_SIZE: usize = 100;

pub(super) const OWNED_PATHS: [&str; 17] = [
    ".github",
    "ARCHITECTURE.md",
    "CONTRIBUTING.md",
    "CONTENT_RIGHTS.md",
    "HEAD.json",
    "index.json",
    "LICENSE.md",
    "README.md",
    "SECURITY.md",
    "articles",
    "companies",
    "feeds",
    "index",
    "indexes",
    "openapi",
    "schemas",
    "scripts",
];

const STATIC_FILES: &[(&str, &[u8])] = &[
    (
        ".github/workflows/validate.yml",
        include_bytes!("../assets/github/workflows/validate.yml"),
    ),
    (
        "ARCHITECTURE.md",
        include_bytes!("../assets/ARCHITECTURE.md"),
    ),
    (
        "CONTRIBUTING.md",
        include_bytes!("../assets/CONTRIBUTING.md"),
    ),
    (
        "CONTENT_RIGHTS.md",
        include_bytes!("../assets/CONTENT_RIGHTS.md"),
    ),
    ("LICENSE.md", include_bytes!("../assets/LICENSE.md")),
    ("SECURITY.md", include_bytes!("../assets/SECURITY.md")),
    (
        ".github/ISSUE_TEMPLATE/archive-bug.yml",
        include_bytes!("../assets/github/ISSUE_TEMPLATE/archive-bug.yml"),
    ),
    (
        ".github/ISSUE_TEMPLATE/config.yml",
        include_bytes!("../assets/github/ISSUE_TEMPLATE/config.yml"),
    ),
    (
        ".github/ISSUE_TEMPLATE/data-correction.yml",
        include_bytes!("../assets/github/ISSUE_TEMPLATE/data-correction.yml"),
    ),
    (
        ".github/ISSUE_TEMPLATE/rights.yml",
        include_bytes!("../assets/github/ISSUE_TEMPLATE/rights.yml"),
    ),
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
        "schemas/v1/data-index.schema.json",
        include_bytes!("../assets/schemas/v1/data-index.schema.json"),
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
        "schemas/v1/article-summary.schema.json",
        include_bytes!("../assets/schemas/v1/article-summary.schema.json"),
    ),
    (
        "schemas/v1/browse-page.schema.json",
        include_bytes!("../assets/schemas/v1/browse-page.schema.json"),
    ),
    (
        "schemas/v1/recent-manifest.schema.json",
        include_bytes!("../assets/schemas/v1/recent-manifest.schema.json"),
    ),
    (
        "schemas/v1/company-directory-manifest.schema.json",
        include_bytes!("../assets/schemas/v1/company-directory-manifest.schema.json"),
    ),
    (
        "schemas/v1/company-directory-bucket.schema.json",
        include_bytes!("../assets/schemas/v1/company-directory-bucket.schema.json"),
    ),
    (
        "schemas/v1/category-directory-manifest.schema.json",
        include_bytes!("../assets/schemas/v1/category-directory-manifest.schema.json"),
    ),
    (
        "schemas/v1/category-directory-page.schema.json",
        include_bytes!("../assets/schemas/v1/category-directory-page.schema.json"),
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
        .iter()
        .map(|(path, bytes)| (PathBuf::from(*path), bytes.to_vec()))
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

    for (projection_index, projection) in projections.iter().enumerate() {
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
            .record(projection, projection_index)?;

        exported.push(ExportedItem {
            feed_item_id: projection.item.item.id,
            exported_path: projection.article_path.clone(),
            exported_content_hash: projection.item.item.content_hash.clone(),
        });
    }

    let taxonomy_generation = taxonomy_generation_id(&company_accumulators);
    let mut company_directory = BTreeMap::<String, Vec<CompanyDirectoryEntry>>::new();
    let mut category_directory = BTreeMap::<String, CategoryDirectoryAccumulator>::new();
    for accumulator in company_accumulators.values() {
        let mut projection_indexes = accumulator.projection_indexes.clone();
        sort_projection_indexes_newest_first(&mut projection_indexes, &projections);
        let pages = materialize_browse_pages(
            &accumulator.article_index_base(),
            &projection_indexes,
            &projections,
            &generation,
            &mut desired,
        )?;
        desired.insert(
            accumulator.manifest_path(),
            pretty_json(&accumulator.manifest(&generation, pages))?,
        );
        let directory_entry = accumulator.directory_entry();
        company_directory
            .entry(company_directory_bucket(&accumulator.company_name))
            .or_default()
            .push(directory_entry.clone());
        category_directory
            .entry(accumulator.company_category_key.clone())
            .or_insert_with(|| CategoryDirectoryAccumulator {
                name: accumulator.company_category_name.clone(),
                record_count: 0,
                companies: Vec::new(),
            })
            .record(accumulator, directory_entry)?;
    }
    let company_directory_manifest_path =
        materialize_company_directory(&generation, company_directory, &mut desired)?;
    let category_directory_manifest_path = materialize_category_directory(
        &generation,
        &taxonomy_generation,
        category_directory,
        &mut desired,
    )?;

    let mut recent_projection_indexes = (0..projections.len()).collect::<Vec<_>>();
    sort_projection_indexes_newest_first(&mut recent_projection_indexes, &projections);
    let recent_base = PathBuf::from("index/v1/current/recent");
    let recent_pages = materialize_browse_pages(
        &recent_base,
        &recent_projection_indexes,
        &projections,
        &generation,
        &mut desired,
    )?;
    let recent_manifest_path = recent_base.join("manifest.json");
    let recent_manifest = RecentManifest {
        schema_version: SCHEMA_VERSION,
        generation: &generation,
        sort: "published_at_desc_fetched_at_desc",
        page_size: BROWSE_PAGE_SIZE,
        record_count: projections.len(),
        page_count: recent_pages.len(),
        pages: recent_pages,
    };
    desired.insert(recent_manifest_path.clone(), pretty_json(&recent_manifest)?);

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
    let archive_manifest_path = path_string(&manifest_path)?;
    let recent_manifest_path_string = path_string(&recent_manifest_path)?;
    let company_directory_manifest_path_string = path_string(&company_directory_manifest_path)?;
    let category_directory_manifest_path_string = path_string(&category_directory_manifest_path)?;
    let data_index = DataIndex {
        contract_version: DATA_INDEX_VERSION,
        dataset: "company-news-data",
        schema_version: SCHEMA_VERSION,
        generation: &generation,
        taxonomy_generation: &taxonomy_generation,
        generated_at: &generated_at,
        record_count: projections.len(),
        company_count,
        paths: DataIndexPaths {
            head: "HEAD.json",
            archive_manifest: &archive_manifest_path,
            recent_manifest: &recent_manifest_path_string,
            company_directory_manifest: &company_directory_manifest_path_string,
            category_directory_manifest: &category_directory_manifest_path_string,
            openapi: "openapi/openapi.json",
            content_rights: "CONTENT_RIGHTS.md",
        },
    };
    desired.insert(PathBuf::from("index.json"), pretty_json(&data_index)?);
    desired.insert(
        PathBuf::from("README.md"),
        readme(
            projections.len(),
            company_count,
            &generation,
            &taxonomy_generation,
            &generated_at,
        )
        .into_bytes(),
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
    // This namespace is part of the durable article identity contract. It
    // intentionally retains the original repository name after the public
    // rename so existing document IDs and article paths remain stable.
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

fn is_safe_category_key(value: &str) -> bool {
    if value == "uncategorized" {
        return true;
    }
    let Some((slug, digest)) = value.rsplit_once('-') else {
        return false;
    };
    value.len() <= 64
        && is_safe_company_key(value)
        && !slug.is_empty()
        && digest.len() == 16
        && digest
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
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
    // Keep the original namespace stable for deterministic generations.
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

fn taxonomy_generation_id(companies: &BTreeMap<String, CompanyAccumulator>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"company-news-data/taxonomy/v1\0");
    for company in companies.values() {
        hasher.update(company.company_key.as_bytes());
        hasher.update(b"\0");
        hasher.update(company.company_category_key.as_bytes());
        hasher.update(b"\0");
        hasher.update(company.company_category_name.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
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

fn article_summary<'a>(projection: &'a ArticleProjection<'a>) -> ArticleSummary<'a> {
    let item = &projection.item.item;
    ArticleSummary {
        schema_version: SCHEMA_VERSION,
        document_id: &projection.document_id,
        company_key: &projection.item.company_key,
        company_name: &projection.item.company_name,
        source_id: &projection.item.source_key,
        source_kind: item.source_kind.to_string(),
        canonical_url: item.canonical_url.as_str(),
        title: &item.title,
        summary: &item.summary,
        published_at: item.published_at.map(|value| value.to_rfc3339()),
        fetched_at: item.fetched_at.to_rfc3339(),
        article_path: path_string_lossless(&projection.article_path),
        record_path: path_string_lossless(&projection.record_path),
    }
}

fn effective_article_timestamp(projection: &ArticleProjection<'_>) -> DateTime<Utc> {
    projection
        .item
        .item
        .published_at
        .unwrap_or(projection.item.item.fetched_at)
}

fn sort_projection_indexes_newest_first(
    indexes: &mut [usize],
    projections: &[ArticleProjection<'_>],
) {
    indexes.sort_by(|left, right| {
        let left = &projections[*left];
        let right = &projections[*right];
        effective_article_timestamp(right)
            .cmp(&effective_article_timestamp(left))
            .then_with(|| right.item.item.fetched_at.cmp(&left.item.item.fetched_at))
            .then_with(|| left.document_id.cmp(&right.document_id))
    });
}

fn materialize_browse_pages(
    base: &Path,
    projection_indexes: &[usize],
    projections: &[ArticleProjection<'_>],
    generation: &str,
    desired: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Vec<PageDescriptor>, ExportError> {
    let mut pages = Vec::new();
    for (page_index, chunk) in projection_indexes.chunks(BROWSE_PAGE_SIZE).enumerate() {
        let page_number = page_index + 1;
        let path = base.join("pages").join(format!("{page_number:06}.json"));
        let page = BrowsePage {
            schema_version: SCHEMA_VERSION,
            generation,
            page: page_number,
            record_count: chunk.len(),
            items: chunk
                .iter()
                .map(|index| article_summary(&projections[*index]))
                .collect(),
        };
        let bytes = pretty_json(&page)?;
        let newest_at = chunk
            .first()
            .map(|index| effective_article_timestamp(&projections[*index]).to_rfc3339());
        let oldest_at = chunk
            .last()
            .map(|index| effective_article_timestamp(&projections[*index]).to_rfc3339());
        pages.push(PageDescriptor {
            page: page_number,
            path: path_string(&path)?,
            record_count: chunk.len(),
            byte_count: bytes.len(),
            sha256: sha256_prefixed(&bytes),
            newest_at,
            oldest_at,
        });
        desired.insert(path, bytes);
    }
    Ok(pages)
}

fn company_directory_bucket(company_name: &str) -> String {
    match company_name.trim_start().chars().next() {
        Some(character) if character.is_ascii_alphabetic() => {
            character.to_ascii_lowercase().to_string()
        }
        Some(character) if character.is_ascii_digit() => "0-9".to_owned(),
        _ => "other".to_owned(),
    }
}

fn materialize_company_directory(
    generation: &str,
    mut buckets: BTreeMap<String, Vec<CompanyDirectoryEntry>>,
    desired: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<PathBuf, ExportError> {
    let base = PathBuf::from("index/v1/current/companies");
    let mut descriptors = Vec::with_capacity(buckets.len());
    let mut company_count = 0;
    for (bucket, entries) in &mut buckets {
        entries.sort_by(|left, right| {
            left.company_name
                .to_lowercase()
                .cmp(&right.company_name.to_lowercase())
                .then_with(|| left.company_key.cmp(&right.company_key))
        });
        company_count += entries.len();
        let path = base.join("buckets").join(format!("{bucket}.json"));
        let page = CompanyDirectoryBucket {
            schema_version: SCHEMA_VERSION,
            generation,
            bucket,
            company_count: entries.len(),
            companies: entries,
        };
        let bytes = pretty_json(&page)?;
        descriptors.push(CompanyDirectoryBucketDescriptor {
            bucket: bucket.clone(),
            path: path_string(&path)?,
            company_count: entries.len(),
            byte_count: bytes.len(),
            sha256: sha256_prefixed(&bytes),
        });
        desired.insert(path, bytes);
    }
    let manifest_path = base.join("manifest.json");
    let manifest = CompanyDirectoryManifest {
        schema_version: SCHEMA_VERSION,
        generation,
        company_count,
        bucket_count: descriptors.len(),
        buckets: descriptors,
    };
    desired.insert(manifest_path.clone(), pretty_json(&manifest)?);
    Ok(manifest_path)
}

fn materialize_category_directory(
    generation: &str,
    taxonomy_generation: &str,
    mut categories: BTreeMap<String, CategoryDirectoryAccumulator>,
    desired: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<PathBuf, ExportError> {
    let base = PathBuf::from("index/v1/current/categories");
    let mut descriptors = Vec::with_capacity(categories.len());
    let mut company_count = 0;
    let mut record_count = 0;
    for (key, category) in &mut categories {
        if !is_safe_category_key(key) {
            return Err(ExportError::InvalidPath(format!(
                "category key {key} cannot form an archive path"
            )));
        }
        category.companies.sort_by(|left, right| {
            left.company_name
                .to_lowercase()
                .cmp(&right.company_name.to_lowercase())
                .then_with(|| left.company_key.cmp(&right.company_key))
        });
        company_count += category.companies.len();
        record_count += category.record_count;
        let mut pages = Vec::new();
        for (page_index, companies) in category.companies.chunks(CATEGORY_PAGE_SIZE).enumerate() {
            let page_number = page_index + 1;
            let path = base
                .join(key)
                .join("pages")
                .join(format!("{page_number:06}.json"));
            let page_record_count = companies.iter().map(|company| company.record_count).sum();
            let page = CategoryDirectoryPage {
                schema_version: SCHEMA_VERSION,
                generation,
                taxonomy_generation,
                key,
                name: &category.name,
                page: page_number,
                company_count: companies.len(),
                record_count: page_record_count,
                companies,
            };
            let bytes = pretty_json(&page)?;
            pages.push(CategoryDirectoryPageDescriptor {
                page: page_number,
                path: path_string(&path)?,
                company_count: companies.len(),
                record_count: page_record_count,
                byte_count: bytes.len(),
                sha256: sha256_prefixed(&bytes),
            });
            desired.insert(path, bytes);
        }
        descriptors.push(CategoryDirectoryDescriptor {
            key: key.clone(),
            name: category.name.clone(),
            company_count: category.companies.len(),
            record_count: category.record_count,
            page_count: pages.len(),
            pages,
        });
    }
    let manifest_path = base.join("manifest.json");
    let manifest = CategoryDirectoryManifest {
        schema_version: SCHEMA_VERSION,
        generation,
        taxonomy_generation,
        company_count,
        record_count,
        category_count: descriptors.len(),
        page_size: CATEGORY_PAGE_SIZE,
        categories: descriptors,
    };
    desired.insert(manifest_path.clone(), pretty_json(&manifest)?);
    Ok(manifest_path)
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

fn readme(
    record_count: usize,
    company_count: usize,
    generation: &str,
    taxonomy_generation: &str,
    generated_at: &str,
) -> String {
    format!(
        r#"# Company News Data

[![Validate archive](https://github.com/Shuozeli/company-news-data/actions/workflows/validate.yml/badge.svg?branch=main)](https://github.com/Shuozeli/company-news-data/actions/workflows/validate.yml)

**[Open the live reader](https://shuozeli.github.io/company-news-ui/)** — it
loads one bounded index page at a time and fetches article text only when an
article is opened.

Company News Data is a version-controlled publication artifact generated by
[`company-feed-server`](https://github.com/Shuozeli/company-feed-server). It
contains company-news metadata, normalized article text, lazy browser indexes,
full-text JSONL shards, and machine-readable contracts.

## Current snapshot

| Field | Value |
| --- | --- |
| Generated at | `{generated_at}` |
| Records | **{record_count}** |
| Companies | **{company_count}** |
| Browser contract | `{DATA_INDEX_VERSION}` |
| Article schema | `{SCHEMA_VERSION}` |
| Generation | `{generation}` |
| Taxonomy generation | `{taxonomy_generation}` |

`main` is the current mutable snapshot. Use both generation values to detect
mixed files and verify hashes from parent manifests.

> [!WARNING]
> Do not start with a normal Git clone. Generated snapshots can contain hundreds
> of thousands of files and a multi-gigabyte checkout. Use the live reader or
> raw lazy indexes. Bulk consumers should fetch only the month partitions or
> company trees they need.

## Read one article

Start with the small `index.json` bootstrap and follow paths declared by that
same snapshot:

```bash
BASE="https://raw.githubusercontent.com/Shuozeli/company-news-data/main/"

INDEX="$(curl -fsSL "${{BASE}}index.json")"
RECENT="$(printf '%s' "$INDEX" | jq -r '.paths.recent_manifest')"
PAGE="$(
  curl -fsSL "${{BASE}}${{RECENT}}" |
    jq -r '.pages[0].path'
)"
ITEM="$(
  curl -fsSL "${{BASE}}${{PAGE}}" |
    jq -c '.items[0]'
)"

printf '%s\n' "$ITEM" | jq '{{company_name, title, published_at, canonical_url}}'
ARTICLE="$(printf '%s' "$ITEM" | jq -r '.article_path')"
curl -fsSL "${{BASE}}${{ARTICLE}}"
```

For category-first navigation, follow
`index.json -> paths.category_directory_manifest -> category.pages[].path`.
Category pages contain at most 100 companies. Each company manifest points to
50-summary article pages. Full article Markdown loads only after selection.

## Scope and methodology

The maintained universe includes public and private companies. Stable
`company_key` values, not stock tickers, identify companies. Upstream display
names may include security-class suffixes and are not guaranteed to be unique
legal names.

The source service discovers operator-reviewed RSS, Atom, HTML, and browser
sources, normalizes observations, and hydrates article content when available.
`document_id` is deterministic over the schema namespace, company key, and
normalized canonical URL. Records preserve publisher, observation, fetch, and
update timestamps when available.

The current target uses `metadata.publication_scope=approved_public`, which can
select non-private items from approved, currently valid sources even when their
source-level `public_export_allowed` flag is false. “Approved” is a pipeline
quality/status decision, not a copyright license, redistribution permission, or
publisher endorsement.

## Known limitations

- Some records lack a publisher date, summary, or extracted body.
- Extracted Markdown can retain boilerplate or lose layouts, images,
  interactive media, and script-rendered content.
- Websites, URLs, and recipes change; historical success does not guarantee
  current reachability.
- Missing sector metadata is published under `Uncategorized`.
- The static tree provides bounded navigation, not server-side search.
- Third-party article text is not CC0 and is not relicensed here.

Use the
[data-correction form](https://github.com/Shuozeli/company-news-data/issues/new?template=data-correction.yml)
for identity, category, URL, provenance, duplicate, or extraction problems.
Use the separate
[rights and removal form](https://github.com/Shuozeli/company-news-data/issues/new?template=rights.yml)
for copyright, permission, attribution, or removal concerns.

## Contracts, validation, and contribution

[`ARCHITECTURE.md`](ARCHITECTURE.md) explains identities, trees, shards,
checkpoints, compatibility, and the Git scale boundary. JSON Schemas use Draft
2020-12 under [`schemas/v1/`](schemas/v1/), and
[`openapi/openapi.json`](openapi/openapi.json) describes the OpenAPI 3.1 static
read contract.

Full-mirror maintainers can run:

```bash
python3 scripts/validate_archive.py
```

The validator checks hashes, counts, ordering, identities, bounded indexes, and
record/article cross-references. Lightweight clients should verify only the
manifests and shards they consume.

The CC0 waiver in [`LICENSE.md`](LICENSE.md) applies only to
repository-authored schemas, manifests, validation code, and documentation.
Read [`CONTENT_RIGHTS.md`](CONTENT_RIGHTS.md) before reusing publisher material.
This repository is generated; route durable changes through
[`CONTRIBUTING.md`](CONTRIBUTING.md) and report vulnerabilities as described in
[`SECURITY.md`](SECURITY.md).
"#
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
    company_category_key: String,
    company_category_name: String,
    record_count: usize,
    first_published_at: Option<DateTime<Utc>>,
    last_published_at: Option<DateTime<Utc>>,
    partitions: BTreeMap<String, usize>,
    projection_indexes: Vec<usize>,
}

impl CompanyAccumulator {
    fn new(projection: &ArticleProjection<'_>) -> Self {
        Self {
            company_key: projection.item.company_key.clone(),
            company_name: projection.item.company_name.clone(),
            company_category_key: projection.item.company_category_key.clone(),
            company_category_name: projection.item.company_category_name.clone(),
            record_count: 0,
            first_published_at: None,
            last_published_at: None,
            partitions: BTreeMap::new(),
            projection_indexes: Vec::new(),
        }
    }

    fn record(
        &mut self,
        projection: &ArticleProjection<'_>,
        projection_index: usize,
    ) -> Result<(), ExportError> {
        if self.company_category_key != projection.item.company_category_key
            || self.company_category_name != projection.item.company_category_name
        {
            return Err(ExportError::Invariant(format!(
                "company {} has inconsistent category assignments",
                self.company_key
            )));
        }
        self.record_count += 1;
        self.projection_indexes.push(projection_index);
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
        Ok(())
    }

    fn manifest_path(&self) -> PathBuf {
        PathBuf::from("articles")
            .join("v1")
            .join(company_bucket(&self.company_key))
            .join(&self.company_key)
            .join("company.json")
    }

    fn article_index_base(&self) -> PathBuf {
        self.manifest_path()
            .parent()
            .expect("company manifest has a parent")
            .join("index")
    }

    fn manifest<'a>(
        &'a self,
        generation: &'a str,
        pages: Vec<PageDescriptor>,
    ) -> CompanyManifest<'a> {
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
            article_index: CompanyArticleIndex {
                generation,
                sort: "published_at_desc_fetched_at_desc",
                page_size: BROWSE_PAGE_SIZE,
                page_count: pages.len(),
                pages,
            },
        }
    }

    fn directory_entry(&self) -> CompanyDirectoryEntry {
        CompanyDirectoryEntry {
            company_key: self.company_key.clone(),
            company_name: self.company_name.clone(),
            record_count: self.record_count,
            first_published_at: self.first_published_at.map(|value| value.to_rfc3339()),
            last_published_at: self.last_published_at.map(|value| value.to_rfc3339()),
            company_manifest_path: path_string_lossless(&self.manifest_path()).to_owned(),
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
struct ArticleSummary<'a> {
    schema_version: &'a str,
    document_id: &'a str,
    company_key: &'a str,
    company_name: &'a str,
    source_id: &'a str,
    source_kind: String,
    canonical_url: &'a str,
    title: &'a str,
    summary: &'a str,
    published_at: Option<String>,
    fetched_at: String,
    article_path: &'a str,
    record_path: &'a str,
}

#[derive(Serialize)]
struct BrowsePage<'a> {
    schema_version: &'a str,
    generation: &'a str,
    page: usize,
    record_count: usize,
    items: Vec<ArticleSummary<'a>>,
}

#[derive(Serialize)]
struct PageDescriptor {
    page: usize,
    path: String,
    record_count: usize,
    byte_count: usize,
    sha256: String,
    newest_at: Option<String>,
    oldest_at: Option<String>,
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
    article_index: CompanyArticleIndex<'a>,
}

#[derive(Serialize)]
struct CompanyArticleIndex<'a> {
    generation: &'a str,
    sort: &'a str,
    page_size: usize,
    page_count: usize,
    pages: Vec<PageDescriptor>,
}

#[derive(Clone, Serialize)]
struct CompanyDirectoryEntry {
    company_key: String,
    company_name: String,
    record_count: usize,
    first_published_at: Option<String>,
    last_published_at: Option<String>,
    company_manifest_path: String,
}

#[derive(Serialize)]
struct CompanyDirectoryBucket<'a> {
    schema_version: &'a str,
    generation: &'a str,
    bucket: &'a str,
    company_count: usize,
    companies: &'a [CompanyDirectoryEntry],
}

#[derive(Serialize)]
struct CompanyDirectoryBucketDescriptor {
    bucket: String,
    path: String,
    company_count: usize,
    byte_count: usize,
    sha256: String,
}

#[derive(Serialize)]
struct CompanyDirectoryManifest<'a> {
    schema_version: &'a str,
    generation: &'a str,
    company_count: usize,
    bucket_count: usize,
    buckets: Vec<CompanyDirectoryBucketDescriptor>,
}

struct CategoryDirectoryAccumulator {
    name: String,
    record_count: usize,
    companies: Vec<CompanyDirectoryEntry>,
}

impl CategoryDirectoryAccumulator {
    fn record(
        &mut self,
        company: &CompanyAccumulator,
        directory_entry: CompanyDirectoryEntry,
    ) -> Result<(), ExportError> {
        if self.name != company.company_category_name {
            return Err(ExportError::Invariant(format!(
                "category key {} maps to both {} and {}",
                company.company_category_key, self.name, company.company_category_name
            )));
        }
        self.record_count += company.record_count;
        self.companies.push(directory_entry);
        Ok(())
    }
}

#[derive(Serialize)]
struct CategoryDirectoryDescriptor {
    key: String,
    name: String,
    company_count: usize,
    record_count: usize,
    page_count: usize,
    pages: Vec<CategoryDirectoryPageDescriptor>,
}

#[derive(Serialize)]
struct CategoryDirectoryPageDescriptor {
    page: usize,
    path: String,
    company_count: usize,
    record_count: usize,
    byte_count: usize,
    sha256: String,
}

#[derive(Serialize)]
struct CategoryDirectoryPage<'a> {
    schema_version: &'a str,
    generation: &'a str,
    taxonomy_generation: &'a str,
    key: &'a str,
    name: &'a str,
    page: usize,
    company_count: usize,
    record_count: usize,
    companies: &'a [CompanyDirectoryEntry],
}

#[derive(Serialize)]
struct CategoryDirectoryManifest<'a> {
    schema_version: &'a str,
    generation: &'a str,
    taxonomy_generation: &'a str,
    company_count: usize,
    record_count: usize,
    category_count: usize,
    page_size: usize,
    categories: Vec<CategoryDirectoryDescriptor>,
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

#[derive(Serialize)]
struct RecentManifest<'a> {
    schema_version: &'a str,
    generation: &'a str,
    sort: &'a str,
    page_size: usize,
    record_count: usize,
    page_count: usize,
    pages: Vec<PageDescriptor>,
}

#[derive(Serialize)]
struct DataIndexPaths<'a> {
    head: &'a str,
    archive_manifest: &'a str,
    recent_manifest: &'a str,
    company_directory_manifest: &'a str,
    category_directory_manifest: &'a str,
    openapi: &'a str,
    content_rights: &'a str,
}

#[derive(Serialize)]
struct DataIndex<'a> {
    contract_version: &'a str,
    dataset: &'a str,
    schema_version: &'a str,
    generation: &'a str,
    taxonomy_generation: &'a str,
    generated_at: &'a str,
    record_count: usize,
    company_count: usize,
    paths: DataIndexPaths<'a>,
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
