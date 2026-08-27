#!/usr/bin/env bash
# ONE-TIME baseline reconciliation for company-news-data.
#
# Use this EXACTLY ONCE after the datayuacx26 account is restored (or whenever the
# remote history has diverged from local and needs a single clean reset). It
# squashes the local export to a SINGLE commit and force-pushes it ONCE to
# establish a shared baseline, after which scripts/publish-company-news-data.sh
# does normal INCREMENTAL fast-forward pushes forever.
#
# This is the ONLY sanctioned force-push. Do NOT run it on a schedule. Repeated
# full-tree force-pushes are what got the account suspended (ticket #4698709).
#
# Usage:
#   PAT=<datayuacx26_pat> scripts/publish-baseline-oneshot.sh
set -euo pipefail

CHECKOUT="${CHECKOUT:-/app/exports/company-news-data}"
BRANCH="${BRANCH:-main}"
: "${PAT:?PAT (datayuacx26 fine-grained token) must be set}"
PUSHURL="https://datayuacx26:${PAT}@github.com/datayuacx26/company-news-data.git"
GC="git -c user.name=datayuacx26 -c user.email=314377975+datayuacx26@users.noreply.github.com -c gc.auto=0"

cd "$CHECKOUT"

read -r -p "This will squash local history to one commit and FORCE-PUSH once to $BRANCH. Continue? [y/N] " ans
[ "$ans" = "y" ] || { echo "aborted"; exit 1; }

# Squash to a single clean commit (keeps the current tree, drops all history so the
# remote stays tiny and future incremental pushes are fast-forwards).
$GC checkout -q --orphan _baseline
$GC add -A
$GC commit -qm "company-news-data baseline $(date -u +%FT%TZ)"
$GC branch -D "$BRANCH" 2>/dev/null || true
$GC branch -m "$BRANCH"
$GC reflog expire --expire=now --all || true
$GC gc --prune=now || true

echo "force-pushing the one-time baseline ..."
$GC push --force "$PUSHURL" "HEAD:${BRANCH}"
echo "BASELINE PUBLISHED. From now on use scripts/publish-company-news-data.sh (incremental)."
