use std::{
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, Output},
};

use chrono::Utc;
use feed_core::{ExportLayout, ExportTarget, ExportableFeedItem, ExportedItem};

mod archive_v1;

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

    let records = archive_v1::materialize_archive(&target.local_path, items)?;

    let changed = repository_has_owned_changes(&target.local_path)?;
    if changed {
        git(
            &target.local_path,
            ["add", "--all", "--"]
                .into_iter()
                .chain(existing_owned_pathspecs(&target.local_path)?),
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

fn repository_has_owned_changes(root: &Path) -> Result<bool, ExportError> {
    let owned = existing_owned_pathspecs(root)?;
    let output = git(
        root,
        ["status", "--porcelain", "--untracked-files=all", "--"]
            .into_iter()
            .chain(owned),
    )?;
    Ok(!output.stdout.is_empty())
}

fn existing_owned_pathspecs(root: &Path) -> Result<Vec<&'static str>, ExportError> {
    let mut paths = Vec::new();
    for owned in archive_v1::OWNED_PATHS {
        let tracked = git_optional(root, ["ls-files", "--", owned])?
            .is_some_and(|output| output.status.success() && !output.stdout.is_empty());
        if root.join(owned).exists() || tracked {
            paths.push(owned);
        }
    }
    Ok(paths)
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
    use chrono::{Duration, TimeZone};
    use feed_core::{ExportFormat, ExportLayout, FeedItem, SourceKind};
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
                    content_hash: format!("sha256:{}", "1".repeat(64)),
                    source_kind: SourceKind::Rss,
                    content_processing: json!({}),
                    created_at: now,
                    updated_at: now,
                },
                company_key: "acme".to_owned(),
                company_name: "Acme Corp".to_owned(),
                company_category_key: "technology-3169ce6442acdc81".to_owned(),
                company_category_name: "Technology".to_owned(),
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
        assert!(target.local_path.join("HEAD.json").is_file());
        assert!(target.local_path.join("index.json").is_file());
        assert!(
            target
                .local_path
                .join("index/v1/current/manifest.json")
                .is_file()
        );
        assert!(
            target
                .local_path
                .join("index/v1/current/recent/manifest.json")
                .is_file()
        );
        assert!(
            target
                .local_path
                .join("index/v1/current/companies/manifest.json")
                .is_file()
        );
        assert!(
            target
                .local_path
                .join("index/v1/current/categories/manifest.json")
                .is_file()
        );
        let data_index: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join("index.json")).expect("data index"),
        )
        .expect("valid data index");
        assert_eq!(
            data_index["paths"]["category_directory_manifest"],
            "index/v1/current/categories/manifest.json"
        );
        assert_eq!(data_index["contract_version"], "1.1.0");
        let category_manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(
                target
                    .local_path
                    .join("index/v1/current/categories/manifest.json"),
            )
            .expect("category manifest"),
        )
        .expect("valid category manifest");
        assert_eq!(
            data_index["taxonomy_generation"],
            category_manifest["taxonomy_generation"]
        );
        assert_eq!(category_manifest["category_count"], 1);
        assert_eq!(
            category_manifest["categories"][0]["key"],
            "technology-3169ce6442acdc81"
        );
        assert_eq!(category_manifest["categories"][0]["company_count"], 1);
        assert_eq!(category_manifest["categories"][0]["record_count"], 1);
        assert_eq!(category_manifest["categories"][0]["page_count"], 1);
        assert_eq!(
            category_manifest["categories"][0]["pages"][0]["company_count"],
            1
        );
        assert!(
            target
                .local_path
                .join(&first.records[0].exported_path)
                .with_file_name("record.json")
                .is_file()
        );
        let validation = Command::new("python3")
            .current_dir(&target.local_path)
            .arg("scripts/validate_archive.py")
            .output()
            .expect("run generated archive validator");
        assert!(
            validation.status.success(),
            "{}",
            String::from_utf8_lossy(&validation.stderr)
        );

        let second = export_archive(target, items).await.expect("second export");
        assert!(!second.changed);
        assert_eq!(second.commit_sha, first.commit_sha);
    }

    #[tokio::test]
    async fn taxonomy_changes_do_not_change_article_generation() {
        let temporary = TempDir::new().expect("temporary directory");
        let (target, mut items) = fixture(temporary.path());

        export_archive(target.clone(), items.clone())
            .await
            .expect("first export");
        let first_head: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join("HEAD.json")).expect("first head"),
        )
        .expect("valid first head");
        let first_index: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join("index.json")).expect("first data index"),
        )
        .expect("valid first data index");
        let first_categories: serde_json::Value = serde_json::from_slice(
            &fs::read(
                target
                    .local_path
                    .join("index/v1/current/categories/manifest.json"),
            )
            .expect("first category manifest"),
        )
        .expect("valid first category manifest");

        items[0].company_category_key = "health-care-04b0a88a87070712".to_owned();
        items[0].company_category_name = "Health Care".to_owned();
        let second = export_archive(target.clone(), items)
            .await
            .expect("taxonomy-only export");
        assert!(second.changed);
        let second_head: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join("HEAD.json")).expect("second head"),
        )
        .expect("valid second head");
        let second_index: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join("index.json")).expect("second data index"),
        )
        .expect("valid second data index");
        let second_categories: serde_json::Value = serde_json::from_slice(
            &fs::read(
                target
                    .local_path
                    .join("index/v1/current/categories/manifest.json"),
            )
            .expect("second category manifest"),
        )
        .expect("valid second category manifest");

        assert_eq!(first_head["generation"], second_head["generation"]);
        assert_eq!(
            first_categories["generation"],
            second_categories["generation"]
        );
        assert_ne!(
            first_categories["taxonomy_generation"],
            second_categories["taxonomy_generation"]
        );
        assert_eq!(
            first_index["taxonomy_generation"],
            first_categories["taxonomy_generation"]
        );
        assert_eq!(
            second_index["taxonomy_generation"],
            second_categories["taxonomy_generation"]
        );
        assert_ne!(
            first_index["taxonomy_generation"],
            second_index["taxonomy_generation"]
        );
        assert!(first_head.get("taxonomy_generation").is_none());
        assert!(second_head.get("taxonomy_generation").is_none());
        assert_eq!(
            second_categories["categories"][0]["key"],
            "health-care-04b0a88a87070712"
        );
    }

    #[tokio::test]
    async fn category_company_pages_are_bounded() {
        let temporary = TempDir::new().expect("temporary directory");
        let (target, seed) = fixture(temporary.path());
        let seed = seed.first().expect("fixture item");
        let items = (0..101)
            .map(|index| {
                let mut item = seed.clone();
                let url = Url::parse(&format!("https://example.com/company-{index}/post"))
                    .expect("valid URL");
                item.item.id = Uuid::new_v4();
                item.item.company_id = Uuid::new_v4();
                item.item.source_id = Uuid::new_v4();
                item.item.external_id = format!("company-{index}-post");
                item.item.url = url.clone();
                item.item.canonical_url = url;
                item.company_key = format!("company-{index:03}");
                item.company_name = format!("Company {index:03}");
                item.source_key = format!("company-{index:03}-news");
                item
            })
            .collect::<Vec<_>>();

        export_archive(target.clone(), items)
            .await
            .expect("category-paged export");
        let categories: serde_json::Value = serde_json::from_slice(
            &fs::read(
                target
                    .local_path
                    .join("index/v1/current/categories/manifest.json"),
            )
            .expect("category manifest"),
        )
        .expect("valid category manifest");
        assert_eq!(categories["page_size"], 100);
        assert_eq!(categories["categories"][0]["page_count"], 2);
        assert_eq!(
            categories["categories"][0]["pages"][0]["company_count"],
            100
        );
        assert_eq!(categories["categories"][0]["pages"][1]["company_count"], 1);
        assert!(
            !target
                .local_path
                .join("index/v1/current/categories/buckets/technology-3169ce6442acdc81.json")
                .exists()
        );
        let validation = Command::new("python3")
            .current_dir(&target.local_path)
            .arg("scripts/validate_archive.py")
            .output()
            .expect("run generated archive validator");
        assert!(
            validation.status.success(),
            "{}",
            String::from_utf8_lossy(&validation.stderr)
        );
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
        assert!(target.local_path.join("HEAD.json").is_file());
        assert!(target.local_path.join("index.json").is_file());
        assert!(
            target
                .local_path
                .join("index/v1/current/manifest.json")
                .is_file()
        );
    }

    #[tokio::test]
    async fn emits_bounded_navigation_pages_without_article_bodies() {
        let temporary = TempDir::new().expect("temporary directory");
        let (target, seed) = fixture(temporary.path());
        let seed = seed.first().expect("fixture item");
        let items = (0..51)
            .map(|index| {
                let mut item = seed.clone();
                let url =
                    Url::parse(&format!("https://example.com/post-{index}")).expect("valid URL");
                item.item.id = Uuid::new_v4();
                item.item.external_id = format!("post-{index}");
                item.item.url = url.clone();
                item.item.canonical_url = url;
                item.item.title = format!("Article {index}");
                item.item.published_at = item
                    .item
                    .published_at
                    .map(|value| value + Duration::seconds(index));
                item
            })
            .collect::<Vec<_>>();

        export_archive(target.clone(), items)
            .await
            .expect("paged export");

        let recent: serde_json::Value = serde_json::from_slice(
            &fs::read(
                target
                    .local_path
                    .join("index/v1/current/recent/manifest.json"),
            )
            .expect("recent manifest"),
        )
        .expect("valid recent manifest");
        assert_eq!(recent["page_count"], 2);
        assert_eq!(recent["pages"][0]["record_count"], 50);
        assert_eq!(recent["pages"][1]["record_count"], 1);

        let first_page_path = recent["pages"][0]["path"].as_str().expect("page path");
        let first_page: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join(first_page_path)).expect("first page"),
        )
        .expect("valid first page");
        assert_eq!(first_page["items"].as_array().map(Vec::len), Some(50));
        assert!(
            first_page["items"]
                .as_array()
                .expect("page items")
                .iter()
                .all(|item| item.get("body_text").is_none())
        );
    }
}
