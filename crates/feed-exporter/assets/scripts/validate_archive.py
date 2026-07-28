#!/usr/bin/env python3
"""Validate archive hashes, counts, ordering, paths, and cross-references."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]


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


def main() -> None:
    for schema in sorted((ROOT / "schemas").rglob("*.json")):
        load_json(schema)
    openapi = load_json(ROOT / "openapi/openapi.json")
    if openapi.get("openapi") != "3.1.2":
        raise ValueError("openapi/openapi.json: expected OpenAPI 3.1.2")

    head = load_json(ROOT / "HEAD.json")
    manifest = load_json(resolve(head["manifest_path"]))
    if manifest["generation"] != head["generation"]:
        raise ValueError("HEAD and root manifest generations differ")

    seen: set[str] = set()
    companies: set[str] = set()
    company_counts: dict[str, int] = {}
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
    if len(companies) != manifest["company_count"] or len(companies) != head["company_count"]:
        raise ValueError("root company count mismatch")

    for company_key in companies:
        bucket = hashlib.sha256(company_key.encode("utf-8")).hexdigest()[:2]
        company = load_json(resolve(f"articles/v1/{bucket}/{company_key}/company.json"))
        if company["record_count"] != company_counts[company_key]:
            raise ValueError(f"{company_key}: company record count mismatch")
        if sum(item["record_count"] for item in company["partitions"]) != company["record_count"]:
            raise ValueError(f"{company_key}: company partition count mismatch")

    print(
        f"validated generation {head['generation']} "
        f"({total_records} records, {len(companies)} companies)"
    )


if __name__ == "__main__":
    main()
