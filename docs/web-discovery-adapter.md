# Web Discovery Adapter

Company Feed Server can optionally request public URL suggestions from a
separately operated HTTP adapter. This is a narrow interoperability boundary,
not an embedded search or AI implementation.

## Ownership Boundary

The open-source service owns:

- the versioned neutral JSON contract
- the outbound HTTP client and bearer authentication
- idempotency headers and bounded retries
- response-size, schema, URL, role, rank, and count validation
- fetching every suggested URL with the public discovery client
- feed parsing, HTML inspection, candidate evidence, activation policy, and
  reversible operator decisions

The external adapter owns:

- how it researches the public web
- prompts, queries, models, provider SDKs, and credentials
- provider-specific rate limits and retries
- raw provider responses and private audit logs

The adapter may return multiple properties for one organization: corporate
newsrooms, engineering publications, research labs, and product or brand blogs.
The neutral roles describe why each public URL was suggested; they do not limit
a company to one website.

Only the company name, aliases, known public URLs, broad public classification,
requested roles, and public URL suggestions cross the boundary.

## Name-First Contract

The `company-web-discovery.v2` request deliberately has no ticker or exchange
field. Discovery uses `name` and `aliases`. This keeps the same path valid for a
private company with no public-market listing.

```text
POST /v1/discover
Authorization: Bearer <token>
Idempotency-Key: <request UUID>
Content-Type: application/json
```

Example request:

```json
{
  "schema_version": "company-web-discovery.v2",
  "request_id": "0ed99763-ab7c-47b0-92f5-0efb9f2f99fc",
  "company": {
    "company_id": "d9707700-55a8-43b5-a7d5-eab5243d03c0",
    "name": "Example",
    "aliases": ["Example Corporation"],
    "known_urls": ["https://example.com/"],
    "sector": "Technology",
    "industry": "Software"
  },
  "requested_roles": [
    "homepage",
    "investor_relations",
    "newsroom",
    "press_releases",
    "corporate_blog",
    "engineering_blog",
    "feed"
  ],
  "max_candidates": 20
}
```

Example response:

```json
{
  "schema_version": "company-web-discovery.v2",
  "request_id": "0ed99763-ab7c-47b0-92f5-0efb9f2f99fc",
  "candidates": [
    {
      "url": "https://example.com/newsroom",
      "role": "newsroom",
      "suggested_kind": "html",
      "rank_score": 0.6
    }
  ],
  "adapter_trace_id": "opaque-adapter-reference"
}
```

`rank_score` is adapter ordering information only. It does not become public
candidate confidence. The public discovery client makes a separate HTTP request
and derives evidence from the returned content. Adapter provenance is retained
separately so an explicitly configured validation policy can use it.

## Runtime Configuration

```text
WEB_DISCOVERY_ADAPTER_MODE=disabled|fallback|augment
WEB_DISCOVERY_ADAPTER_URL=
WEB_DISCOVERY_ADAPTER_TOKEN=
WEB_DISCOVERY_ADAPTER_TIMEOUT_SECONDS=90
WEB_DISCOVERY_ADAPTER_MAX_RESPONSE_BYTES=1048576
WEB_DISCOVERY_ADAPTER_MAX_CANDIDATES=20
DISCOVERY_ALLOW_PRIVATE_NETWORKS=false
```

The default is `disabled`, and no adapter URL is required in that mode.
`fallback` requests suggestions only for companies without configured entry
points. `augment` requests suggestions for every discovery run.

The client does not follow redirects at the adapter boundary, does not log the
bearer token, rejects mismatched request IDs and schemas, and treats `429` plus
`5xx` as retryable. Adapter error bodies are not persisted in discovery state.

The public URL validator resolves every candidate and redirect itself, disables
environment proxies, and rejects loopback, private, link-local, multicast,
documentation, benchmark, and other reserved address ranges. It also checks the
connected peer address to reduce DNS-rebinding risk.
`DISCOVERY_ALLOW_PRIVATE_NETWORKS=true` exists only for isolated local fixtures.

## Validation Path

```text
adapter suggestion
  -> URL shape validation
  -> bounded public HTTP fetch
  -> redirect and response checks
  -> RSS/Atom parse or same-site HTML inspection
  -> evidence-backed source candidate
  -> separate technical validation and trust signals
  -> strict activation, provisional trusted-adapter activation, or rejection
```

Locale copies and HTML query variants are canonicalized before persistence, and
keyword links must remain on the same company site. No adapter implementation
can directly create an approved source. Automatic activation is owned entirely
by the open-source validation worker.

The default `VALIDATION_ACTIVATION_POLICY=strict` requires independent
ownership, editorial-scope, freshness, locale, safe-scope, and feed-content
evidence. An operator may opt into `trusted_adapter`; in that mode adapter
provenance plus independently verified, non-empty titled RSS/Atom content is
enough for provisional activation. All failed strict signals remain visible,
and disabling a wrong source preserves the audit history.
