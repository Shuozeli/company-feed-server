# Show HN Human-Writing and Launch Preflight

This is a factual worksheet and checklist, not draft submission copy. Do not
paste wording from this file into Hacker News.

Before launch, re-read the official [Show HN
guidelines](https://news.ycombinator.com/showhn.html) and [Hacker News
guidelines](https://news.ycombinator.com/newsguidelines.html); they can change.
As of this review, Show HN is for work the submitter personally made and that
readers can try, and the general guidelines prohibit generated or AI-edited
comments and solicitation of votes, comments, or submissions.

The founder must write the submission title, introductory comment, and every
reply personally, from scratch, without generated text or AI editing. This
worksheet supplies verification prompts only.

## Do Not Launch Until

- [ ] The publication/rights model in
      [`launch-readiness.md`](launch-readiness.md) is resolved.
- [ ] The unbounded Git archive scale problem is resolved and the documented
      consumer path works from a fresh machine.
- [ ] Company display names and keys represent corporate entities rather than
      duplicate security or share-class records.
- [ ] The public reader works while logged out, without signup or an email gate.
- [ ] A clean clone follows the README quick start successfully.
- [ ] The release commit has passing CI, security, schema, archive-validator,
      and secret-scan evidence.
- [ ] Public repositories contain no private endpoints, credentials, database
      dumps, operational handoffs, or private provider material.
- [ ] Security, bug, provenance, and rights-correction routes are visible and
      monitored.
- [ ] The founder can remain available to answer technical questions after
      submitting.

## Facts for the Founder to Verify

Answer these privately in notes. Do not turn the prompts or repository wording
into copy automatically.

- What firsthand problem led to building a company-first news stack?
- What can a visitor actually try today, without credentials?
- Which repository should the submission link open, and why is that the most
  useful starting point?
- What does “name-first” change for private companies and multi-publication
  companies compared with ticker-based aggregation?
- Which parts are deterministic open-source code, which are optional adapter
  contracts, and which are generated data?
- What did the founder personally design or implement?
- What tradeoff was hardest: source correctness, content freshness, durable
  jobs, static indexing, publication policy, or archive scale?
- What is still incomplete or deliberately out of scope?
- What specific feedback would materially improve the project?
- Can every numerical or performance claim be reproduced from the linked
  release commit or public artifact?

Repository facts to re-check on launch day:

- Rust/Postgres server with separate API, discovery, validation, feed crawl,
  recipe-build, content-hydration, and export runtimes.
- Company identity is based on names and stable company keys; stock listings are
  optional metadata, not identity or search input.
- RSS/Atom is preferred, with validated public-HTML recipes as fallback.
- Static consumers start from bounded `index.json` directories rather than
  loading the full archive.
- The server, generated data, and Vite/React reader are separate repositories.
- The deployment is self-hosted and operator endpoints require an external
  trusted-network or authentication boundary.
- Export selection does not establish rights to redistribute publisher
  material.

## Submission Check

- [ ] The founder chose the destination only after confirming it is immediately
      usable and represents the project honestly.
- [ ] The founder wrote a plain factual title by hand and prefixed it as required
      by the current Show HN guidelines.
- [ ] The founder wrote any introductory comment by hand, including motivation,
      architecture, limitations, and the requested feedback.
- [ ] The title contains no superlatives, urgency, gratuitous capitalization, or
      unsupported metrics.
- [ ] The submission is not a landing page, fundraiser, signup gate, newsletter,
      or announcement for something readers cannot try.
- [ ] Known publication-rights, scale, platform, and authentication limitations
      are disclosed rather than hidden.
- [ ] All links work in a logged-out browser and no link exposes a private
      Tailscale address.

## No Vote or Comment Solicitation

Do not ask friends, coworkers, investors, customers, communities, mailing
lists, social followers, or automated agents to upvote, comment on, or submit
the project. Do not coordinate voting or seed supportive comments. Do not offer
rewards or reciprocal engagement.

Operational announcements may state that the project is available, but must not
request or imply Hacker News voting or commenting. When uncertain, do not send
the announcement and consult the current official guidelines.

## Response Checklist

- [ ] The founder writes every Hacker News response personally without generated
      or AI-edited text.
- [ ] Answer from direct knowledge; say when a fact needs checking.
- [ ] Lead with the concrete behavior, evidence, or tradeoff behind a decision.
- [ ] Acknowledge reproducible bugs and record them in the appropriate public
      issue tracker.
- [ ] Move vulnerability details to the private security route.
- [ ] Route provenance or rights concerns to the generated archive's correction
      process and preserve the canonical URL involved.
- [ ] Do not argue about votes, ranking, flags, suspected promotion, or other
      users' motives.
- [ ] Do not delete and repost merely because the submission received little
      attention; consult the current guidelines first.
- [ ] Keep a factual list of follow-up fixes, but do not promise dates that have
      not been planned.

## After the Thread

- [ ] Summarize actionable technical feedback in project issues using the
      founder's own words.
- [ ] Prioritize security, correctness, provenance, and data-removal reports
      before feature requests.
- [ ] Update documentation when feedback reveals a misleading claim.
- [ ] Preserve the release commit and validation evidence referenced during the
      discussion.
- [ ] Do not use voter or commenter identities for unsolicited marketing.
