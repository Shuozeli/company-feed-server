# Database Design

The project uses Postgres as the only database. Local development runs Postgres through Docker Compose.

## Tables

### `companies`

Canonical company registry.

```text
id uuid primary key
ticker text unique
name text not null
homepage_url text
investor_relations_url text
metadata jsonb not null default '{}'
created_at timestamptz not null
updated_at timestamptz not null
```

### `sources`

Approved or disabled crawl sources.

```text
id uuid primary key
source_id text unique not null
company_id uuid references companies(id)
kind text not null                  -- rss | atom | html | browser
url text not null
status text not null                -- candidate | approved | disabled
freshness_slo_seconds int not null
browser_required boolean not null default false
public_export_allowed boolean not null default false
discovery_confidence double precision
metadata jsonb not null default '{}'
created_at timestamptz not null
updated_at timestamptz not null
```

### `source_candidates`

Discovery output before approval.

```text
id uuid primary key
company_id uuid references companies(id)
candidate_url text not null
candidate_kind text not null
confidence double precision not null
evidence jsonb not null default '{}'
status text not null                -- new | accepted | rejected
created_at timestamptz not null
updated_at timestamptz not null
```

### `feed_items`

Normalized company news items.

```text
id uuid primary key
company_id uuid references companies(id)
source_id uuid references sources(id)
external_id text not null
url text not null
canonical_url text not null
title text not null
summary text not null default ''
body_text text not null default ''
body_html text not null default ''
published_at timestamptz
fetched_at timestamptz not null
content_hash text not null
source_kind text not null
raw jsonb not null default '{}'
normalized jsonb not null default '{}'
created_at timestamptz not null
updated_at timestamptz not null
```

Dedup constraints:

```sql
UNIQUE (source_id, external_id)
UNIQUE (source_id, canonical_url)
UNIQUE (content_hash)
```

### `source_state`

Scheduler state.

```text
source_id uuid primary key references sources(id)
last_attempt_at timestamptz
last_success_at timestamptz
last_error text
consecutive_failures int not null default 0
backoff_until timestamptz
cursor jsonb not null default '{}'
updated_at timestamptz not null
```

### `crawl_runs`

Per-crawl execution record.

```text
id uuid primary key
source_id uuid references sources(id)
started_at timestamptz not null
finished_at timestamptz
status text not null                -- running | completed | failed
item_count int not null default 0
new_item_count int not null default 0
error text
metadata jsonb not null default '{}'
```

### `discovery_runs`

Per-company discovery record.

```text
id uuid primary key
company_id uuid references companies(id)
started_at timestamptz not null
finished_at timestamptz
status text not null
candidate_count int not null default 0
error text
metadata jsonb not null default '{}'
```

### `export_targets`

Configured Git export destinations.

```text
id uuid primary key
target_id text unique not null
repo_url text not null
local_path text not null
branch text not null default 'main'
format text not null                -- markdown_json | jsonl
layout text not null                -- by_company_date
enabled boolean not null default true
metadata jsonb not null default '{}'
created_at timestamptz not null
updated_at timestamptz not null
```

### `exported_items`

Idempotency state for Git exports.

```text
target_id uuid references export_targets(id)
feed_item_id uuid references feed_items(id)
exported_path text not null
exported_commit text
exported_at timestamptz not null
primary key (target_id, feed_item_id)
```

### `event_log`

Structured operational log.

```text
id bigserial primary key
event_type text not null
company_id uuid
source_id uuid
payload jsonb not null default '{}'
created_at timestamptz not null
```

