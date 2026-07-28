#!/usr/bin/env bash
set -euo pipefail

base_ref="${1:-origin/main}"
git rev-parse --verify "${base_ref}^{commit}" >/dev/null

violations="$(
  git diff --name-status --find-renames "${base_ref}...HEAD" -- migrations/ |
    awk '$1 !~ /^A/ { print }'
)"

if [[ -n "${violations}" ]]; then
  echo "Applied migrations are immutable; add a new numbered migration instead:"
  echo "${violations}"
  exit 1
fi
