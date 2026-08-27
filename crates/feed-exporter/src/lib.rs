use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use chrono::Utc;
use feed_core::{ExportLayout, ExportTarget, ExportableFeedItem, ExportedItem};
use serde::Deserialize;

/// Opt-in config (via `export_target.metadata.v2_mirror`) to publish the v2 tree to
/// a dedicated, clean, v2-only repository with its OWN history + remote, kept
/// separate from the exporter's heavyweight working repo (which still carries the
/// frozen v1 tree and a huge history). This is what lets a servable data host
/// receive tiny incremental pushes without inheriting the working repo's bloat.
#[derive(Debug, Clone, Deserialize)]
struct V2Mirror {
    #[serde(default = "default_true")]
    enabled: bool,
    /// Local path of the clean v2-only repo (v2/ lives at its root).
    repo_path: PathBuf,
    /// Push URL WITHOUT credentials, e.g. https://github.com/<acct>/<repo>.git.
    remote_url: String,
    #[serde(default = "default_main_branch")]
    branch: String,
    /// Name of the env var holding the push PAT (injected into the URL at push
    /// time so it is never persisted in a committed config).
    #[serde(default = "default_pat_env")]
    pat_env: String,
}

fn default_true() -> bool {
    true
}
fn default_main_branch() -> String {
    "main".to_owned()
}
fn default_pat_env() -> String {
    "DATA_HOST_GITHUB_PAT".to_owned()
}

mod archive_v1;
mod archive_v2;
pub mod selector;

pub use selector::{SelectorTestVector, ShardScheme, canonical_test_vectors, selector};

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

    // The UI now reads `v2/` exclusively, so v2 is always written and is the source
    // of the returned records. The legacy v1 tree (~296k files) is only re-written
    // when a target opts back in via `metadata.dual_write_v1 = true`; by default the
    // dual-write is retired, which is the dominant cost of an export. The already-
    // committed v1 tree simply freezes (it is not deleted). Absence of the flag keeps
    // the historical behavior for any target that has not opted out. See the
    // migration section of docs/design/2026-08-26-time-partitioned-archive.md.
    let dual_write_v1 = target
        .metadata
        .get("dual_write_v1")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mirror = parse_v2_mirror(&target.metadata)?;

    // v2 is written into the clean mirror repo when configured (its own history +
    // remote), otherwise into the main working repo (legacy path).
    let v2_root: PathBuf = match &mirror {
        Some(m) => {
            ensure_git_repo(&m.repo_path, &m.branch)?;
            m.repo_path.clone()
        }
        None => target.local_path.clone(),
    };
    let records = if dual_write_v1 {
        let v1_records = archive_v1::materialize_archive(&target.local_path, items)?;
        archive_v2::materialize_archive(&v2_root, items)?;
        v1_records
    } else {
        archive_v2::materialize_archive(&v2_root, items)?
    };

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
    let main_pushed = if target.push_enabled {
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

    // Commit + incrementally push the v2 tree to its dedicated data host.
    let (mirror_committed, mirror_pushed) = match &mirror {
        Some(m) => mirror_commit_and_push(m)?,
        None => (false, false),
    };

    Ok(ExportResult {
        changed: changed || mirror_committed,
        pushed: main_pushed || mirror_pushed,
        records,
        commit_sha,
    })
}

/// Parse the optional v2-mirror config from a target's metadata. Absent key =>
/// None (legacy behavior). Present-but-malformed => hard error (fail-fast on a
/// misconfigured host rather than silently not publishing). Present + enabled:false
/// => None.
fn parse_v2_mirror(metadata: &serde_json::Value) -> Result<Option<V2Mirror>, ExportError> {
    let Some(raw) = metadata.get("v2_mirror") else {
        return Ok(None);
    };
    let mirror: V2Mirror = serde_json::from_value(raw.clone())
        .map_err(|error| ExportError::Git(format!("invalid v2_mirror config: {error}")))?;
    Ok(mirror.enabled.then_some(mirror))
}

fn ensure_git_repo(root: &Path, branch: &str) -> Result<(), ExportError> {
    fs::create_dir_all(root)?;
    if !root.join(".git").is_dir() {
        git(root, ["init", "--initial-branch", branch])?;
    }
    if !git_optional(root, ["config", "--get", "user.name"])?
        .is_some_and(|output| output.status.success())
    {
        git(root, ["config", "user.name", "yuacxshard1"])?;
    }
    if !git_optional(root, ["config", "--get", "user.email"])?
        .is_some_and(|output| output.status.success())
    {
        git(
            root,
            ["config", "user.email", "yuacxshard1@users.noreply.github.com"],
        )?;
    }
    Ok(())
}

