#!/usr/bin/env python3
"""Validate dataset hashes, counts, ordering, paths, and navigation indexes."""

from __future__ import annotations

import hashlib
import json
from datetime import datetime
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
SUMMARY_KEYS = (
    "schema_version",
    "document_id",
    "company_key",
    "company_name",
    "source_id",
    "source_kind",
    "canonical_url",
    "title",
    "summary",
    "published_at",
    "fetched_at",
    "article_path",
    "record_path",
)


def load_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected a JSON object")
    return value


def resolve(relative: str) -> Path:
    path = Path(relative)
    if path.is_absolute() or ".." in path.parts:
        raise ValueError(f"unsafe relative path: {relative}")
    resolved = ROOT / path
    if not resolved.is_file():
        raise ValueError(f"missing file: {relative}")
    return resolved


def sha256_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def compact_summary(document: dict[str, Any]) -> dict[str, Any]:
    return {key: document[key] for key in SUMMARY_KEYS}


def company_bucket(company_key: str) -> str:
    return hashlib.sha256(company_key.encode("utf-8")).hexdigest()[:2]


def directory_bucket(company_name: str) -> str:
    stripped = company_name.lstrip()
    if not stripped:
        return "other"
    first = stripped[0]
    if first.isascii() and first.isalpha():
        return first.lower()
    if first.isascii() and first.isdigit():
        return "0-9"
    return "other"


def article_sort_key(item: dict[str, Any]) -> tuple[datetime, datetime, str]:
    published = item["published_at"] or item["fetched_at"]
    return (
        datetime.fromisoformat(published),
        datetime.fromisoformat(item["fetched_at"]),
        item["document_id"],
    )


def assert_newest_first(
    previous: tuple[datetime, datetime, str] | None,
    current: tuple[datetime, datetime, str],
    location: Path,
) -> None:
    if previous is None:
        return
    previous_time, previous_fetched, previous_id = previous
    current_time, current_fetched, current_id = current
    out_of_order = (
        current_time > previous_time
        or (
            current_time == previous_time
            and current_fetched > previous_fetched
        )
        or (
            current_time == previous_time
            and current_fetched == previous_fetched
            and current_id < previous_id
        )
    )
    if out_of_order:
        raise ValueError(f"{location}: article summaries are not newest-first")


def validate_browse_pages(
    generation: str,
    descriptors: list[dict[str, Any]],
    expected: dict[str, dict[str, Any]],
    expected_company: str | None = None,
) -> set[str]:
    page_ids: set[str] = set()
    previous_key: tuple[datetime, datetime, str] | None = None
    expected_page = 1
    for descriptor in descriptors:
        if descriptor["page"] != expected_page:
            raise ValueError("browse page numbers are not contiguous")
        expected_page += 1
        page_path = resolve(descriptor["path"])
        page_bytes = page_path.read_bytes()
        if len(page_bytes) != descriptor["byte_count"]:
            raise ValueError(f"{page_path}: byte count mismatch")
        if sha256_bytes(page_bytes) != descriptor["sha256"]:
            raise ValueError(f"{page_path}: digest mismatch")
        page = json.loads(page_bytes)
        if page["generation"] != generation:
            raise ValueError(f"{page_path}: generation mismatch")
        if page["page"] != descriptor["page"]:
            raise ValueError(f"{page_path}: page number mismatch")
        if page["record_count"] != descriptor["record_count"]:
            raise ValueError(f"{page_path}: descriptor count mismatch")
        if len(page["items"]) != page["record_count"]:
            raise ValueError(f"{page_path}: item count mismatch")
        if "body_text" in page:
            raise ValueError(f"{page_path}: browse page must not contain body text")

        for item in page["items"]:
            if "body_text" in item:
                raise ValueError(f"{page_path}: summary must not contain body text")
            document_id = item["document_id"]
            if document_id in page_ids:
                raise ValueError(f"{page_path}: duplicate document ID {document_id}")
            if document_id not in expected:
                raise ValueError(f"{page_path}: unknown document ID {document_id}")
            if item != expected[document_id]:
                raise ValueError(f"{page_path}: summary mismatch for {document_id}")
            if expected_company is not None and item["company_key"] != expected_company:
                raise ValueError(f"{page_path}: company mismatch")
            current_key = article_sort_key(item)
            assert_newest_first(previous_key, current_key, page_path)
            previous_key = current_key
            page_ids.add(document_id)
    return page_ids


