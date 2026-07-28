# Web Discovery Adapter Contract

This crate defines the optional, provider-neutral HTTP boundary used to obtain
public web-property suggestions for a company.

The open-source service sends public company identity and known public URLs to
`POST /v1/discover`. A separately operated adapter returns URL suggestions with
generic roles and ranking scores. Provider prompts, search queries, credentials,
raw responses, and provider-specific metadata are outside this repository.

Adapter output is never an approved source. Company Feed Server independently
fetches and validates every returned URL before it becomes a source candidate.

The adapter is disabled by default. Implementations should require
authentication, honor the `Idempotency-Key` header, bound their own concurrency,
and return `429 Retry-After` when saturated.