/// Stage the v2 tree in the mirror repo, commit if anything changed, and push
/// incrementally to the data host. Returns (committed, pushed). A push failure is
/// logged and swallowed (the local commit is kept and the next export fast-forwards
/// it) so a transient host outage never fails the export -- the data is already
/// written locally.
fn mirror_commit_and_push(mirror: &V2Mirror) -> Result<(bool, bool), ExportError> {
    git(&mirror.repo_path, ["add", "v2"])?;
    if mirror.repo_path.join("README.md").exists() {
        git(&mirror.repo_path, ["add", "README.md"])?;
    }
    let status = git(&mirror.repo_path, ["status", "--porcelain"])?;
    if status.stdout.is_empty() {
        return Ok((false, false));
    }
    let message = format!("v2 export {}", Utc::now().format("%Y-%m-%dT%H:%M:%SZ"));
    git(&mirror.repo_path, ["commit", "-m", &message])?;

    let pat = std::env::var(&mirror.pat_env).unwrap_or_default();
    if pat.is_empty() {
        eprintln!(
            "[exporter] v2 mirror: env {} is empty; committed locally but NOT pushing (set the PAT to publish)",
            mirror.pat_env
        );
        return Ok((true, false));
    }
    let authed = authed_push_url(&mirror.remote_url, &pat);
    let refspec = format!("HEAD:refs/heads/{}", mirror.branch);
    match git(
        &mirror.repo_path,
        ["push", authed.as_str(), refspec.as_str()],
    ) {
        Ok(_) => {
            // We push to an explicit authed URL (so the PAT is never stored in the
            // repo config), which does not advance the local remote-tracking ref.
            // Sync it so the repo's view of origin stays honest and future
            // `rev-list origin/..HEAD` diagnostics don't grow unbounded.
            let tracking = format!("refs/remotes/origin/{}", mirror.branch);
            let _ = git(&mirror.repo_path, ["update-ref", tracking.as_str(), "HEAD"]);
            Ok((true, true))
        }
        Err(error) => {
            eprintln!(
                "[exporter] v2 mirror push failed (local commit kept, next export retries): {error}"
            );
            Ok((true, false))
        }
    }
}