def main() -> None:
    for schema in sorted((ROOT / "schemas").rglob("*.json")):
        load_json(schema)
    openapi = load_json(ROOT / "openapi/openapi.json")
    if openapi.get("openapi") != "3.1.2":
        raise ValueError("openapi/openapi.json: expected OpenAPI 3.1.2")

    data_index = load_json(ROOT / "index.json")
    if data_index["dataset"] != "company-news-data":
        raise ValueError("index.json: unexpected dataset")
    head = load_json(resolve(data_index["paths"]["head"]))
    manifest = load_json(resolve(data_index["paths"]["archive_manifest"]))
    if head["manifest_path"] != data_index["paths"]["archive_manifest"]:
        raise ValueError("index.json and HEAD.json manifest paths differ")
    if manifest["generation"] != head["generation"]:
        raise ValueError("HEAD and root manifest generations differ")
    if data_index["generation"] != head["generation"]:
        raise ValueError("index.json and HEAD.json generations differ")
    for path_key in ("openapi", "content_rights"):
        resolve(data_index["paths"][path_key])

    seen: set[str] = set()
    companies: set[str] = set()
    company_counts: dict[str, int] = {}
    company_documents: dict[str, set[str]] = {}
    summaries: dict[str, dict[str, Any]] = {}
    total_records = 0

    for partition_ref in manifest["partitions"]:
        partition_path = resolve(partition_ref["manifest_path"])
        partition_bytes = partition_path.read_bytes()
        if sha256_bytes(partition_bytes) != partition_ref["sha256"]:
            raise ValueError(f"{partition_path}: digest mismatch")
        partition = json.loads(partition_bytes)
        if partition["generation"] != head["generation"]:
            raise ValueError(f"{partition_path}: generation mismatch")

        partition_records = 0
        partition_bytes_total = 0
        for shard in partition["shards"]:
            shard_path = resolve(shard["path"])
            raw = shard_path.read_bytes()
            if len(raw) != shard["byte_count"]:
                raise ValueError(f"{shard_path}: byte count mismatch")
            if sha256_bytes(raw) != shard["sha256"]:
                raise ValueError(f"{shard_path}: digest mismatch")
            if raw and not raw.endswith(b"\n"):
                raise ValueError(f"{shard_path}: missing final newline")

            previous_id = ""
            line_count = 0
            for line_number, line in enumerate(raw.splitlines(), start=1):
                document = json.loads(line)
                document_id = document["document_id"]
                if document_id <= previous_id:
                    raise ValueError(
                        f"{shard_path}:{line_number}: document IDs are not sorted"
                    )
                if shard["prefix"] and not document_id.startswith(shard["prefix"]):
                    raise ValueError(
                        f"{shard_path}:{line_number}: prefix mismatch"
                    )
                if document_id in seen:
                    raise ValueError(f"duplicate document ID: {document_id}")
                seen.add(document_id)
                previous_id = document_id
                company_key = document["company_key"]
                companies.add(company_key)
                company_counts[company_key] = company_counts.get(company_key, 0) + 1
                company_documents.setdefault(company_key, set()).add(document_id)
                summaries[document_id] = compact_summary(document)

                record = load_json(resolve(document["record_path"]))
                if record["document_id"] != document_id:
                    raise ValueError(
                        f"{document['record_path']}: document ID mismatch"
                    )
                article_path = resolve(document["article_path"])
                article = article_path.read_bytes()
                if len(article) != record["content"]["bytes"]:
                    raise ValueError(f"{article_path}: byte count mismatch")
                if sha256_bytes(article) != record["content"]["sha256"]:
                    raise ValueError(f"{article_path}: digest mismatch")
                line_count += 1

            if line_count != shard["record_count"]:
                raise ValueError(f"{shard_path}: record count mismatch")
            partition_records += line_count
            partition_bytes_total += len(raw)

        if partition_records != partition["record_count"]:
            raise ValueError(f"{partition_path}: record count mismatch")
        if len(partition["shards"]) != partition["shard_count"]:
            raise ValueError(f"{partition_path}: shard count mismatch")
        if partition_bytes_total != partition["byte_count"]:
            raise ValueError(f"{partition_path}: byte count mismatch")
        if partition_records != partition_ref["record_count"]:
            raise ValueError(f"{partition_path}: root record count mismatch")
        total_records += partition_records

    if total_records != manifest["record_count"] or total_records != head["record_count"]:
        raise ValueError("root record count mismatch")
    if total_records != data_index["record_count"]:
        raise ValueError("index.json record count mismatch")
    if len(companies) != manifest["company_count"] or len(companies) != head["company_count"]:
        raise ValueError("root company count mismatch")
    if len(companies) != data_index["company_count"]:
        raise ValueError("index.json company count mismatch")

    recent = load_json(resolve(data_index["paths"]["recent_manifest"]))
    if recent["generation"] != head["generation"]:
        raise ValueError("recent manifest generation mismatch")
    if recent["record_count"] != total_records:
        raise ValueError("recent manifest record count mismatch")
    if recent["page_count"] != len(recent["pages"]):
        raise ValueError("recent manifest page count mismatch")
    recent_ids = validate_browse_pages(
        head["generation"], recent["pages"], summaries
    )
    if recent_ids != seen:
        raise ValueError("recent pages do not cover every document exactly once")

    directory = load_json(resolve(data_index["paths"]["company_directory_manifest"]))
    if directory["generation"] != head["generation"]:
        raise ValueError("company directory generation mismatch")
    if directory["company_count"] != len(companies):
        raise ValueError("company directory count mismatch")
    if directory["bucket_count"] != len(directory["buckets"]):
        raise ValueError("company directory bucket count mismatch")

    directory_entries: dict[str, dict[str, Any]] = {}
    previous_bucket = ""
    for descriptor in directory["buckets"]:
        bucket = descriptor["bucket"]
        if bucket <= previous_bucket:
            raise ValueError("company directory buckets are not sorted")
        previous_bucket = bucket
        bucket_path = resolve(descriptor["path"])
        bucket_bytes = bucket_path.read_bytes()
        if len(bucket_bytes) != descriptor["byte_count"]:
            raise ValueError(f"{bucket_path}: byte count mismatch")
        if sha256_bytes(bucket_bytes) != descriptor["sha256"]:
            raise ValueError(f"{bucket_path}: digest mismatch")
        page = json.loads(bucket_bytes)
        if page["generation"] != head["generation"] or page["bucket"] != bucket:
            raise ValueError(f"{bucket_path}: identity mismatch")
        if page["company_count"] != len(page["companies"]):
            raise ValueError(f"{bucket_path}: company count mismatch")
        if page["company_count"] != descriptor["company_count"]:
            raise ValueError(f"{bucket_path}: descriptor count mismatch")
        previous_name = ""
        for entry in page["companies"]:
            company_key = entry["company_key"]
            if company_key in directory_entries:
                raise ValueError(f"duplicate company directory entry: {company_key}")
            if directory_bucket(entry["company_name"]) != bucket:
                raise ValueError(f"{bucket_path}: company bucket mismatch")
            normalized_name = entry["company_name"].lower()
            if normalized_name < previous_name:
                raise ValueError(f"{bucket_path}: companies are not sorted")
            previous_name = normalized_name
            directory_entries[company_key] = entry

    if set(directory_entries) != companies:
        raise ValueError("company directory does not cover every company")

    for company_key in companies:
        company_path = (
            f"articles/v1/{company_bucket(company_key)}/{company_key}/company.json"
        )
        company = load_json(resolve(company_path))
        if company["record_count"] != company_counts[company_key]:
            raise ValueError(f"{company_key}: company record count mismatch")
        if sum(item["record_count"] for item in company["partitions"]) != company["record_count"]:
            raise ValueError(f"{company_key}: company partition count mismatch")
        article_index = company["article_index"]
        if article_index["generation"] != head["generation"]:
            raise ValueError(f"{company_key}: article index generation mismatch")
        if article_index["page_count"] != len(article_index["pages"]):
            raise ValueError(f"{company_key}: article page count mismatch")
        company_ids = validate_browse_pages(
            head["generation"],
            article_index["pages"],
            summaries,
            expected_company=company_key,
        )
        if company_ids != company_documents[company_key]:
            raise ValueError(f"{company_key}: article pages do not cover company")
        entry = directory_entries[company_key]
        if entry["company_manifest_path"] != company_path:
            raise ValueError(f"{company_key}: directory manifest path mismatch")
        if entry["record_count"] != company["record_count"]:
            raise ValueError(f"{company_key}: directory record count mismatch")

    print(
        f"validated generation {head['generation']} "
        f"({total_records} records, {len(companies)} companies)"
    )


if __name__ == "__main__":
    main()
