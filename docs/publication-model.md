<!-- agent-updated: 2026-08-25T00:00:00Z -->

# Publication Model

This document records the intended publication model for the public archive
this project generates, resolving the "publication model and third-party
rights" gate in [launch-readiness.md](launch-readiness.md). It is the
authoritative statement of what the public archive contains, the rights stance,
and how to request correction or removal.

## What the public archive contains

The public archive (`company-news-data`) is generated with the
`approved_public` export scope. For approved, currently valid RSS, Atom, HTML,
and browser sources it contains:

- factual company records (canonical corporate name, aliases, listings,
  sector); and
- per-article metadata (title, canonical URL, source/publisher, timestamps,
  and provenance/extraction evidence) together with normalized article text
  extracted from publicly accessible pages.

Every article retains its original canonical URL and source provenance, so a
reader can always reach the publisher's original.

## Rights stance

- The MIT license covers only this project's source code and project-authored
  documentation. It grants no rights over third-party content. See
  [data-and-content-policy.md](data-and-content-policy.md).
- The project claims no ownership of, and grants no license to, third-party
  article text, headlines, images, logos, or trademarks. Those remain the
  property of their respective owners.
- Selection under the `approved_public` scope is an operational control, not a
  determination of ownership, permission, licensing, or fair use. Anyone who
  runs this software is solely responsible for ensuring their own crawling and
  redistribution comply with applicable website terms, robots directives,
  copyright and database rights, and privacy law in their jurisdiction.

## Correction and rights-removal route

Requests are monitored and actioned. The public archive lives at
[`datayuacx26/company-news-data`](https://github.com/datayuacx26/company-news-data):

- **Metadata correction** (wrong company, wrong link, misclassification): open
  the [data-correction form](https://github.com/datayuacx26/company-news-data/issues/new?template=data-correction.yml).
- **Rights / removal** (copyright, licensing, attribution, trademark, or a
  request to remove specific material): open the
  [rights form](https://github.com/datayuacx26/company-news-data/issues/new?template=rights.yml).
  Do not include confidential legal material in a public issue.
- **Confidential legal correspondence** (for example a formal takedown notice):
  use this repository's
  [private advisory form](https://github.com/Shuozeli/company-feed-server/security/advisories/new),
  which is a private channel to the maintainers.

Maintainers aim to acknowledge a complete removal request within five business
days and to remove or reduce the affected material to metadata promptly on a
valid request. Urgent removals are applied to the published archive directly;
because the archive is regenerated from source, excluding the source or record
also propagates the removal to every subsequent generation.

## Corpus review

The live corpus is generated only from approved sources under the scope above.
Company display names are canonical corporate names (security-level suffixes
such as "Common Stock" or share-class labels are removed and retained as
aliases), and multiple share classes of one legal entity are presented as a
single company. Rights or provenance conflicts discovered after publication are
handled through the routes above and by excluding the affected source or record
from subsequent generations.

## Publishing to GitHub -- INCREMENTAL ONLY (do not orphan-force-push)

The public archive lives at `datayuacx26/company-news-data` and is read by the
`company-news-ui` SPA over `raw.githubusercontent.com`. The exporter writes/commits
the export LOCALLY only (`export_targets.yaml` sets `push_enabled: false`); pushing
to GitHub is a separate, deliberate step.

**Incident (2026-08-25):** the old `pub_push.sh` did `git checkout --orphan` +
`git add -A` (~154k files / ~1GB) + `git push --force` on every publish, rewriting
the whole tree/history each time, while `raw.githubusercontent` was used as a
high-frequency CDN. GitHub's abuse detection SUSPENDED the `datayuacx26` account
(all repos + raw 404 anonymously). Appeal ticket **#4698709** was filed from
`yuanchenxi2026@gmail.com` (the account email; logged in on alienware Chrome CDP
`:9403`). See memory `datayuacx26-github-suspended-incident`.

**Rules:**
- **Never orphan-force-push the whole tree.** Publish with
  `scripts/publish-company-news-data.sh` -- a normal commit + fast-forward
  `git push` that sends only the diff and refuses `--force`.
- The ONLY sanctioned force-push is `scripts/publish-baseline-oneshot.sh`, run
  EXACTLY ONCE after an account restore (or a needed history reset) to establish a
  single-commit baseline; after that, incremental only.
- **Push infrequently** (daily / on-demand, not hourly). Keep `.git` lean (periodic
  gc, not per-push). Do not use `raw.githubusercontent` as a high-frequency CDN.
- While the account is suspended, do NOT push to / call datayuacx26 at all.
