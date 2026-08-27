//! Archive v2 writer: time-partitioned, append-only, minimal-diff layout.
//!
//! Strategy: DETERMINISTIC full regeneration. Every file's bytes are a pure
//! function of the (stable) content, so unchanged partitions serialize to
//! byte-identical files and git sees no diff -- functionally append-only without
//! incremental bookkeeping. The only per-crawl-volatile data (fetched_at,
//! last_updated_at) is isolated to a single `meta/timestamps.jsonl`, so a no-content
//! re-crawl diffs exactly that one file. Article data files are only (re)written
//! when new or when their content_hash changed, to keep disk IO bounded.
//!
//! See docs/design/2026-08-26-time-partitioned-archive.md.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::Path,
};

use chrono::{DateTime, Datelike, Utc};
use feed_core::{ExportableFeedItem, ExportedItem};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::selector::{ShardScheme, selector};

const ARTICLE_SCHEME_SALT: &str = "cnd/articles/v1";
const COMPANY_SCHEME_SALT: &str = "cnd/companies/v1";
const ARTICLE_BUCKETS: u32 = 16;
const COMPANY_BUCKETS: u32 = 16;
const ARCHIVE_VERSION: &str = "v2";
const ARTICLE_SCHEMA: &str = "article/v2";

/// Repo paths this writer owns. The exporter only stages/commits owned paths so
/// it never clobbers repo files it doesn't manage; the whole v2 tree lives under
/// `v2/`, so a single entry covers it.
pub(super) const OWNED_PATHS: [&str; 1] = ["v2"];

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn document_id(item: &ExportableFeedItem) -> String {
    // Same identity contract as v1 so doc_ids are stable across the migration.
    sha256_hex(
        format!(
            "company-news-archive/article/v1\0{}\0{}",
            item.company_key, item.item.canonical_url
        )
        .as_bytes(),
    )
}

/// The article's publish date (falls back to the stable first-seen timestamp
/// when the item carries no published_at). Using the group's first-seen -- not
/// the representative row's own created_at -- keeps the day partition stable
/// even as duplicate rows come and go.
fn publish_date(item: &ExportableFeedItem, first_seen: DateTime<Utc>) -> DateTime<Utc> {
    item.item.published_at.unwrap_or(first_seen)
}

fn date_dir(date: DateTime<Utc>) -> String {
    format!("{:04}/{:02}/{:02}", date.year(), date.month(), date.day())
}

/// Stable content-only article record (NO fetched_at / last_updated_at).
#[derive(Serialize)]
struct ArticleV2<'a> {
    schema: &'a str,
    doc_id: &'a str,
    company_key: &'a str,
    company_name: &'a str,
    category_key: &'a str,
    category_name: &'a str,
    source_kind: &'a str,
    url: String,
    canonical_url: String,
    title: &'a str,
    summary: &'a str,
    body_markdown: &'a str,
    published_at: Option<String>,
    first_seen_at: String,
    content_hash: &'a str,
}

#[derive(Serialize)]
struct TimestampRow<'a> {
    doc_id: &'a str,
    fetched_at: String,
    last_updated_at: String,
}

#[derive(Serialize)]
struct ArticleIndexRow<'a> {
    doc_id: &'a str,
    date: &'a str,
    company_key: &'a str,
}

/// A lightweight article summary carried inline by the recent-day and
/// company-month indexes, so list views (Latest / Company) render from ONE index
/// fetch instead of fetching each article's full data file. Deliberately excludes
/// `body_markdown` (heavy) and `fetched_at` (volatile -> lives in the timestamps
/// sidecar). Past day/month files stay immutable because none of these fields move
/// on a normal re-crawl.
#[derive(Serialize)]
struct SummaryRow<'a> {
    doc_id: &'a str,
    company_key: &'a str,
    company_name: &'a str,
    source_kind: &'a str,
    title: &'a str,
    summary: &'a str,
    published_at: Option<String>,
    first_seen_at: String,
}

#[derive(Serialize)]
struct SchemeConfig {
    current: &'static str,
    v1: ShardScheme,
}