/// Inject a PAT into an `https://host/...` URL as `https://<pat>@host/...`.
fn authed_push_url(remote_url: &str, pat: &str) -> String {
    match remote_url.strip_prefix("https://") {
        Some(rest) if !pat.is_empty() => format!("https://{pat}@{rest}"),
        _ => remote_url.to_owned(),
    }
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
    for owned in archive_v1::OWNED_PATHS
        .iter()
        .copied()
        .chain(archive_v2::OWNED_PATHS.iter().copied())
    {
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
        for generated_asset in [
            "CONTRIBUTING.md",
            "SECURITY.md",
            ".github/ISSUE_TEMPLATE/archive-bug.yml",
            ".github/ISSUE_TEMPLATE/config.yml",
            ".github/ISSUE_TEMPLATE/data-correction.yml",
            ".github/ISSUE_TEMPLATE/rights.yml",
        ] {
            assert!(
                target.local_path.join(generated_asset).is_file(),
                "missing generated repository asset {generated_asset}"
            );
        }
        let generated_readme =
            fs::read_to_string(target.local_path.join("README.md")).expect("generated README");
        assert!(generated_readme.contains("[`CONTRIBUTING.md`](CONTRIBUTING.md)"));
        assert!(generated_readme.contains("[`SECURITY.md`](SECURITY.md)"));
        assert!(generated_readme.contains("Do not start with a normal Git clone"));
        assert!(generated_readme.contains("Taxonomy generation"));
        assert!(generated_readme.contains("data-correction.yml"));
        assert!(generated_readme.contains("rights.yml"));
        let generated_rights = fs::read_to_string(target.local_path.join("CONTENT_RIGHTS.md"))
            .expect("generated content-rights policy");
        assert!(generated_rights.contains("approved_public"));
        assert!(generated_rights.contains("not a copyright license"));
        assert!(generated_rights.contains("data-correction.yml"));
        assert!(generated_rights.contains("rights.yml"));
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

        // The v2 dual-write tree must be committed, not left untracked. This is
        // the regression guard for the `git add --all -- <owned pathspecs>` bug
        // where v2/ fell outside the owned pathspecs and stayed `?? v2/`.
        assert!(target.local_path.join("v2/config.json").is_file());
        let v2_tracked = git(&target.local_path, ["ls-files", "--", "v2"])
            .expect("git ls-files v2");
        assert!(
            !v2_tracked.stdout.is_empty(),
            "v2/ tree was written but not git-tracked (owned-pathspec regression)"
        );
        let v2_status = git(
            &target.local_path,
            ["status", "--porcelain", "--untracked-files=all", "--", "v2"],
        )
        .expect("git status v2");
        assert!(
            v2_status.stdout.is_empty(),
            "v2/ has uncommitted changes after export: {}",
            String::from_utf8_lossy(&v2_status.stdout)
        );

        let second = export_archive(target, items).await.expect("second export");
        assert!(!second.changed);
        assert_eq!(second.commit_sha, first.commit_sha);
    }

    #[tokio::test]
    async fn dual_write_v1_false_writes_only_v2() {
        let temporary = TempDir::new().expect("temporary directory");
        let (mut target, items) = fixture(temporary.path());
        // Opt out of the legacy v1 dual-write (the post-migration production mode).
        target.metadata = json!({ "dual_write_v1": false });

        let result = export_archive(target.clone(), items)
            .await
            .expect("v2-only export");
        assert!(result.changed);
        // v2 tree is written and the records point into it.
        assert!(target.local_path.join("v2/config.json").is_file());
        assert!(result.records[0].exported_path.starts_with("v2/data/"));
        assert!(
            target
                .local_path
                .join(&result.records[0].exported_path)
                .is_file()
        );
        // the heavy v1 article tree is NOT produced.
        assert!(!target.local_path.join("articles").exists());
        assert!(!target.local_path.join("index/v1").exists());
    }

    #[tokio::test]
    async fn v2_mirror_writes_v2_to_the_mirror_repo_and_commits_it() {
        let temporary = TempDir::new().expect("temporary directory");
        let (mut target, items) = fixture(temporary.path());
        let mirror_path = temporary.path().join("mirror");
        target.metadata = json!({
            "dual_write_v1": false,
            "v2_mirror": {
                "enabled": true,
                "repo_path": mirror_path.to_str().unwrap(),
                "remote_url": "https://github.com/example/company-news-data.git",
                "branch": "main",
                // point at an env var that is unset so the test never hits the network
                "pat_env": "EXPORTER_TEST_UNSET_PAT",
            },
        });

        let result = export_archive(target.clone(), items)
            .await
            .expect("mirror export");
        assert!(result.changed);
        // v2 went to the MIRROR repo, not the main working repo.
        assert!(mirror_path.join("v2/config.json").is_file());
        assert!(!target.local_path.join("v2/config.json").exists());
        // the mirror repo committed the v2 tree (nothing left uncommitted).
        let mirror_status = git(&mirror_path, ["status", "--porcelain"]).expect("git status");
        assert!(
            mirror_status.stdout.is_empty(),
            "mirror repo has uncommitted changes: {}",
            String::from_utf8_lossy(&mirror_status.stdout)
        );
        let tracked = git(&mirror_path, ["ls-files", "--", "v2"]).expect("ls-files");
        assert!(!tracked.stdout.is_empty(), "v2/ must be tracked in the mirror repo");
        // records point into the v2 tree.
        assert!(result.records[0].exported_path.starts_with("v2/data/"));
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
    async fn pure_refetch_does_not_change_article_generation() {
        let temporary = TempDir::new().expect("temporary directory");
        let (target, mut items) = fixture(temporary.path());

        export_archive(target.clone(), items.clone())
            .await
            .expect("first export");
        let first_head: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join("HEAD.json")).expect("first head"),
        )
        .expect("valid first head");

        // A no-op re-crawl: only the volatile fetch bookkeeping timestamps
        // advance, while every content field (title, summary, body, published_at,
        // content_hash) is byte-identical. The generation is a content hash, so
        // it must NOT change.
        let later = Utc
            .with_ymd_and_hms(2026, 8, 9, 6, 0, 0)
            .single()
            .expect("valid timestamp");
        items[0].item.fetched_at = later;
        items[0].item.updated_at = later;

        export_archive(target.clone(), items)
            .await
            .expect("refetch export");
        let second_head: serde_json::Value = serde_json::from_slice(
            &fs::read(target.local_path.join("HEAD.json")).expect("second head"),
        )
        .expect("valid second head");

        assert_eq!(first_head["generation"], second_head["generation"]);
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
