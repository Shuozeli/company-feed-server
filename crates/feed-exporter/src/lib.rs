use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

use chrono::{DateTime, NaiveDate, Utc};
use feed_core::{
    ExportFormat, ExportLayout, ExportTarget, ExportableFeedItem, ExportedItem, FeedItem,
};
use serde::Serialize;

const OWNED_PATHS: [&str; 4] = ["README.md", "companies", "feeds", "indexes"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportResult {
    pub records: Vec<ExportedItem>,
    pub commit_sha: Option<String>,
    pub pushed: bool,
    pub changed: bool,
}

pub async fn export_archive(
    target: ExportTarget,
    items: Vec<ExportableFeedItem>,
) -> Result<ExportResult, ExportError> {
    tokio::task::spawn_blocking(move || export_archive_sync(&target, &items))
        .await
        .map_err(ExportError::Join)?
}

fn export_archive_sync(
    target: &ExportTarget,
    items: &[ExportableFeedItem],
) -> Result<ExportResult, ExportError> {
    if target.layout != ExportLayout::ByCompanyDate {
        return Err(ExportError::UnsupportedLayout(target.layout.to_string()));
    }
    fs::create_dir_all(&target.local_path)?;
    initialize_repository(target)?;

    let mut public_items = items.iter().map(PublicFeedItem::from).collect::<Vec<_>>();
    public_items.sort_by(|left, right| {
        right
            .sort_timestamp()
            .cmp(left.sort_timestamp())
            .then_with(|| left.id.cmp(&right.id))
    });

    let records = match target.format {
        ExportFormat::MarkdownJson => {
            materialize_markdown_json(&target.local_path, items, &public_items)?
        }
        ExportFormat::Jsonl => materialize_jsonl_records(items),
    };
    materialize_shared_files(&target.local_path, &public_items)?;

    let changed = repository_has_owned_changes(&target.local_path)?;
    if changed {
        git(
            &target.local_path,
            ["add", "--all", "--"].into_iter().chain(OWNED_PATHS),
        )?;
        let message = format!(
            "company-feed export {}",
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        git(&target.local_path, ["commit", "-m", &message])?;
    }

    let commit_sha = git_optional(&target.local_path, ["rev-parse", "HEAD"])?
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let pushed = if target.push_enabled {
        let Some(_) = commit_sha else {
            return Err(ExportError::Git(
                "cannot push an archive without a commit".to_owned(),
            ));
        };
        git(
            &target.local_path,
            [
                "push",
                "--set-upstream",
                "origin",
                &format!("HEAD:refs/heads/{}", target.branch),
            ],
        )?;
        true
    } else {
        false
    };

    Ok(ExportResult {
        records,
        commit_sha,
        pushed,
        changed,
    })
}

fn initialize_repository(target: &ExportTarget) -> Result<(), ExportError> {
    let root = &target.local_path;
    if !root.join(".git").is_dir() {
        git(root, ["init", "--initial-branch", &target.branch])?;
    }

    let current_branch = git_optional(root, ["symbolic-ref", "--short", "HEAD"])?
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| target.branch.clone());
    if current_branch != target.branch {
        return Err(ExportError::Git(format!(
            "archive is on branch {current_branch}, expected {}",
            target.branch
        )));
    }

    if !git_optional(root, ["config", "--get", "user.name"])?
        .is_some_and(|output| output.status.success())
    {
        git(root, ["config", "user.name", "Company Feed Exporter"])?;
    }
    if !git_optional(root, ["config", "--get", "user.email"])?
        .is_some_and(|output| output.status.success())
    {
        git(
            root,
            ["config", "user.email", "company-feed-exporter@localhost"],
        )?;
    }

    match git_optional(root, ["remote", "get-url", "origin"])? {
        Some(output) if output.status.success() => {
            let existing = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if existing != target.repo_url {
                return Err(ExportError::Git(format!(
                    "origin is {existing}, expected {}",
                    target.repo_url
                )));
            }
        }
        _ => {
            git(root, ["remote", "add", "origin", &target.repo_url])?;
        }
    }
    Ok(())
}

fn materialize_markdown_json(
    root: &Path,
    items: &[ExportableFeedItem],
    public_items: &[PublicFeedItem],
) -> Result<Vec<ExportedItem>, ExportError> {
    let public_by_id = public_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let mut records = Vec::with_capacity(items.len());

    for exportable in items {
        let item = public_by_id
            .get(exportable.item.id.to_string().as_str())
            .copied()
            .ok_or_else(|| {
                ExportError::Invariant(format!(
                    "public export projection missing item {}",
                    exportable.item.id
                ))
            })?;
        let relative_markdown = stable_item_path(exportable)?;
        let relative_json = relative_markdown.with_extension("json");

        if let Some(previous) = exportable.previous_exported_path.as_deref()
            && previous != relative_markdown
            && is_safe_owned_relative_path(previous)
        {
            remove_if_exists(&root.join(previous))?;
            remove_if_exists(&root.join(previous).with_extension("json"))?;
        }

        write_if_changed(
            &root.join(&relative_markdown),
            markdown_document(item).as_bytes(),
        )?;
        write_json(&root.join(&relative_json), item)?;
        records.push(ExportedItem {
            feed_item_id: exportable.item.id,
            exported_path: relative_markdown,
            exported_content_hash: exportable.item.content_hash.clone(),
        });
    }
    Ok(records)
}

fn materialize_jsonl_records(items: &[ExportableFeedItem]) -> Vec<ExportedItem> {
    items
        .iter()
        .map(|item| ExportedItem {
            feed_item_id: item.item.id,
            exported_path: PathBuf::from("feeds/latest.jsonl"),
            exported_content_hash: item.item.content_hash.clone(),
        })
        .collect()
}

fn materialize_shared_files(root: &Path, items: &[PublicFeedItem]) -> Result<(), ExportError> {
    write_if_changed(
        &root.join("README.md"),
        b"# Company News Archive\n\nGenerated by company-feed-server from approved public sources.\n",
    )?;

    let mut jsonl = Vec::new();
    for item in items {
        serde_json::to_writer(&mut jsonl, item)?;
        jsonl.push(b'\n');
    }
    write_if_changed(&root.join("feeds/latest.jsonl"), &jsonl)?;
    write_if_changed(&root.join("companies/.gitkeep"), b"")?;
    write_if_changed(&root.join("indexes/by_date/.gitkeep"), b"")?;

    let mut by_company: BTreeMap<&str, Vec<IndexEntry<'_>>> = BTreeMap::new();
    let mut by_date: BTreeMap<NaiveDate, Vec<IndexEntry<'_>>> = BTreeMap::new();
    for item in items {
        let entry = IndexEntry::from(item);
        by_company
            .entry(&item.company_key)
            .or_default()
            .push(entry.clone());
        by_date.entry(item.archive_date()).or_default().push(entry);
    }

    let company_summary = by_company
        .iter()
        .map(|(company_key, entries)| {
            (
                *company_key,
                CompanyIndexSummary {
                    item_count: entries.len(),
                    index_path: format!("companies/{company_key}/index.json"),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    write_json(&root.join("indexes/by_company.json"), &company_summary)?;

    for (company_key, entries) in by_company {
        write_json(
            &root.join("companies").join(company_key).join("index.json"),
            &entries,
        )?;
    }
    for (date, entries) in by_date {
        write_json(
            &root
                .join("indexes/by_date")
                .join(format!("{}.json", date.format("%Y-%m-%d"))),
            &entries,
        )?;
    }
    Ok(())
}

fn stable_item_path(item: &ExportableFeedItem) -> Result<PathBuf, ExportError> {
    if let Some(previous) = item.previous_exported_path.as_deref()
        && previous.extension() == Some(OsStr::new("md"))
        && is_safe_owned_relative_path(previous)
    {
        return Ok(previous.to_owned());
    }

    let company_key = safe_component(&item.company_key);
    if company_key.is_empty() {
        return Err(ExportError::InvalidPath(format!(
            "company key {} cannot form an archive path",
            item.company_key
        )));
    }
    let date = item
        .item
        .published_at
        .unwrap_or(item.item.fetched_at)
        .date_naive();
    let slug = slugify(&item.item.title);
    let id = item.item.id.simple().to_string();
    Ok(PathBuf::from("companies")
        .join(company_key)
        .join(date.format("%Y").to_string())
        .join(date.format("%m").to_string())
        .join(format!(
            "{}-{}-{}.md",
            date.format("%Y-%m-%d"),
            slug,
            &id[..8]
        )))
}

fn slugify(value: &str) -> String {
    let mut output = String::new();
    let mut separator_pending = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if separator_pending && !output.is_empty() {
                output.push('-');
            }
            output.push(character.to_ascii_lowercase());
            separator_pending = false;
        } else if !output.is_empty() {
            separator_pending = true;
        }
        if output.len() >= 72 {
            break;
        }
    }
    if output.is_empty() {
        "item".to_owned()
    } else {
        output.trim_end_matches('-').to_owned()
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect()
}

fn is_safe_owned_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && path.starts_with("companies")
}

fn markdown_document(item: &PublicFeedItem) -> String {
    let quoted = |value: &str| serde_json::to_string(value).expect("strings serialize to JSON");
    format!(
        "---\ncompany_key: {}\ncompany: {}\nsource_id: {}\nurl: {}\ncanonical_url: {}\npublished_at: {}\nfetched_at: {}\ncontent_hash: {}\n---\n\n# {}\n\n{}\n",
        quoted(&item.company_key),
        quoted(&item.company_name),
        quoted(&item.source_id),
        quoted(&item.url),
        quoted(&item.canonical_url),
        item.published_at
            .as_deref()
            .map(quoted)
            .unwrap_or_else(|| "null".to_owned()),
        quoted(&item.fetched_at),
        quoted(&item.content_hash),
        item.title,
        item.body_markdown,
    )
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ExportError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_if_changed(path, &bytes)
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

fn remove_if_exists(path: &Path) -> Result<(), ExportError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn repository_has_owned_changes(root: &Path) -> Result<bool, ExportError> {
    let output = git(
        root,
        ["status", "--porcelain", "--untracked-files=all", "--"]
            .into_iter()
            .chain(OWNED_PATHS),
    )?;
    Ok(!output.stdout.is_empty())
}

fn git<I, S>(root: &Path, arguments: I) -> Result<Output, ExportError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(ExportError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn git_optional<I, S>(root: &Path, arguments: I) -> Result<Option<Output>, ExportError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    match Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
    {
        Ok(output) => Ok(Some(output)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[derive(Clone, Debug, Serialize)]
struct PublicFeedItem {
    id: String,
    company_key: String,
    company_name: String,
    source_id: String,
    external_id: String,
    url: String,
    canonical_url: String,
    title: String,
    summary: String,
    body_text: String,
    body_html: String,
    body_markdown: String,
    published_at: Option<String>,
    fetched_at: String,
    content_hash: String,
    source_kind: String,
}

impl PublicFeedItem {
    fn sort_timestamp(&self) -> &str {
        self.published_at.as_deref().unwrap_or(&self.fetched_at)
    }

    fn archive_date(&self) -> NaiveDate {
        self.published_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.date_naive())
            .unwrap_or_else(|| {
                DateTime::parse_from_rfc3339(&self.fetched_at)
                    .expect("export timestamps are RFC 3339")
                    .date_naive()
            })
    }
}

impl From<&ExportableFeedItem> for PublicFeedItem {
    fn from(exportable: &ExportableFeedItem) -> Self {
        let item: &FeedItem = &exportable.item;
        Self {
            id: item.id.to_string(),
            company_key: exportable.company_key.clone(),
            company_name: exportable.company_name.clone(),
            source_id: exportable.source_key.clone(),
            external_id: item.external_id.clone(),
            url: item.url.to_string(),
            canonical_url: item.canonical_url.to_string(),
            title: item.title.clone(),
            summary: item.summary.clone(),
            body_text: item.body_text.clone(),
            body_html: item.body_html.clone(),
            body_markdown: item.body_markdown.clone(),
            published_at: item.published_at.map(|value| value.to_rfc3339()),
            fetched_at: item.fetched_at.to_rfc3339(),
            content_hash: item.content_hash.clone(),
            source_kind: item.source_kind.to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct IndexEntry<'a> {
    id: &'a str,
    title: &'a str,
    url: &'a str,
    published_at: Option<&'a str>,
    fetched_at: &'a str,
    content_hash: &'a str,
}

impl<'a> From<&'a PublicFeedItem> for IndexEntry<'a> {
    fn from(item: &'a PublicFeedItem) -> Self {
        Self {
            id: &item.id,
            title: &item.title,
            url: &item.canonical_url,
            published_at: item.published_at.as_deref(),
            fetched_at: &item.fetched_at,
            content_hash: &item.content_hash,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct CompanyIndexSummary {
    item_count: usize,
    index_path: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("export worker failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("git operation failed: {0}")]
    Git(String),
    #[error("unsupported export layout: {0}")]
    UnsupportedLayout(String),
    #[error("invalid export path: {0}")]
    InvalidPath(String),
    #[error("export invariant violated: {0}")]
    Invariant(String),
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::TimeZone;
    use feed_core::{ExportFormat, ExportLayout, SourceKind};
    use serde_json::json;
    use tempfile::TempDir;
    use url::Url;
    use uuid::Uuid;

    use super::*;

    fn fixture(root: &Path) -> (ExportTarget, Vec<ExportableFeedItem>) {
        let now = Utc
            .with_ymd_and_hms(2026, 7, 19, 12, 0, 0)
            .single()
            .expect("valid timestamp");
        let id = Uuid::new_v4();
        (
            ExportTarget {
                id: Uuid::new_v4(),
                target_id: "archive".to_owned(),
                repo_url: root.join("unused.git").display().to_string(),
                local_path: root.join("archive"),
                branch: "main".to_owned(),
                format: ExportFormat::MarkdownJson,
                layout: ExportLayout::ByCompanyDate,
                cadence_seconds: 3600,
                enabled: true,
                push_enabled: false,
                metadata: json!({}),
                last_scheduled_at: None,
                created_at: now,
                updated_at: now,
            },
            vec![ExportableFeedItem {
                item: FeedItem {
                    id,
                    company_id: Uuid::new_v4(),
                    source_id: Uuid::new_v4(),
                    external_id: "post-1".to_owned(),
                    url: Url::parse("https://example.com/post-1").expect("valid URL"),
                    canonical_url: Url::parse("https://example.com/post-1").expect("valid URL"),
                    title: "A Product Launch!".to_owned(),
                    summary: "Summary".to_owned(),
                    body_text: "Safe body".to_owned(),
                    body_html: "<p>Safe body</p>".to_owned(),
                    body_markdown: "Safe body".to_owned(),
                    published_at: Some(now),
                    fetched_at: now,
                    content_hash: "sha256:test".to_owned(),
                    source_kind: SourceKind::Rss,
                    content_processing: json!({}),
                    created_at: now,
                    updated_at: now,
                },
                company_key: "acme".to_owned(),
                company_name: "Acme Corp".to_owned(),
                source_key: "acme-news".to_owned(),
                previous_exported_path: None,
                previous_content_hash: None,
            }],
        )
    }

    #[tokio::test]
    async fn materializes_and_commits_an_idempotent_archive() {
        let temporary = TempDir::new().expect("temporary directory");
        let (target, items) = fixture(temporary.path());

        let first = export_archive(target.clone(), items.clone())
            .await
            .expect("first export");
        assert!(first.changed);
        assert!(first.commit_sha.is_some());
        assert_eq!(first.records.len(), 1);
        assert!(
            target
                .local_path
                .join(&first.records[0].exported_path)
                .is_file()
        );
        assert!(target.local_path.join("feeds/latest.jsonl").is_file());

        let second = export_archive(target, items).await.expect("second export");
        assert!(!second.changed);
        assert_eq!(second.commit_sha, first.commit_sha);
    }

    #[tokio::test]
    async fn commits_an_empty_archive_on_cold_start() {
        let temporary = TempDir::new().expect("temporary directory");
        let (target, _) = fixture(temporary.path());

        let result = export_archive(target.clone(), Vec::new())
            .await
            .expect("empty export");
        assert!(result.changed);
        assert!(result.commit_sha.is_some());
        assert!(result.records.is_empty());
        assert!(target.local_path.join("companies/.gitkeep").is_file());
        assert!(target.local_path.join("feeds/latest.jsonl").is_file());
    }

    #[test]
    fn rejects_traversal_in_previous_paths() {
        assert!(!is_safe_owned_relative_path(Path::new("../secrets")));
        assert!(!is_safe_owned_relative_path(Path::new("/tmp/file")));
        assert!(is_safe_owned_relative_path(Path::new(
            "companies/acme/2026/07/item.md"
        )));
    }

    #[test]
    fn stable_path_reuses_a_safe_previous_path() {
        let temporary = TempDir::new().expect("temporary directory");
        let (_, mut items) = fixture(temporary.path());
        let prior = PathBuf::from("companies/acme/2025/01/original.md");
        items[0].previous_exported_path = Some(prior.clone());
        assert_eq!(stable_item_path(&items[0]).expect("path"), prior);
    }
}