#[derive(Serialize)]
struct ArchiveConfig {
    archive_version: &'static str,
    generation: String,
    generated_at: String,
    record_count: usize,
    company_count: usize,
    articles_scheme: SchemeConfig,
    companies_scheme: SchemeConfig,
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if let Ok(existing) = fs::read(path)
        && existing == bytes
    {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    Ok(true)
}

/// Write the full v2 archive under `root/v2/`. Returns the exported records.
pub fn materialize_archive(
    root: &Path,
    items: &[ExportableFeedItem],
) -> io::Result<Vec<ExportedItem>> {
    let base = root.join(ARCHIVE_VERSION);

    // Group by doc_id deterministically. Multiple feed_item rows can map to one
    // doc_id (same URL crawled repeatedly / from several sources) with differing
    // created_at/fetched_at. To keep data files byte-stable across exports we must
    // NOT let the arbitrary DB row order decide which row wins:
    //   - first_seen_at := MIN(created_at) over the group (true earliest sighting,
    //     order-independent), so it never flaps.
    //   - the representative row (whose content fields are written) is the one with
    //     the greatest (created_at, item.id) -- i.e. the freshest content, chosen
    //     by a total order so it is deterministic.
    // BTreeMap iteration is sorted by doc_id, giving stable output everywhere.
    let mut groups: BTreeMap<String, (&ExportableFeedItem, DateTime<Utc>)> = BTreeMap::new();
    for i in items {
        let doc_id = document_id(i);
        let created = i.item.created_at;
        groups
            .entry(doc_id)
            .and_modify(|(rep, first_seen)| {
                if created < *first_seen {
                    *first_seen = created;
                }
                if (created, i.item.id) > (rep.item.created_at, rep.item.id) {
                    *rep = i;
                }
            })
            .or_insert((i, created));
    }
    let projected: Vec<(String, &ExportableFeedItem, DateTime<Utc>)> = groups
        .into_iter()
        .map(|(doc_id, (rep, first_seen))| (doc_id, rep, first_seen))
        .collect();

    let mut exported = Vec::with_capacity(projected.len());
    let mut article_index_buckets: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    // "YYYY/MM/DD" -> [(doc_id, summary row)]; the doc_id is kept only to sort rows
    // into a stable order for byte-consistency.
    let mut recent_days: BTreeMap<String, Vec<(String, serde_json::Value)>> = BTreeMap::new();
    // (company, "YYYY-MM") -> [(doc_id, summary row)]
    let mut company_months: BTreeMap<(String, String), Vec<(String, serde_json::Value)>> =
        BTreeMap::new();
    let mut company_meta: BTreeMap<String, (String, u32)> = BTreeMap::new(); // company_key -> (name, count)
    // taxonomy accumulation (categories change only on taxonomy edits; counts move
    // with content). company_key -> (category_key, category_name) and the latest
    // publish date per company, so a category file can list its companies with the
    // same {record_count, last_published_at} the reader sorts on.
    let mut company_category: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut company_last_published: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    let mut timestamp_rows: Vec<String> = Vec::with_capacity(projected.len());
    let mut generation_hasher = Sha256::new();
    generation_hasher.update(b"company-news-archive/generation/v2\0");

    for (doc_id, item, first_seen) in &projected {
        let it = &item.item;
        let date = publish_date(item, *first_seen);
        let ddir = date_dir(date);
        let ym = format!("{:04}-{:02}", date.year(), date.month());

        // 1) article data file (content only, written only when new/changed)
        let record = ArticleV2 {
            schema: ARTICLE_SCHEMA,
            doc_id,
            company_key: &item.company_key,
            company_name: &item.company_name,
            category_key: &item.company_category_key,
            category_name: &item.company_category_name,
            source_kind: it.source_kind.as_str(),
            url: it.url.to_string(),
            canonical_url: it.canonical_url.to_string(),
            title: &it.title,
            summary: &it.summary,
            body_markdown: &it.body_markdown,
            published_at: it.published_at.map(|d| d.to_rfc3339()),
            first_seen_at: first_seen.to_rfc3339(),
            content_hash: &it.content_hash,
        };
        let bytes = serde_json::to_vec_pretty(&record)?;
        let rel_path = format!("data/{ddir}/{doc_id}.json");
        let abs_path = base.join(&rel_path);
        write_if_changed(&abs_path, &bytes)?;

        // 2) accumulate indexes
        let bucket = selector(doc_id, ARTICLE_SCHEME_SALT, ARTICLE_BUCKETS);
        article_index_buckets
            .entry(bucket)
            .or_default()
            .push(serde_json::to_string(&ArticleIndexRow {
                doc_id,
                date: &ddir,
                company_key: &item.company_key,
            })?);
        let summary = serde_json::to_value(SummaryRow {
            doc_id,
            company_key: &item.company_key,
            company_name: &item.company_name,
            source_kind: it.source_kind.as_str(),
            title: &it.title,
            summary: &it.summary,
            published_at: it.published_at.map(|d| d.to_rfc3339()),
            first_seen_at: first_seen.to_rfc3339(),
        })?;
        recent_days
            .entry(ddir.clone())
            .or_default()
            .push((doc_id.clone(), summary.clone()));
        company_months
            .entry((item.company_key.clone(), ym))
            .or_default()
            .push((doc_id.clone(), summary));
        let entry = company_meta
            .entry(item.company_key.clone())
            .or_insert_with(|| (item.company_name.clone(), 0));
        entry.1 += 1;
        company_category.insert(
            item.company_key.clone(),
            (
                item.company_category_key.clone(),
                item.company_category_name.clone(),
            ),
        );
        company_last_published
            .entry(item.company_key.clone())
            .and_modify(|d| {
                if date > *d {
                    *d = date;
                }
            })
            .or_insert(date);

        // 3) volatile sidecar row
        timestamp_rows.push(serde_json::to_string(&TimestampRow {
            doc_id,
            fetched_at: it.fetched_at.to_rfc3339(),
            last_updated_at: it.updated_at.to_rfc3339(),
        })?);

        // 4) generation content fingerprint (content only, stable)
        generation_hasher.update(doc_id.as_bytes());
        generation_hasher.update([0]);
        generation_hasher.update(it.content_hash.as_bytes());
        generation_hasher.update([0]);

        exported.push(ExportedItem {
            feed_item_id: it.id,
            // root-relative (includes the `v2/` prefix) so the recorded path matches
            // how v1 recorded paths and resolves against the repo root.
            exported_path: Path::new(ARCHIVE_VERSION).join(&rel_path),
            exported_content_hash: it.content_hash.clone(),
        });
    }

    let generation = hex::encode(generation_hasher.finalize());

    // meta/timestamps.jsonl (the single volatile file), sorted by doc_id already
    write_if_changed(
        &base.join("meta/timestamps.jsonl"),
        (timestamp_rows.join("\n") + "\n").as_bytes(),
    )?;

    // index/articles/<scheme>/<bucket>.jsonl
    for (bucket, mut rows) in article_index_buckets {
        rows.sort();
        write_if_changed(
            &base.join(format!("index/articles/v1/{bucket:03}.jsonl")),
            (rows.join("\n") + "\n").as_bytes(),
        )?;
    }

    // index/recent/<Y>/<M>/<D>.json (per-day doc_id list; unchanged days => no diff)
    // + index/recent/days.json: the sorted-desc list of days so the UI can page
    // the recency feed without listing a directory.
    let mut day_dirs: Vec<String> = recent_days.keys().cloned().collect();
    for (ddir, mut rows) in recent_days {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let articles: Vec<serde_json::Value> = rows.into_iter().map(|(_, v)| v).collect();
        let body = serde_json::to_vec_pretty(&serde_json::json!({
            "date": ddir, "articles": articles,
        }))?;
        write_if_changed(&base.join(format!("index/recent/{ddir}.json")), &body)?;
    }
    day_dirs.sort();
    day_dirs.reverse();
    write_if_changed(
        &base.join("index/recent/days.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({ "days": day_dirs }))?,
    )?;

    // months present per company, so the company index row can list them (the UI
    // cannot list a directory over HTTP).
    let mut company_month_list: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (company, ym) in company_months.keys() {
        company_month_list
            .entry(company.clone())
            .or_default()
            .push(ym.clone());
    }
    for months in company_month_list.values_mut() {
        months.sort();
        months.reverse();
    }

    // index/companies/<company_key>/<YYYY-MM>.json
    for ((company, ym), mut rows) in company_months {
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let articles: Vec<serde_json::Value> = rows.into_iter().map(|(_, v)| v).collect();
        let body = serde_json::to_vec_pretty(&serde_json::json!({
            "company_key": company, "month": ym, "articles": articles,
        }))?;
        write_if_changed(
            &base.join(format!("index/companies/{company}/{ym}.json")),
            &body,
        )?;
    }

    // index/companies/<scheme>/<bucket>.jsonl (company_key -> summary)
    let mut company_index_buckets: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for (company, (name, count)) in &company_meta {
        let bucket = selector(company, COMPANY_SCHEME_SALT, COMPANY_BUCKETS);
        let months = company_month_list.get(company).cloned().unwrap_or_default();
        company_index_buckets.entry(bucket).or_default().push(
            serde_json::to_string(&serde_json::json!({
                "company_key": company, "company_name": name,
                "article_count": count, "months": months,
            }))?,
        );
    }
    for (bucket, mut rows) in company_index_buckets {
        rows.sort();
        write_if_changed(
            &base.join(format!("index/companies/_index/v1/{bucket:03}.jsonl")),
            (rows.join("\n") + "\n").as_bytes(),
        )?;
    }

    // taxonomy/: per-category company lists + a small directory aggregate. A
    // category file diffs only when one of its companies gains an article
    // (record_count / last_published_at move) or membership changes; the whole set
    // is a handful of small files. company_meta is a BTreeMap, so companies are
    // pushed in company_key order -> stable bytes.
    let mut category_companies: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    let mut category_meta: BTreeMap<String, (String, u32, u32)> = BTreeMap::new(); // cat_key -> (name, company_count, record_count)
    for (company_key, (company_name, count)) in &company_meta {
        let Some((cat_key, cat_name)) = company_category.get(company_key) else {
            continue;
        };
        let last = company_last_published
            .get(company_key)
            .map(|d| d.to_rfc3339());
        category_companies
            .entry(cat_key.clone())
            .or_default()
            .push(serde_json::json!({
                "company_key": company_key,
                "company_name": company_name,
                "record_count": count,
                "last_published_at": last,
            }));
        let m = category_meta
            .entry(cat_key.clone())
            .or_insert_with(|| (cat_name.clone(), 0, 0));
        m.1 += 1;
        m.2 += count;
    }
    for (cat_key, companies) in &category_companies {
        let (name, company_count, record_count) = &category_meta[cat_key];
        write_if_changed(
            &base.join(format!("taxonomy/categories/{cat_key}.json")),
            &serde_json::to_vec_pretty(&serde_json::json!({
                "key": cat_key, "name": name,
                "company_count": company_count, "record_count": record_count,
                "companies": companies,
            }))?,
        )?;
    }

    // taxonomy_generation: a fingerprint of the category STRUCTURE (which company
    // is in which category), stable across pure-content exports so a reader can
    // tell a taxonomy edit from a normal crawl.
    let mut tax_hasher = Sha256::new();
    tax_hasher.update(b"company-news-archive/taxonomy/v2\0");
    for (company_key, (cat_key, cat_name)) in &company_category {
        tax_hasher.update(company_key.as_bytes());
        tax_hasher.update([0]);
        tax_hasher.update(cat_key.as_bytes());
        tax_hasher.update([0]);
        tax_hasher.update(cat_name.as_bytes());
        tax_hasher.update([0]);
    }
    let taxonomy_generation = hex::encode(tax_hasher.finalize());
    let categories_dir: Vec<serde_json::Value> = category_meta
        .iter()
        .map(|(k, (name, cc, rc))| {
            serde_json::json!({
                "key": k, "name": name, "company_count": cc, "record_count": rc,
            })
        })
        .collect();
    write_if_changed(
        &base.join("taxonomy/directory.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({
            "taxonomy_generation": taxonomy_generation,
            "category_count": category_meta.len(),
            "company_count": company_meta.len(),
            "record_count": projected.len(),
            "categories": categories_dir,
        }))?,
    )?;

