#!/usr/bin/env bash
# Incremental, low-abuse-signal publish of the company-news-data export to GitHub.
#
# WHY THIS EXISTS: the old pub_push.sh did `git checkout --orphan` + `git add -A`
# (154k files / ~1GB) + `git push --force` on EVERY publish, rewriting the entire
# tree/history each time. Combined with using raw.githubusercontent as a CDN, that
# tripped GitHub abuse detection and got the datayuacx26 account SUSPENDED
# (2026-08-25, appeal ticket #4698709). NEVER orphan-force-push the whole tree.
#
# This script pushes ONLY the diff (normal fast-forward commit) and refuses to
# force. Run it INFREQUENTLY (daily / on-demand), not hourly.
#
# Usage:
#   PAT=<datayuacx26_pat> scripts/publish-company-news-data.sh
#   (run from inside the server container, or set CHECKOUT to the export path)
set -euo pipefail

CHECKOUT="${CHECKOUT:-/app/exports/company-news-data}"
BRANCH="${BRANCH:-main}"
: "${PAT:?PAT (datayuacx26 fine-grained token) must be set}"
PUSHURL="https://datayuacx26:${PAT}@github.com/datayuacx26/company-news-data.git"
GC="git -c user.name=datayuacx26 -c user.email=314377975+datayuacx26@users.noreply.github.com -c gc.auto=0"

cd "$CHECKOUT"

# 1) Commit whatever the exporter left in the working tree (a normal commit on top
#    of existing history -- NOT an orphan, NOT a full re-add of a new root).
if [ -n "$($GC status --porcelain)" ]; then
  $GC add -A
  $GC commit -qm "Publish export $(date -u +%FT%TZ)"
  echo "committed working-tree changes"
else
  echo "working tree clean; pushing existing HEAD"
fi

# 2) Fast-forward incremental push. Git sends only new objects since the remote tip.
#    No --force. If the remote has diverged (e.g. right after account restore, the
#    remote still holds the old orphan commit), this FAILS ON PURPOSE -- run the
#    one-time scripts/publish-baseline-oneshot.sh first, then use this script.
echo "pushing (incremental, no force) ..."
if $GC push "$PUSHURL" "HEAD:${BRANCH}"; then
  echo "PUBLISHED incrementally"
else
  echo "PUSH REJECTED (non-fast-forward). Do NOT force from here." >&2
  echo "If this is the first push after an account restore, run" >&2
  echo "  scripts/publish-baseline-oneshot.sh   (a rare, one-time reconciliation)" >&2
  exit 1
fi