    // generated_at derives from content generation (stable unless content changes)
    let generated_at = format!("gen-{}", &generation[..16]);

    // config.json + index.json + HEAD.json
    let config = ArchiveConfig {
        archive_version: ARCHIVE_VERSION,
        generation: generation.clone(),
        generated_at: generated_at.clone(),
        record_count: projected.len(),
        company_count: company_meta.len(),
        articles_scheme: SchemeConfig {
            current: "v1",
            v1: ShardScheme::new(ARTICLE_SCHEME_SALT, ARTICLE_BUCKETS),
        },
        companies_scheme: SchemeConfig {
            current: "v1",
            v1: ShardScheme::new(COMPANY_SCHEME_SALT, COMPANY_BUCKETS),
        },
    };
    let config_bytes = serde_json::to_vec_pretty(&config)?;
    write_if_changed(&base.join("config.json"), &config_bytes)?;
    write_if_changed(&base.join("index.json"), &config_bytes)?;
    write_if_changed(
        &base.join("HEAD.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({
            "archive_version": ARCHIVE_VERSION,
            "generation": generation,
            "generated_at": generated_at,
            "record_count": projected.len(),
            "company_count": company_meta.len(),
        }))?,
    )?;

    Ok(exported)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chrono::TimeZone;
    use feed_core::SourceKind;
    use serde_json::json;
    use url::Url;
    use uuid::Uuid;

    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).single().unwrap()
    }

    fn item(url: &str, title: &str, content_hash: &str, fetched: i64, updated: i64) -> ExportableFeedItem {
        let u = Url::parse(url).unwrap();
        ExportableFeedItem {
            item: feed_core::FeedItem {
                id: Uuid::new_v4(),
                company_id: Uuid::new_v4(),
                source_id: Uuid::new_v4(),
                external_id: url.to_owned(),
                url: u.clone(),
                canonical_url: u,
                title: title.to_owned(),
                summary: "s".to_owned(),
                body_text: "b".to_owned(),
                body_html: "<p>b</p>".to_owned(),
                body_markdown: "b".to_owned(),
                published_at: Some(ts(1_760_000_000)),
                fetched_at: ts(fetched),
                content_hash: content_hash.to_owned(),
                source_kind: SourceKind::Sitemap,
                content_processing: json!({}),
                created_at: ts(1_759_000_000),
                updated_at: ts(updated),
            },
            company_key: "apple".to_owned(),
            company_name: "Apple".to_owned(),
            company_category_key: "tech".to_owned(),
            company_category_name: "Tech".to_owned(),
            source_key: "apple:sitemap".to_owned(),
            previous_exported_path: None,
            previous_content_hash: None,
        }
    }

    fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                    out.insert(rel, fs::read(&p).unwrap());
                }
            }
        }
        if root.exists() {
            walk(root, root, &mut out);
        }
        out
    }

    fn changed_files(a: &BTreeMap<String, Vec<u8>>, b: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
        let mut c = Vec::new();
        for (k, v) in b {
            if a.get(k) != Some(v) {
                c.push(k.clone());
            }
        }
        for k in a.keys() {
            if !b.contains_key(k) {
                c.push(format!("REMOVED:{k}"));
            }
        }
        c.sort();
        c
    }

    fn tmp() -> std::path::PathBuf {
        let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let d = std::env::temp_dir().join(format!("archv2_{n}"));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn deterministic_and_recrawl_diffs_only_timestamps() {
        let root = tmp();
        let items = vec![
            item("https://apple.com/a", "A", "hashA", 100, 100),
            item("https://apple.com/b", "B", "hashB", 100, 100),
        ];
        materialize_archive(&root, &items).unwrap();
        let snap1 = snapshot(&root.join("v2"));
        assert!(snap1.keys().any(|k| k.starts_with("data/")), "wrote article data files");
        assert!(snap1.contains_key("meta/timestamps.jsonl"));

        // 1) identical re-export -> byte-identical (zero diff)
        materialize_archive(&root, &items).unwrap();
        let snap2 = snapshot(&root.join("v2"));
        assert_eq!(changed_files(&snap1, &snap2), Vec::<String>::new(), "identical export must not change any file");

        // 2) re-crawl item A: only fetched_at/updated_at change (content_hash same)
        let mut re = items.clone();
        re[0].item.fetched_at = ts(999);
        re[0].item.updated_at = ts(999);
        materialize_archive(&root, &re).unwrap();
        let snap3 = snapshot(&root.join("v2"));
        assert_eq!(
            changed_files(&snap2, &snap3),
            vec!["meta/timestamps.jsonl".to_owned()],
            "a no-content re-crawl must diff ONLY the timestamps sidecar"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn duplicate_rows_pick_stable_first_seen_regardless_of_order() {
        // Same URL (=> same doc_id) crawled several times: duplicate rows differ
        // only in created_at. The data file must be byte-identical no matter what
        // order the DB hands the rows in, because first_seen_at is MIN(created_at)
        // and content comes from a deterministically chosen representative. This
        // is the regression guard for the observed first_seen_at flap that made
        // ~50 data files churn every export.
        let mk = |created: i64, fetched: i64| {
            let mut it = item("https://apple.com/dup", "Dup", "hashDup", fetched, fetched);
            it.item.created_at = ts(created);
            it.item.published_at = None; // force publish_date to fall back to first_seen
            it
        };

        let root_a = tmp();
        materialize_archive(&root_a, &[mk(300, 300), mk(100, 100), mk(200, 200)]).unwrap();
        let snap_a = snapshot(&root_a.join("v2"));

        // same three rows, shuffled order + an extra later duplicate
        let root_b = tmp();
        materialize_archive(&root_b, &[mk(200, 200), mk(400, 400), mk(100, 100), mk(300, 300)]).unwrap();
        let snap_b = snapshot(&root_b.join("v2"));

        let data_a: BTreeMap<_, _> = snap_a.iter().filter(|(k, _)| k.starts_with("data/")).collect();
        let data_b: BTreeMap<_, _> = snap_b.iter().filter(|(k, _)| k.starts_with("data/")).collect();
        assert_eq!(data_a, data_b, "data file must not depend on row order / duplicate count");

        // and first_seen_at must be the MIN created_at (100), rendered stably
        let (_, bytes) = data_a.iter().next().expect("one data file");
        let v: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(v["first_seen_at"], ts(100).to_rfc3339());

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    #[test]
    fn recent_and_company_indexes_carry_summary_rows_not_just_ids() {
        let root = tmp();
        let items = vec![item("https://apple.com/a", "Big News", "hashA", 100, 100)];
        materialize_archive(&root, &items).unwrap();
        let doc_id = document_id(&items[0]);

        // recent day file (located via the snapshot): articles[] with title/summary,
        // no body_markdown / fetched_at
        let snap = snapshot(&root.join("v2"));
        let (_, day_bytes) = snap
            .iter()
            .find(|(k, _)| k.starts_with("index/recent/2") && k.ends_with(".json"))
            .expect("a recent day file exists");
        let day: serde_json::Value = serde_json::from_slice(day_bytes).unwrap();
        let arts = day["articles"].as_array().expect("articles array");
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0]["doc_id"], doc_id);
        assert_eq!(arts[0]["title"], "Big News");
        assert!(arts[0].get("summary").is_some());
        assert!(arts[0].get("body_markdown").is_none(), "must NOT inline the body");
        assert!(arts[0].get("fetched_at").is_none(), "must NOT inline volatile fetched_at");

        // company month file: same summary rows under articles[]
        let (_, month_bytes) = snap
            .iter()
            .find(|(k, _)| k.starts_with("index/companies/apple/") && k.ends_with(".json"))
            .expect("a company month file exists");
        let month: serde_json::Value = serde_json::from_slice(month_bytes).unwrap();
        assert_eq!(month["articles"][0]["doc_id"], doc_id);
        assert_eq!(month["articles"][0]["title"], "Big News");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn taxonomy_directory_and_category_files_reflect_membership() {
        let root = tmp();
        // two companies in the same category "tech" (the item() default)
        let mut a = item("https://apple.com/a", "A", "hashA", 100, 100);
        a.company_key = "apple".to_owned();
        a.company_name = "Apple".to_owned();
        let mut g = item("https://google.com/g", "G", "hashG", 100, 100);
        g.company_key = "google".to_owned();
        g.company_name = "Google".to_owned();
        materialize_archive(&root, &[a, g]).unwrap();

        let dir: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("v2/taxonomy/directory.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(dir["category_count"], 1);
        assert_eq!(dir["company_count"], 2);
        assert_eq!(dir["categories"][0]["key"], "tech");
        assert_eq!(dir["categories"][0]["company_count"], 2);
        assert_eq!(dir["categories"][0]["record_count"], 2);

        let cat: serde_json::Value = serde_json::from_slice(
            &fs::read(root.join("v2/taxonomy/categories/tech.json")).unwrap(),
        )
        .unwrap();
        let companies = cat["companies"].as_array().unwrap();
        assert_eq!(companies.len(), 2);
        // sorted by company_key: apple before google
        assert_eq!(companies[0]["company_key"], "apple");
        assert_eq!(companies[1]["company_key"], "google");
        assert_eq!(companies[0]["record_count"], 1);
        assert!(companies[0]["last_published_at"].is_string());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn new_article_creates_files_without_touching_old() {
        let root = tmp();
        let base = vec![item("https://apple.com/a", "A", "hashA", 100, 100)];
        materialize_archive(&root, &base).unwrap();
        let snap1 = snapshot(&root.join("v2"));

        let mut more = base.clone();
        more.push(item("https://apple.com/c", "C", "hashC", 200, 200));
        materialize_archive(&root, &more).unwrap();
        let snap2 = snapshot(&root.join("v2"));

        let changed = changed_files(&snap1, &snap2);
        // A's data file must be untouched
        assert!(
            !changed.iter().any(|c| c.starts_with("data/") && c.contains(&document_id(&base[0]))),
            "adding an article must not rewrite an existing article's data file: {changed:?}"
        );
        // C's data file must be created
        assert!(
            changed.iter().any(|c| c.starts_with("data/") && c.contains(&document_id(&more[1]))),
            "new article data file must be created"
        );
        fs::remove_dir_all(&root).ok();
    }
}
