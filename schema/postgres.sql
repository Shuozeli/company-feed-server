-- Company Feed Server PostgreSQL schema.
--
-- This file is the single source of truth for database structure. Apply it
-- declaratively with pg-schema-diff; do not add sequential migration files.

CREATE OR REPLACE FUNCTION public.public_url_identity_key(value text)
 RETURNS text
 LANGUAGE sql
 IMMUTABLE PARALLEL SAFE STRICT
AS $function$
WITH fragmentless AS (
    SELECT split_part(value, '#', 1) AS value
), parts AS (
    SELECT
        split_part(value, '?', 1) AS resource,
        CASE
            WHEN position('?' IN value) > 0
                THEN substring(value FROM position('?' IN value))
            ELSE ''
        END AS query
    FROM fragmentless
), normalized_resource AS (
    SELECT
        rtrim(
            regexp_replace(resource, '^https?://(www\.)?', '', 'i'),
            '/'
        ) AS resource,
        query
    FROM parts
)
SELECT
    lower(split_part(resource, '/', 1))
        || substring(resource FROM length(split_part(resource, '/', 1)) + 1)
        || query
FROM normalized_resource
$function$
;

CREATE OR REPLACE FUNCTION public.set_updated_at()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$function$
;

CREATE SEQUENCE "public"."event_log_id_seq"
	AS bigint
	INCREMENT BY 1
	MINVALUE 1 MAXVALUE 9223372036854775807
	START WITH 1 CACHE 1 NO CYCLE
;

CREATE TABLE "public"."candidate_decisions" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"candidate_id" uuid NOT NULL,
	"source_id" uuid,
	"decision" text COLLATE "pg_catalog"."default" NOT NULL,
	"decision_mode" text COLLATE "pg_catalog"."default" NOT NULL,
	"actor" text COLLATE "pg_catalog"."default" NOT NULL,
	"reason" text COLLATE "pg_catalog"."default" NOT NULL,
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."candidate_decisions" ADD CONSTRAINT "candidate_decisions_actor_not_blank" CHECK((btrim(actor) <> ''::text));

ALTER TABLE "public"."candidate_decisions" ADD CONSTRAINT "candidate_decisions_decision_valid" CHECK((decision = ANY (ARRAY['activated'::text, 'rejected'::text, 'kept_for_review'::text])));

ALTER TABLE "public"."candidate_decisions" ADD CONSTRAINT "candidate_decisions_mode_valid" CHECK((decision_mode = ANY (ARRAY['automatic'::text, 'operator'::text])));

ALTER TABLE "public"."candidate_decisions" ADD CONSTRAINT "candidate_decisions_reason_not_blank" CHECK((btrim(reason) <> ''::text));

CREATE UNIQUE INDEX candidate_decisions_pkey ON public.candidate_decisions USING btree (id);

ALTER TABLE "public"."candidate_decisions" ADD CONSTRAINT "candidate_decisions_pkey" PRIMARY KEY USING INDEX "candidate_decisions_pkey";

CREATE INDEX candidate_decisions_candidate_created_idx ON public.candidate_decisions USING btree (candidate_id, created_at DESC, id DESC);

CREATE TABLE "public"."candidate_validation_runs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"candidate_id" uuid NOT NULL,
	"job_id" uuid,
	"started_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"finished_at" timestamp with time zone,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'running'::text NOT NULL,
	"detected_kind" text COLLATE "pg_catalog"."default",
	"final_url" text COLLATE "pg_catalog"."default",
	"http_status" integer,
	"item_count" integer DEFAULT 0 NOT NULL,
	"titled_item_count" integer DEFAULT 0 NOT NULL,
	"latest_item_at" timestamp with time zone,
	"policy_reasons" jsonb DEFAULT '[]'::jsonb NOT NULL,
	"error" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL
);

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_finished_state" CHECK((((status = 'running'::text) AND (finished_at IS NULL)) OR ((status <> 'running'::text) AND (finished_at IS NOT NULL))));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_http_status_valid" CHECK(((http_status IS NULL) OR ((http_status >= 100) AND (http_status <= 599))));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_item_count_nonnegative" CHECK((item_count >= 0));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_kind_valid" CHECK(((detected_kind IS NULL) OR (detected_kind = ANY (ARRAY['rss'::text, 'atom'::text]))));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_policy_reasons_array" CHECK((jsonb_typeof(policy_reasons) = 'array'::text));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_status_valid" CHECK((status = ANY (ARRAY['running'::text, 'valid'::text, 'needs_review'::text, 'invalid'::text, 'failed'::text, 'cancelled'::text])));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_titled_count_bounded" CHECK((titled_item_count <= item_count));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_titled_count_nonnegative" CHECK((titled_item_count >= 0));

CREATE UNIQUE INDEX candidate_validation_runs_pkey ON public.candidate_validation_runs USING btree (id);

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_pkey" PRIMARY KEY USING INDEX "candidate_validation_runs_pkey";

CREATE INDEX candidate_validation_runs_candidate_started_idx ON public.candidate_validation_runs USING btree (candidate_id, started_at DESC, id DESC);

CREATE UNIQUE INDEX candidate_validation_runs_one_running_idx ON public.candidate_validation_runs USING btree (candidate_id) WHERE (status = 'running'::text);

CREATE TABLE "public"."companies" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"name" text COLLATE "pg_catalog"."default" NOT NULL,
	"homepage_url" text COLLATE "pg_catalog"."default",
	"investor_relations_url" text COLLATE "pg_catalog"."default",
	"newsroom_url" text COLLATE "pg_catalog"."default",
	"blog_url" text COLLATE "pg_catalog"."default",
	"hints" jsonb DEFAULT '[]'::jsonb NOT NULL,
	"discovery_cadence_seconds" integer DEFAULT 604800 NOT NULL,
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"discovery_enabled" boolean DEFAULT true NOT NULL,
	"discovery_not_before" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"name_source" text COLLATE "pg_catalog"."default" DEFAULT 'operator'::text NOT NULL,
	"company_key" text COLLATE "pg_catalog"."default" NOT NULL,
	"aliases" jsonb DEFAULT '[]'::jsonb NOT NULL,
	"ownership_status" text COLLATE "pg_catalog"."default" DEFAULT 'unknown'::text NOT NULL,
	"lifecycle_status" text COLLATE "pg_catalog"."default" DEFAULT 'unknown'::text NOT NULL
);

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_aliases_array" CHECK((jsonb_typeof(aliases) = 'array'::text));

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_company_key_format" CHECK((company_key ~ '^[a-z0-9]+(-[a-z0-9]+)*$'::text));

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_company_key_not_blank" CHECK((btrim(company_key) <> ''::text));

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_discovery_cadence_positive" CHECK((discovery_cadence_seconds > 0));

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_lifecycle_status_valid" CHECK((lifecycle_status = ANY (ARRAY['active'::text, 'acquired'::text, 'inactive'::text, 'unknown'::text])));

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_name_not_blank" CHECK((btrim(name) <> ''::text));

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_name_source_valid" CHECK((name_source = ANY (ARRAY['seed_config'::text, 'universe'::text, 'operator'::text])));

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_ownership_status_valid" CHECK((ownership_status = ANY (ARRAY['public'::text, 'private'::text, 'state_owned'::text, 'cooperative'::text, 'unknown'::text])));

CREATE UNIQUE INDEX companies_company_key_unique ON public.companies USING btree (company_key);

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_company_key_unique" UNIQUE USING INDEX "companies_company_key_unique";

CREATE UNIQUE INDEX companies_pkey ON public.companies USING btree (id);

ALTER TABLE "public"."companies" ADD CONSTRAINT "companies_pkey" PRIMARY KEY USING INDEX "companies_pkey";

CREATE INDEX companies_discovery_schedule_idx ON public.companies USING btree (discovery_not_before, company_key) WHERE discovery_enabled;

CREATE INDEX companies_name_lower_idx ON public.companies USING btree (lower(name));

CREATE TRIGGER companies_set_updated_at BEFORE UPDATE ON public.companies FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."company_external_ids" (
	"source_name" text COLLATE "pg_catalog"."default" NOT NULL,
	"source_company_id" text COLLATE "pg_catalog"."default" NOT NULL,
	"company_id" uuid NOT NULL,
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."company_external_ids" ADD CONSTRAINT "company_external_ids_metadata_object" CHECK((jsonb_typeof(metadata) = 'object'::text));

ALTER TABLE "public"."company_external_ids" ADD CONSTRAINT "company_external_ids_source_not_blank" CHECK((btrim(source_name) <> ''::text));

ALTER TABLE "public"."company_external_ids" ADD CONSTRAINT "company_external_ids_value_not_blank" CHECK((btrim(source_company_id) <> ''::text));

ALTER TABLE "public"."company_external_ids" ADD CONSTRAINT "company_external_ids_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_external_ids" VALIDATE CONSTRAINT "company_external_ids_company_id_fkey";

CREATE UNIQUE INDEX company_external_ids_pkey ON public.company_external_ids USING btree (source_name, source_company_id);

ALTER TABLE "public"."company_external_ids" ADD CONSTRAINT "company_external_ids_pkey" PRIMARY KEY USING INDEX "company_external_ids_pkey";

CREATE INDEX company_external_ids_company_idx ON public.company_external_ids USING btree (company_id);

CREATE TRIGGER company_external_ids_set_updated_at BEFORE UPDATE ON public.company_external_ids FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."company_import_rows" (
	"import_run_id" uuid NOT NULL,
	"row_number" integer NOT NULL,
	"action" text COLLATE "pg_catalog"."default" NOT NULL,
	"company_id" uuid NOT NULL,
	"source_record" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"source_company_id" text COLLATE "pg_catalog"."default" NOT NULL,
	"company_key" text COLLATE "pg_catalog"."default" NOT NULL,
	"company_name" text COLLATE "pg_catalog"."default" NOT NULL
);

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_action_valid" CHECK((action = ANY (ARRAY['inserted'::text, 'updated'::text, 'unchanged'::text])));

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_company_key_not_blank" CHECK((btrim(company_key) <> ''::text));

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_company_name_not_blank" CHECK((btrim(company_name) <> ''::text));

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_number_positive" CHECK((row_number > 0));

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_source_id_not_blank" CHECK((btrim(source_company_id) <> ''::text));

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_import_rows" VALIDATE CONSTRAINT "company_import_rows_company_id_fkey";

CREATE UNIQUE INDEX company_import_rows_pkey ON public.company_import_rows USING btree (import_run_id, row_number);

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_pkey" PRIMARY KEY USING INDEX "company_import_rows_pkey";

CREATE UNIQUE INDEX company_import_rows_source_id_unique ON public.company_import_rows USING btree (import_run_id, source_company_id);

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_source_id_unique" UNIQUE USING INDEX "company_import_rows_source_id_unique";

CREATE INDEX company_import_rows_company_idx ON public.company_import_rows USING btree (company_id, import_run_id);

CREATE TABLE "public"."company_import_runs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"source_name" text COLLATE "pg_catalog"."default" NOT NULL,
	"source_revision" text COLLATE "pg_catalog"."default",
	"input_sha256" text COLLATE "pg_catalog"."default" NOT NULL,
	"input_bytes" bigint NOT NULL,
	"input_rows" integer NOT NULL,
	"inserted_rows" integer NOT NULL,
	"updated_rows" integer NOT NULL,
	"unchanged_rows" integer NOT NULL,
	"activate_new" boolean NOT NULL,
	"discovery_cadence_seconds" integer NOT NULL,
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"completed_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_cadence_positive" CHECK((discovery_cadence_seconds > 0));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_input_bytes_positive" CHECK((input_bytes > 0));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_input_rows_positive" CHECK((input_rows > 0));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_inserted_rows_nonnegative" CHECK((inserted_rows >= 0));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_row_counts_consistent" CHECK((input_rows = ((inserted_rows + updated_rows) + unchanged_rows)));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_sha256_valid" CHECK((input_sha256 ~ '^[0-9a-f]{64}$'::text));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_source_not_blank" CHECK((btrim(source_name) <> ''::text));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_unchanged_rows_nonnegative" CHECK((unchanged_rows >= 0));

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_updated_rows_nonnegative" CHECK((updated_rows >= 0));

CREATE UNIQUE INDEX company_import_runs_pkey ON public.company_import_runs USING btree (id);

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_pkey" PRIMARY KEY USING INDEX "company_import_runs_pkey";

CREATE UNIQUE INDEX company_import_runs_source_name_input_sha256_key ON public.company_import_runs USING btree (source_name, input_sha256);

ALTER TABLE "public"."company_import_runs" ADD CONSTRAINT "company_import_runs_source_name_input_sha256_key" UNIQUE USING INDEX "company_import_runs_source_name_input_sha256_key";

CREATE INDEX company_import_runs_created_idx ON public.company_import_runs USING btree (created_at DESC);

ALTER TABLE "public"."company_import_rows" ADD CONSTRAINT "company_import_rows_import_run_id_fkey" FOREIGN KEY (import_run_id) REFERENCES company_import_runs(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_import_rows" VALIDATE CONSTRAINT "company_import_rows_import_run_id_fkey";

CREATE TABLE "public"."company_listings" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"company_id" uuid NOT NULL,
	"ticker" text COLLATE "pg_catalog"."default" NOT NULL,
	"exchange" text COLLATE "pg_catalog"."default" DEFAULT ''::text NOT NULL,
	"is_primary" boolean DEFAULT false NOT NULL,
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."company_listings" ADD CONSTRAINT "company_listings_metadata_object" CHECK((jsonb_typeof(metadata) = 'object'::text));

ALTER TABLE "public"."company_listings" ADD CONSTRAINT "company_listings_ticker_not_blank" CHECK((btrim(ticker) <> ''::text));

ALTER TABLE "public"."company_listings" ADD CONSTRAINT "company_listings_ticker_uppercase" CHECK((ticker = upper(ticker)));

ALTER TABLE "public"."company_listings" ADD CONSTRAINT "company_listings_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_listings" VALIDATE CONSTRAINT "company_listings_company_id_fkey";

CREATE UNIQUE INDEX company_listings_company_id_ticker_exchange_key ON public.company_listings USING btree (company_id, ticker, exchange);

ALTER TABLE "public"."company_listings" ADD CONSTRAINT "company_listings_company_id_ticker_exchange_key" UNIQUE USING INDEX "company_listings_company_id_ticker_exchange_key";

CREATE UNIQUE INDEX company_listings_pkey ON public.company_listings USING btree (id);

ALTER TABLE "public"."company_listings" ADD CONSTRAINT "company_listings_pkey" PRIMARY KEY USING INDEX "company_listings_pkey";

CREATE INDEX company_listings_company_idx ON public.company_listings USING btree (company_id, is_primary DESC, ticker);

CREATE UNIQUE INDEX company_listings_one_primary_idx ON public.company_listings USING btree (company_id) WHERE is_primary;

CREATE INDEX company_listings_ticker_idx ON public.company_listings USING btree (ticker);

CREATE TRIGGER company_listings_set_updated_at BEFORE UPDATE ON public.company_listings FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."company_news_extraction_runs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"company_id" uuid NOT NULL,
	"job_id" uuid,
	"window_start" timestamp with time zone NOT NULL,
	"window_end" timestamp with time zone NOT NULL,
	"started_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"finished_at" timestamp with time zone,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'running'::text NOT NULL,
	"suggested_url_count" integer DEFAULT 0 NOT NULL,
	"accepted_url_count" integer DEFAULT 0 NOT NULL,
	"rejected_url_count" integer DEFAULT 0 NOT NULL,
	"source_count" integer DEFAULT 0 NOT NULL,
	"normalized_item_count" integer DEFAULT 0 NOT NULL,
	"new_item_count" integer DEFAULT 0 NOT NULL,
	"error" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL
);

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_counts_nonnegative" CHECK(((suggested_url_count >= 0) AND (accepted_url_count >= 0) AND (rejected_url_count >= 0) AND (source_count >= 0) AND (normalized_item_count >= 0) AND (new_item_count >= 0)));

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_finished_state" CHECK((((status = 'running'::text) AND (finished_at IS NULL)) OR ((status <> 'running'::text) AND (finished_at IS NOT NULL))));

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_items_bounded" CHECK(((new_item_count <= normalized_item_count) AND (normalized_item_count <= accepted_url_count)));

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_status_valid" CHECK((status = ANY (ARRAY['running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text])));

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_suggestions_bounded" CHECK(((accepted_url_count + rejected_url_count) <= suggested_url_count));

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_window_valid" CHECK((window_start < window_end));

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_news_extraction_runs" VALIDATE CONSTRAINT "company_news_extraction_runs_company_id_fkey";

CREATE UNIQUE INDEX company_news_extraction_runs_pkey ON public.company_news_extraction_runs USING btree (id);

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_pkey" PRIMARY KEY USING INDEX "company_news_extraction_runs_pkey";

CREATE INDEX company_news_extraction_runs_company_started_idx ON public.company_news_extraction_runs USING btree (company_id, started_at DESC, id DESC);

CREATE UNIQUE INDEX company_news_extraction_runs_one_running_job_idx ON public.company_news_extraction_runs USING btree (job_id) WHERE ((status = 'running'::text) AND (job_id IS NOT NULL));

CREATE INDEX company_news_extraction_runs_status_started_idx ON public.company_news_extraction_runs USING btree (status, started_at DESC);

CREATE TABLE "public"."company_news_recipe_runs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"recipe_id" uuid NOT NULL,
	"source_id" uuid NOT NULL,
	"job_id" uuid,
	"crawl_run_id" uuid,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'running'::text NOT NULL,
	"started_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"finished_at" timestamp with time zone,
	"discovered_url_count" integer DEFAULT 0 NOT NULL,
	"accepted_item_count" integer DEFAULT 0 NOT NULL,
	"rejected_url_count" integer DEFAULT 0 NOT NULL,
	"normalized_item_count" integer DEFAULT 0 NOT NULL,
	"new_item_count" integer DEFAULT 0 NOT NULL,
	"latest_published_at" timestamp with time zone,
	"acceptance_ratio_bps" integer DEFAULT 0 NOT NULL,
	"structure_fingerprint" text COLLATE "pg_catalog"."default",
	"reasons" jsonb DEFAULT '[]'::jsonb NOT NULL,
	"error" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL
);

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_counts_bounded" CHECK((((accepted_item_count + rejected_url_count) <= discovered_url_count) AND (normalized_item_count <= accepted_item_count) AND (new_item_count <= normalized_item_count)));

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_counts_nonnegative" CHECK(((discovered_url_count >= 0) AND (accepted_item_count >= 0) AND (rejected_url_count >= 0) AND (normalized_item_count >= 0) AND (new_item_count >= 0)));

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_finished_state" CHECK((((status = 'running'::text) AND (finished_at IS NULL)) OR ((status <> 'running'::text) AND (finished_at IS NOT NULL))));

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_ratio_valid" CHECK(((acceptance_ratio_bps >= 0) AND (acceptance_ratio_bps <= 10000)));

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_reasons_array" CHECK((jsonb_typeof(reasons) = 'array'::text));

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_status_valid" CHECK((status = ANY (ARRAY['running'::text, 'passed'::text, 'failed'::text, 'stale'::text, 'cancelled'::text])));

CREATE UNIQUE INDEX company_news_recipe_runs_pkey ON public.company_news_recipe_runs USING btree (id);

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_pkey" PRIMARY KEY USING INDEX "company_news_recipe_runs_pkey";

CREATE UNIQUE INDEX company_news_recipe_runs_one_running_job_idx ON public.company_news_recipe_runs USING btree (job_id) WHERE ((status = 'running'::text) AND (job_id IS NOT NULL));

CREATE INDEX company_news_recipe_runs_recipe_started_idx ON public.company_news_recipe_runs USING btree (recipe_id, started_at DESC);

CREATE INDEX company_news_recipe_runs_status_started_idx ON public.company_news_recipe_runs USING btree (status, started_at DESC);

CREATE TABLE "public"."company_news_recipe_state" (
	"recipe_id" uuid NOT NULL,
	"last_attempt_at" timestamp with time zone,
	"last_success_at" timestamp with time zone,
	"last_correct_at" timestamp with time zone,
	"last_nonempty_at" timestamp with time zone,
	"last_item_published_at" timestamp with time zone,
	"consecutive_failures" integer DEFAULT 0 NOT NULL,
	"consecutive_empty_runs" integer DEFAULT 0 NOT NULL,
	"consecutive_correctness_failures" integer DEFAULT 0 NOT NULL,
	"last_structure_fingerprint" text COLLATE "pg_catalog"."default",
	"freshness_status" text COLLATE "pg_catalog"."default" DEFAULT 'unknown'::text NOT NULL,
	"correctness_status" text COLLATE "pg_catalog"."default" DEFAULT 'unknown'::text NOT NULL,
	"rebuild_required" boolean DEFAULT false NOT NULL,
	"reason" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."company_news_recipe_state" ADD CONSTRAINT "company_news_recipe_state_correctness_valid" CHECK((correctness_status = ANY (ARRAY['unknown'::text, 'passing'::text, 'failing'::text])));

ALTER TABLE "public"."company_news_recipe_state" ADD CONSTRAINT "company_news_recipe_state_counts_nonnegative" CHECK(((consecutive_failures >= 0) AND (consecutive_empty_runs >= 0) AND (consecutive_correctness_failures >= 0)));

ALTER TABLE "public"."company_news_recipe_state" ADD CONSTRAINT "company_news_recipe_state_freshness_valid" CHECK((freshness_status = ANY (ARRAY['unknown'::text, 'fresh'::text, 'overdue'::text, 'content_stale'::text])));

CREATE UNIQUE INDEX company_news_recipe_state_pkey ON public.company_news_recipe_state USING btree (recipe_id);

ALTER TABLE "public"."company_news_recipe_state" ADD CONSTRAINT "company_news_recipe_state_pkey" PRIMARY KEY USING INDEX "company_news_recipe_state_pkey";

CREATE TRIGGER company_news_recipe_state_set_updated_at BEFORE UPDATE ON public.company_news_recipe_state FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."company_news_recipes" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"recipe_key" text COLLATE "pg_catalog"."default" NOT NULL,
	"company_id" uuid NOT NULL,
	"source_id" uuid NOT NULL,
	"version" integer NOT NULL,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'draft'::text NOT NULL,
	"schema_version" text COLLATE "pg_catalog"."default" NOT NULL,
	"spec" jsonb NOT NULL,
	"content_hash" text COLLATE "pg_catalog"."default" NOT NULL,
	"generated_by_run_id" uuid,
	"verified_at" timestamp with time zone,
	"stale_at" timestamp with time zone,
	"stale_reason" text COLLATE "pg_catalog"."default",
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_hash_not_blank" CHECK((btrim(content_hash) <> ''::text));

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_key_not_blank" CHECK((btrim(recipe_key) <> ''::text));

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_schema_not_blank" CHECK((btrim(schema_version) <> ''::text));

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_stale_state" CHECK((((status = 'stale'::text) AND (stale_at IS NOT NULL) AND (stale_reason IS NOT NULL)) OR (status <> 'stale'::text)));

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_status_valid" CHECK((status = ANY (ARRAY['draft'::text, 'active'::text, 'stale'::text, 'superseded'::text, 'disabled'::text])));

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_version_positive" CHECK((version > 0));

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_news_recipes" VALIDATE CONSTRAINT "company_news_recipes_company_id_fkey";

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_generated_by_run_id_fkey" FOREIGN KEY (generated_by_run_id) REFERENCES company_news_extraction_runs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."company_news_recipes" VALIDATE CONSTRAINT "company_news_recipes_generated_by_run_id_fkey";

CREATE UNIQUE INDEX company_news_recipes_pkey ON public.company_news_recipes USING btree (id);

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_pkey" PRIMARY KEY USING INDEX "company_news_recipes_pkey";

CREATE UNIQUE INDEX company_news_recipes_recipe_key_version_key ON public.company_news_recipes USING btree (recipe_key, version);

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_recipe_key_version_key" UNIQUE USING INDEX "company_news_recipes_recipe_key_version_key";

CREATE INDEX company_news_recipes_company_status_idx ON public.company_news_recipes USING btree (company_id, status, updated_at DESC);

CREATE UNIQUE INDEX company_news_recipes_one_active_source_idx ON public.company_news_recipes USING btree (source_id) WHERE (status = 'active'::text);

CREATE INDEX company_news_recipes_source_version_idx ON public.company_news_recipes USING btree (source_id, version DESC);

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_recipe_id_fkey" FOREIGN KEY (recipe_id) REFERENCES company_news_recipes(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_news_recipe_runs" VALIDATE CONSTRAINT "company_news_recipe_runs_recipe_id_fkey";

ALTER TABLE "public"."company_news_recipe_state" ADD CONSTRAINT "company_news_recipe_state_recipe_id_fkey" FOREIGN KEY (recipe_id) REFERENCES company_news_recipes(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_news_recipe_state" VALIDATE CONSTRAINT "company_news_recipe_state_recipe_id_fkey";

CREATE TRIGGER company_news_recipes_set_updated_at BEFORE UPDATE ON public.company_news_recipes FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."crawl_runs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"source_id" uuid NOT NULL,
	"job_id" uuid,
	"started_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"finished_at" timestamp with time zone,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'running'::text NOT NULL,
	"item_count" integer DEFAULT 0 NOT NULL,
	"new_item_count" integer DEFAULT 0 NOT NULL,
	"error" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL
);

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_finished_state" CHECK((((status = 'running'::text) AND (finished_at IS NULL)) OR ((status <> 'running'::text) AND (finished_at IS NOT NULL))));

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_item_count_nonnegative" CHECK((item_count >= 0));

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_new_item_count_bounded" CHECK((new_item_count <= item_count));

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_new_item_count_nonnegative" CHECK((new_item_count >= 0));

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_status_valid" CHECK((status = ANY (ARRAY['running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text])));

CREATE UNIQUE INDEX crawl_runs_pkey ON public.crawl_runs USING btree (id);

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_pkey" PRIMARY KEY USING INDEX "crawl_runs_pkey";

CREATE INDEX crawl_runs_source_started_idx ON public.crawl_runs USING btree (source_id, started_at DESC);

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_crawl_run_id_fkey" FOREIGN KEY (crawl_run_id) REFERENCES crawl_runs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."company_news_recipe_runs" VALIDATE CONSTRAINT "company_news_recipe_runs_crawl_run_id_fkey";

CREATE TABLE "public"."discovery_runs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"company_id" uuid NOT NULL,
	"job_id" uuid,
	"started_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"finished_at" timestamp with time zone,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'running'::text NOT NULL,
	"candidate_count" integer DEFAULT 0 NOT NULL,
	"error" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL
);

ALTER TABLE "public"."discovery_runs" ADD CONSTRAINT "discovery_runs_candidate_count_nonnegative" CHECK((candidate_count >= 0));

ALTER TABLE "public"."discovery_runs" ADD CONSTRAINT "discovery_runs_finished_state" CHECK((((status = 'running'::text) AND (finished_at IS NULL)) OR ((status <> 'running'::text) AND (finished_at IS NOT NULL))));

ALTER TABLE "public"."discovery_runs" ADD CONSTRAINT "discovery_runs_status_valid" CHECK((status = ANY (ARRAY['running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text])));

ALTER TABLE "public"."discovery_runs" ADD CONSTRAINT "discovery_runs_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."discovery_runs" VALIDATE CONSTRAINT "discovery_runs_company_id_fkey";

CREATE UNIQUE INDEX discovery_runs_pkey ON public.discovery_runs USING btree (id);

ALTER TABLE "public"."discovery_runs" ADD CONSTRAINT "discovery_runs_pkey" PRIMARY KEY USING INDEX "discovery_runs_pkey";

CREATE INDEX discovery_runs_company_started_idx ON public.discovery_runs USING btree (company_id, started_at DESC);

CREATE TABLE "public"."event_log" (
	"id" bigint DEFAULT nextval('event_log_id_seq'::regclass) NOT NULL,
	"event_type" text COLLATE "pg_catalog"."default" NOT NULL,
	"company_id" uuid,
	"source_id" uuid,
	"job_id" uuid,
	"payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."event_log" ADD CONSTRAINT "event_log_type_not_blank" CHECK((btrim(event_type) <> ''::text));

ALTER TABLE "public"."event_log" ADD CONSTRAINT "event_log_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."event_log" VALIDATE CONSTRAINT "event_log_company_id_fkey";

CREATE UNIQUE INDEX event_log_pkey ON public.event_log USING btree (id);

ALTER TABLE "public"."event_log" ADD CONSTRAINT "event_log_pkey" PRIMARY KEY USING INDEX "event_log_pkey";

CREATE INDEX event_log_company_idx ON public.event_log USING btree (company_id, created_at DESC) WHERE (company_id IS NOT NULL);

CREATE INDEX event_log_created_idx ON public.event_log USING btree (created_at DESC);

CREATE INDEX event_log_source_idx ON public.event_log USING btree (source_id, created_at DESC) WHERE (source_id IS NOT NULL);

ALTER SEQUENCE "public"."event_log_id_seq" OWNED BY "public"."event_log"."id";

CREATE TABLE "public"."export_runs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"export_target_id" uuid NOT NULL,
	"job_id" uuid,
	"started_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"finished_at" timestamp with time zone,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'running'::text NOT NULL,
	"item_count" integer DEFAULT 0 NOT NULL,
	"commit_sha" text COLLATE "pg_catalog"."default",
	"pushed" boolean DEFAULT false NOT NULL,
	"error" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL
);

ALTER TABLE "public"."export_runs" ADD CONSTRAINT "export_runs_finished_state" CHECK((((status = 'running'::text) AND (finished_at IS NULL)) OR ((status <> 'running'::text) AND (finished_at IS NOT NULL))));

ALTER TABLE "public"."export_runs" ADD CONSTRAINT "export_runs_item_count_nonnegative" CHECK((item_count >= 0));

ALTER TABLE "public"."export_runs" ADD CONSTRAINT "export_runs_status_valid" CHECK((status = ANY (ARRAY['running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text])));

CREATE UNIQUE INDEX export_runs_pkey ON public.export_runs USING btree (id);

ALTER TABLE "public"."export_runs" ADD CONSTRAINT "export_runs_pkey" PRIMARY KEY USING INDEX "export_runs_pkey";

CREATE INDEX export_runs_target_started_idx ON public.export_runs USING btree (export_target_id, started_at DESC);

CREATE TABLE "public"."export_targets" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"target_id" text COLLATE "pg_catalog"."default" NOT NULL,
	"repo_url" text COLLATE "pg_catalog"."default" NOT NULL,
	"local_path" text COLLATE "pg_catalog"."default" NOT NULL,
	"branch" text COLLATE "pg_catalog"."default" DEFAULT 'main'::text NOT NULL,
	"format" text COLLATE "pg_catalog"."default" NOT NULL,
	"layout" text COLLATE "pg_catalog"."default" NOT NULL,
	"cadence_seconds" integer DEFAULT 3600 NOT NULL,
	"enabled" boolean DEFAULT true NOT NULL,
	"push_enabled" boolean DEFAULT false NOT NULL,
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"last_scheduled_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_branch_not_blank" CHECK((btrim(branch) <> ''::text));

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_cadence_positive" CHECK((cadence_seconds > 0));

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_format_valid" CHECK((format = ANY (ARRAY['markdown_json'::text, 'jsonl'::text])));

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_layout_valid" CHECK((layout = 'by_company_date'::text));

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_local_path_not_blank" CHECK((btrim(local_path) <> ''::text));

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_repo_url_not_blank" CHECK((btrim(repo_url) <> ''::text));

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_target_id_not_blank" CHECK((btrim(target_id) <> ''::text));

CREATE UNIQUE INDEX export_targets_pkey ON public.export_targets USING btree (id);

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_pkey" PRIMARY KEY USING INDEX "export_targets_pkey";

CREATE UNIQUE INDEX export_targets_target_id_key ON public.export_targets USING btree (target_id);

ALTER TABLE "public"."export_targets" ADD CONSTRAINT "export_targets_target_id_key" UNIQUE USING INDEX "export_targets_target_id_key";

CREATE INDEX export_targets_enabled_idx ON public.export_targets USING btree (last_scheduled_at) WHERE enabled;

ALTER TABLE "public"."export_runs" ADD CONSTRAINT "export_runs_export_target_id_fkey" FOREIGN KEY (export_target_id) REFERENCES export_targets(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."export_runs" VALIDATE CONSTRAINT "export_runs_export_target_id_fkey";

CREATE TRIGGER export_targets_set_updated_at BEFORE UPDATE ON public.export_targets FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."exported_items" (
	"target_id" uuid NOT NULL,
	"feed_item_id" uuid NOT NULL,
	"exported_path" text COLLATE "pg_catalog"."default" NOT NULL,
	"exported_content_hash" text COLLATE "pg_catalog"."default" NOT NULL,
	"exported_commit" text COLLATE "pg_catalog"."default",
	"exported_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."exported_items" ADD CONSTRAINT "exported_items_hash_not_blank" CHECK((btrim(exported_content_hash) <> ''::text));

ALTER TABLE "public"."exported_items" ADD CONSTRAINT "exported_items_path_not_blank" CHECK((btrim(exported_path) <> ''::text));

ALTER TABLE "public"."exported_items" ADD CONSTRAINT "exported_items_target_id_fkey" FOREIGN KEY (target_id) REFERENCES export_targets(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."exported_items" VALIDATE CONSTRAINT "exported_items_target_id_fkey";

CREATE UNIQUE INDEX exported_items_pkey ON public.exported_items USING btree (target_id, feed_item_id);

ALTER TABLE "public"."exported_items" ADD CONSTRAINT "exported_items_pkey" PRIMARY KEY USING INDEX "exported_items_pkey";

CREATE TABLE "public"."feed_items" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"company_id" uuid NOT NULL,
	"source_id" uuid NOT NULL,
	"raw_crawl_item_id" uuid,
	"external_id" text COLLATE "pg_catalog"."default" NOT NULL,
	"url" text COLLATE "pg_catalog"."default" NOT NULL,
	"canonical_url" text COLLATE "pg_catalog"."default" NOT NULL,
	"title" text COLLATE "pg_catalog"."default" NOT NULL,
	"summary" text COLLATE "pg_catalog"."default" DEFAULT ''::text NOT NULL,
	"body_text" text COLLATE "pg_catalog"."default" DEFAULT ''::text NOT NULL,
	"body_html" text COLLATE "pg_catalog"."default" DEFAULT ''::text NOT NULL,
	"body_markdown" text COLLATE "pg_catalog"."default" DEFAULT ''::text NOT NULL,
	"published_at" timestamp with time zone,
	"fetched_at" timestamp with time zone NOT NULL,
	"content_hash" text COLLATE "pg_catalog"."default" NOT NULL,
	"source_kind" text COLLATE "pg_catalog"."default" NOT NULL,
	"raw" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"normalized" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"content_processing" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"is_private" boolean DEFAULT false NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_canonical_url_not_blank" CHECK((btrim(canonical_url) <> ''::text));

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_content_hash_not_blank" CHECK((btrim(content_hash) <> ''::text));

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_external_id_not_blank" CHECK((btrim(external_id) <> ''::text));

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_source_kind_valid" CHECK((source_kind = ANY (ARRAY['rss'::text, 'atom'::text, 'html'::text, 'browser'::text])));

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_title_not_blank" CHECK((btrim(title) <> ''::text));

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_url_not_blank" CHECK((btrim(url) <> ''::text));

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."feed_items" VALIDATE CONSTRAINT "feed_items_company_id_fkey";

CREATE UNIQUE INDEX feed_items_pkey ON public.feed_items USING btree (id);

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_pkey" PRIMARY KEY USING INDEX "feed_items_pkey";

CREATE UNIQUE INDEX feed_items_source_id_canonical_url_key ON public.feed_items USING btree (source_id, canonical_url);

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_source_id_canonical_url_key" UNIQUE USING INDEX "feed_items_source_id_canonical_url_key";

CREATE UNIQUE INDEX feed_items_source_id_external_id_key ON public.feed_items USING btree (source_id, external_id);

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_source_id_external_id_key" UNIQUE USING INDEX "feed_items_source_id_external_id_key";

CREATE INDEX feed_items_company_canonical_idx ON public.feed_items USING btree (company_id, canonical_url);

CREATE INDEX feed_items_company_public_url_identity_idx ON public.feed_items USING btree (company_id, public_url_identity_key(canonical_url)) WHERE (NOT is_private);

CREATE INDEX feed_items_company_published_idx ON public.feed_items USING btree (company_id, published_at DESC NULLS LAST, fetched_at DESC);

CREATE INDEX feed_items_content_hash_idx ON public.feed_items USING btree (content_hash);

CREATE INDEX feed_items_public_canonical_url_identity_idx ON public.feed_items USING btree (public_url_identity_key(canonical_url)) WHERE (NOT is_private);

CREATE INDEX feed_items_public_external_id_identity_idx ON public.feed_items USING btree (public_url_identity_key(external_id)) WHERE (NOT is_private);

CREATE INDEX feed_items_public_url_identity_idx ON public.feed_items USING btree (public_url_identity_key(url)) WHERE (NOT is_private);

CREATE INDEX feed_items_source_fetched_idx ON public.feed_items USING btree (source_id, fetched_at DESC);

CREATE UNIQUE INDEX feed_items_source_public_url_identity_unique_idx ON public.feed_items USING btree (source_id, public_url_identity_key(url)) WHERE (NOT is_private);

ALTER TABLE "public"."exported_items" ADD CONSTRAINT "exported_items_feed_item_id_fkey" FOREIGN KEY (feed_item_id) REFERENCES feed_items(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."exported_items" VALIDATE CONSTRAINT "exported_items_feed_item_id_fkey";

CREATE TRIGGER feed_items_set_updated_at BEFORE UPDATE ON public.feed_items FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."jobs" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"job_type" text COLLATE "pg_catalog"."default" NOT NULL,
	"job_key" text COLLATE "pg_catalog"."default" NOT NULL,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'pending'::text NOT NULL,
	"priority" smallint DEFAULT 0 NOT NULL,
	"run_after" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"locked_by" text COLLATE "pg_catalog"."default",
	"locked_at" timestamp with time zone,
	"heartbeat_at" timestamp with time zone,
	"lease_until" timestamp with time zone,
	"lease_token" uuid,
	"attempt_count" integer DEFAULT 0 NOT NULL,
	"max_attempts" integer DEFAULT 5 NOT NULL,
	"company_id" uuid,
	"source_id" uuid,
	"export_target_id" uuid,
	"payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"last_error" text COLLATE "pg_catalog"."default",
	"completed_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"candidate_id" uuid
);

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_attempt_count_bounded" CHECK((attempt_count <= max_attempts));

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_attempt_count_nonnegative" CHECK((attempt_count >= 0));

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_key_not_blank" CHECK((btrim(job_key) <> ''::text));

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_max_attempts_positive" CHECK((max_attempts > 0));

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_running_has_lease" CHECK(((status <> 'running'::text) OR ((locked_by IS NOT NULL) AND (locked_at IS NOT NULL) AND (heartbeat_at IS NOT NULL) AND (lease_until IS NOT NULL) AND (lease_token IS NOT NULL))));

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_status_valid" CHECK((status = ANY (ARRAY['pending'::text, 'running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text])));

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_type_valid" CHECK((job_type = ANY (ARRAY['discover_company'::text, 'validate_candidate'::text, 'crawl_source'::text, 'crawl_content'::text, 'extract_company_news'::text, 'export_target'::text, 'normalize_backfill'::text])));

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."jobs" VALIDATE CONSTRAINT "jobs_company_id_fkey";

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_export_target_id_fkey" FOREIGN KEY (export_target_id) REFERENCES export_targets(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."jobs" VALIDATE CONSTRAINT "jobs_export_target_id_fkey";

CREATE UNIQUE INDEX jobs_pkey ON public.jobs USING btree (id);

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_pkey" PRIMARY KEY USING INDEX "jobs_pkey";

CREATE INDEX jobs_candidate_idx ON public.jobs USING btree (candidate_id, created_at DESC) WHERE (candidate_id IS NOT NULL);

CREATE INDEX jobs_due_idx ON public.jobs USING btree (priority DESC, run_after, created_at) WHERE (status = 'pending'::text);

CREATE INDEX jobs_expired_lease_idx ON public.jobs USING btree (lease_until) WHERE (status = 'running'::text);

CREATE UNIQUE INDEX jobs_one_active_key_idx ON public.jobs USING btree (job_type, job_key) WHERE (status = ANY (ARRAY['pending'::text, 'running'::text]));

CREATE INDEX jobs_running_company_news_idx ON public.jobs USING btree (job_type, status) WHERE ((status = 'running'::text) AND (job_type = 'extract_company_news'::text));

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."candidate_validation_runs" VALIDATE CONSTRAINT "candidate_validation_runs_job_id_fkey";

ALTER TABLE "public"."company_news_extraction_runs" ADD CONSTRAINT "company_news_extraction_runs_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."company_news_extraction_runs" VALIDATE CONSTRAINT "company_news_extraction_runs_job_id_fkey";

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."company_news_recipe_runs" VALIDATE CONSTRAINT "company_news_recipe_runs_job_id_fkey";

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."crawl_runs" VALIDATE CONSTRAINT "crawl_runs_job_id_fkey";

ALTER TABLE "public"."discovery_runs" ADD CONSTRAINT "discovery_runs_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."discovery_runs" VALIDATE CONSTRAINT "discovery_runs_job_id_fkey";

ALTER TABLE "public"."event_log" ADD CONSTRAINT "event_log_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."event_log" VALIDATE CONSTRAINT "event_log_job_id_fkey";

ALTER TABLE "public"."export_runs" ADD CONSTRAINT "export_runs_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."export_runs" VALIDATE CONSTRAINT "export_runs_job_id_fkey";

CREATE TRIGGER jobs_set_updated_at BEFORE UPDATE ON public.jobs FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."content_crawl_state" (
	"feed_item_id" uuid NOT NULL,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'pending'::text NOT NULL,
	"last_attempt_at" timestamp with time zone,
	"last_success_at" timestamp with time zone,
	"next_attempt_at" timestamp with time zone,
	"attempt_count" integer DEFAULT 0 NOT NULL,
	"consecutive_failures" integer DEFAULT 0 NOT NULL,
	"last_error_reason" text COLLATE "pg_catalog"."default",
	"last_error" text COLLATE "pg_catalog"."default",
	"last_http_status" integer,
	"content_chars" integer,
	"extraction_version" text COLLATE "pg_catalog"."default",
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."content_crawl_state" ADD CONSTRAINT "content_crawl_state_attempt_count_nonnegative" CHECK((attempt_count >= 0));

ALTER TABLE "public"."content_crawl_state" ADD CONSTRAINT "content_crawl_state_consecutive_failures_nonnegative" CHECK((consecutive_failures >= 0));

ALTER TABLE "public"."content_crawl_state" ADD CONSTRAINT "content_crawl_state_content_chars_nonnegative" CHECK(((content_chars IS NULL) OR (content_chars >= 0)));

ALTER TABLE "public"."content_crawl_state" ADD CONSTRAINT "content_crawl_state_status_valid" CHECK((status = ANY (ARRAY['pending'::text, 'running'::text, 'succeeded'::text, 'failed'::text, 'permanent_failure'::text])));

ALTER TABLE "public"."content_crawl_state" ADD CONSTRAINT "content_crawl_state_feed_item_id_fkey" FOREIGN KEY (feed_item_id) REFERENCES feed_items(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."content_crawl_state" VALIDATE CONSTRAINT "content_crawl_state_feed_item_id_fkey";

CREATE UNIQUE INDEX content_crawl_state_pkey ON public.content_crawl_state USING btree (feed_item_id);

ALTER TABLE "public"."content_crawl_state" ADD CONSTRAINT "content_crawl_state_pkey" PRIMARY KEY USING INDEX "content_crawl_state_pkey";

CREATE INDEX content_crawl_state_due_idx ON public.content_crawl_state USING btree (next_attempt_at, feed_item_id) WHERE (status = ANY (ARRAY['pending'::text, 'failed'::text]));

CREATE TRIGGER content_crawl_state_set_updated_at BEFORE UPDATE ON public.content_crawl_state FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."content_crawl_attempts" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"feed_item_id" uuid NOT NULL,
	"job_id" uuid,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'running'::text NOT NULL,
	"requested_url" text COLLATE "pg_catalog"."default" NOT NULL,
	"final_url" text COLLATE "pg_catalog"."default",
	"canonical_url" text COLLATE "pg_catalog"."default",
	"started_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"finished_at" timestamp with time zone,
	"retryable" boolean,
	"error_reason" text COLLATE "pg_catalog"."default",
	"error" text COLLATE "pg_catalog"."default",
	"http_status" integer,
	"content_chars" integer,
	"content_hash" text COLLATE "pg_catalog"."default",
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL
);

ALTER TABLE "public"."content_crawl_attempts" ADD CONSTRAINT "content_crawl_attempts_content_chars_nonnegative" CHECK(((content_chars IS NULL) OR (content_chars >= 0)));

ALTER TABLE "public"."content_crawl_attempts" ADD CONSTRAINT "content_crawl_attempts_requested_url_not_blank" CHECK((btrim(requested_url) <> ''::text));

ALTER TABLE "public"."content_crawl_attempts" ADD CONSTRAINT "content_crawl_attempts_status_valid" CHECK((status = ANY (ARRAY['running'::text, 'succeeded'::text, 'failed'::text, 'cancelled'::text])));

ALTER TABLE "public"."content_crawl_attempts" ADD CONSTRAINT "content_crawl_attempts_feed_item_id_fkey" FOREIGN KEY (feed_item_id) REFERENCES feed_items(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."content_crawl_attempts" VALIDATE CONSTRAINT "content_crawl_attempts_feed_item_id_fkey";

ALTER TABLE "public"."content_crawl_attempts" ADD CONSTRAINT "content_crawl_attempts_job_id_fkey" FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."content_crawl_attempts" VALIDATE CONSTRAINT "content_crawl_attempts_job_id_fkey";

CREATE UNIQUE INDEX content_crawl_attempts_pkey ON public.content_crawl_attempts USING btree (id);

ALTER TABLE "public"."content_crawl_attempts" ADD CONSTRAINT "content_crawl_attempts_pkey" PRIMARY KEY USING INDEX "content_crawl_attempts_pkey";

CREATE INDEX content_crawl_attempts_feed_item_idx ON public.content_crawl_attempts USING btree (feed_item_id, started_at DESC, id DESC);

CREATE INDEX content_crawl_attempts_job_idx ON public.content_crawl_attempts USING btree (job_id, started_at, id) WHERE (job_id IS NOT NULL);

CREATE TABLE "public"."raw_crawl_items" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"crawl_run_id" uuid NOT NULL,
	"source_id" uuid NOT NULL,
	"source_item_key" text COLLATE "pg_catalog"."default" NOT NULL,
	"external_id" text COLLATE "pg_catalog"."default",
	"fetched_url" text COLLATE "pg_catalog"."default" NOT NULL,
	"canonical_url" text COLLATE "pg_catalog"."default",
	"payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"fetched_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"processing_status" text COLLATE "pg_catalog"."default" DEFAULT 'pending'::text NOT NULL,
	"normalization_attempt_count" integer DEFAULT 0 NOT NULL,
	"normalization_error" text COLLATE "pg_catalog"."default",
	"normalized_feed_item_id" uuid,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_attempts_nonnegative" CHECK((normalization_attempt_count >= 0));

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_key_not_blank" CHECK((btrim(source_item_key) <> ''::text));

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_status_valid" CHECK((processing_status = ANY (ARRAY['pending'::text, 'normalized'::text, 'failed'::text, 'skipped'::text])));

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_url_not_blank" CHECK((btrim(fetched_url) <> ''::text));

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_crawl_run_id_fkey" FOREIGN KEY (crawl_run_id) REFERENCES crawl_runs(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."raw_crawl_items" VALIDATE CONSTRAINT "raw_crawl_items_crawl_run_id_fkey";

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_normalized_feed_item_fk" FOREIGN KEY (normalized_feed_item_id) REFERENCES feed_items(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."raw_crawl_items" VALIDATE CONSTRAINT "raw_crawl_items_normalized_feed_item_fk";

CREATE UNIQUE INDEX raw_crawl_items_crawl_run_id_source_item_key_key ON public.raw_crawl_items USING btree (crawl_run_id, source_item_key);

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_crawl_run_id_source_item_key_key" UNIQUE USING INDEX "raw_crawl_items_crawl_run_id_source_item_key_key";

CREATE UNIQUE INDEX raw_crawl_items_pkey ON public.raw_crawl_items USING btree (id);

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_pkey" PRIMARY KEY USING INDEX "raw_crawl_items_pkey";

CREATE INDEX raw_crawl_items_processing_idx ON public.raw_crawl_items USING btree (processing_status, created_at) WHERE (processing_status = ANY (ARRAY['pending'::text, 'failed'::text]));

CREATE INDEX raw_crawl_items_source_idx ON public.raw_crawl_items USING btree (source_id, fetched_at DESC);

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_raw_crawl_item_id_fkey" FOREIGN KEY (raw_crawl_item_id) REFERENCES raw_crawl_items(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."feed_items" VALIDATE CONSTRAINT "feed_items_raw_crawl_item_id_fkey";

CREATE TRIGGER raw_crawl_items_set_updated_at BEFORE UPDATE ON public.raw_crawl_items FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."source_candidates" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"company_id" uuid NOT NULL,
	"discovery_run_id" uuid,
	"candidate_url" text COLLATE "pg_catalog"."default" NOT NULL,
	"candidate_kind" text COLLATE "pg_catalog"."default" NOT NULL,
	"confidence" double precision NOT NULL,
	"evidence" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'new'::text NOT NULL,
	"accepted_source_id" uuid,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_acceptance_consistent" CHECK((((status = 'accepted'::text) AND (accepted_source_id IS NOT NULL)) OR ((status <> 'accepted'::text) AND (accepted_source_id IS NULL))));

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_confidence_range" CHECK(((confidence >= (0.0)::double precision) AND (confidence <= (1.0)::double precision)));

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_kind_valid" CHECK((candidate_kind = ANY (ARRAY['rss'::text, 'atom'::text, 'html'::text, 'browser'::text])));

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_status_valid" CHECK((status = ANY (ARRAY['new'::text, 'accepted'::text, 'rejected'::text])));

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_url_not_blank" CHECK((btrim(candidate_url) <> ''::text));

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."source_candidates" VALIDATE CONSTRAINT "source_candidates_company_id_fkey";

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_discovery_run_id_fkey" FOREIGN KEY (discovery_run_id) REFERENCES discovery_runs(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."source_candidates" VALIDATE CONSTRAINT "source_candidates_discovery_run_id_fkey";

CREATE UNIQUE INDEX source_candidates_company_id_candidate_url_candidate_kind_key ON public.source_candidates USING btree (company_id, candidate_url, candidate_kind);

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_company_id_candidate_url_candidate_kind_key" UNIQUE USING INDEX "source_candidates_company_id_candidate_url_candidate_kind_key";

CREATE UNIQUE INDEX source_candidates_pkey ON public.source_candidates USING btree (id);

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_pkey" PRIMARY KEY USING INDEX "source_candidates_pkey";

CREATE INDEX source_candidates_review_idx ON public.source_candidates USING btree (company_id, confidence DESC, created_at) WHERE (status = 'new'::text);

ALTER TABLE "public"."candidate_decisions" ADD CONSTRAINT "candidate_decisions_candidate_id_fkey" FOREIGN KEY (candidate_id) REFERENCES source_candidates(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."candidate_decisions" VALIDATE CONSTRAINT "candidate_decisions_candidate_id_fkey";

ALTER TABLE "public"."candidate_validation_runs" ADD CONSTRAINT "candidate_validation_runs_candidate_id_fkey" FOREIGN KEY (candidate_id) REFERENCES source_candidates(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."candidate_validation_runs" VALIDATE CONSTRAINT "candidate_validation_runs_candidate_id_fkey";

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_candidate_id_fkey" FOREIGN KEY (candidate_id) REFERENCES source_candidates(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."jobs" VALIDATE CONSTRAINT "jobs_candidate_id_fkey";

CREATE TRIGGER source_candidates_set_updated_at BEFORE UPDATE ON public.source_candidates FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."source_state" (
	"source_id" uuid NOT NULL,
	"last_attempt_at" timestamp with time zone,
	"last_success_at" timestamp with time zone,
	"last_error" text COLLATE "pg_catalog"."default",
	"consecutive_failures" integer DEFAULT 0 NOT NULL,
	"backoff_until" timestamp with time zone,
	"cursor" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"consecutive_zero_runs" integer DEFAULT 0 NOT NULL,
	"total_successful_runs" bigint DEFAULT 0 NOT NULL,
	"total_items" bigint DEFAULT 0 NOT NULL,
	"last_nonzero_at" timestamp with time zone,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."source_state" ADD CONSTRAINT "source_state_failures_nonnegative" CHECK((consecutive_failures >= 0));

ALTER TABLE "public"."source_state" ADD CONSTRAINT "source_state_successful_runs_nonnegative" CHECK((total_successful_runs >= 0));

ALTER TABLE "public"."source_state" ADD CONSTRAINT "source_state_total_items_nonnegative" CHECK((total_items >= 0));

ALTER TABLE "public"."source_state" ADD CONSTRAINT "source_state_zero_runs_nonnegative" CHECK((consecutive_zero_runs >= 0));

CREATE UNIQUE INDEX source_state_pkey ON public.source_state USING btree (source_id);

ALTER TABLE "public"."source_state" ADD CONSTRAINT "source_state_pkey" PRIMARY KEY USING INDEX "source_state_pkey";

CREATE TRIGGER source_state_set_updated_at BEFORE UPDATE ON public.source_state FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE "public"."sources" (
	"id" uuid DEFAULT gen_random_uuid() NOT NULL,
	"source_id" text COLLATE "pg_catalog"."default" NOT NULL,
	"company_id" uuid NOT NULL,
	"kind" text COLLATE "pg_catalog"."default" NOT NULL,
	"url" text COLLATE "pg_catalog"."default" NOT NULL,
	"status" text COLLATE "pg_catalog"."default" DEFAULT 'approved'::text NOT NULL,
	"freshness_slo_seconds" integer DEFAULT 3600 NOT NULL,
	"browser_required" boolean DEFAULT false NOT NULL,
	"public_export_allowed" boolean DEFAULT false NOT NULL,
	"discovery_confidence" double precision,
	"metadata" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
	"updated_at" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_discovery_confidence_range" CHECK(((discovery_confidence IS NULL) OR ((discovery_confidence >= (0.0)::double precision) AND (discovery_confidence <= (1.0)::double precision))));

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_freshness_slo_positive" CHECK((freshness_slo_seconds > 0));

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_kind_valid" CHECK((kind = ANY (ARRAY['rss'::text, 'atom'::text, 'html'::text, 'browser'::text])));

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_source_id_not_blank" CHECK((btrim(source_id) <> ''::text));

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_status_valid" CHECK((status = ANY (ARRAY['approved'::text, 'disabled'::text])));

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_url_not_blank" CHECK((btrim(url) <> ''::text));

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_company_id_fkey" FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."sources" VALIDATE CONSTRAINT "sources_company_id_fkey";

CREATE UNIQUE INDEX sources_company_id_url_key ON public.sources USING btree (company_id, url);

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_company_id_url_key" UNIQUE USING INDEX "sources_company_id_url_key";

CREATE UNIQUE INDEX sources_pkey ON public.sources USING btree (id);

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_pkey" PRIMARY KEY USING INDEX "sources_pkey";

CREATE UNIQUE INDEX sources_source_id_key ON public.sources USING btree (source_id);

ALTER TABLE "public"."sources" ADD CONSTRAINT "sources_source_id_key" UNIQUE USING INDEX "sources_source_id_key";

CREATE INDEX sources_approved_idx ON public.sources USING btree (company_id, freshness_slo_seconds) WHERE (status = 'approved'::text);

CREATE INDEX sources_company_idx ON public.sources USING btree (company_id);

ALTER TABLE "public"."candidate_decisions" ADD CONSTRAINT "candidate_decisions_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."candidate_decisions" VALIDATE CONSTRAINT "candidate_decisions_source_id_fkey";

ALTER TABLE "public"."company_news_recipe_runs" ADD CONSTRAINT "company_news_recipe_runs_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_news_recipe_runs" VALIDATE CONSTRAINT "company_news_recipe_runs_source_id_fkey";

ALTER TABLE "public"."company_news_recipes" ADD CONSTRAINT "company_news_recipes_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."company_news_recipes" VALIDATE CONSTRAINT "company_news_recipes_source_id_fkey";

ALTER TABLE "public"."crawl_runs" ADD CONSTRAINT "crawl_runs_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."crawl_runs" VALIDATE CONSTRAINT "crawl_runs_source_id_fkey";

ALTER TABLE "public"."event_log" ADD CONSTRAINT "event_log_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."event_log" VALIDATE CONSTRAINT "event_log_source_id_fkey";

ALTER TABLE "public"."feed_items" ADD CONSTRAINT "feed_items_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."feed_items" VALIDATE CONSTRAINT "feed_items_source_id_fkey";

ALTER TABLE "public"."jobs" ADD CONSTRAINT "jobs_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."jobs" VALIDATE CONSTRAINT "jobs_source_id_fkey";

ALTER TABLE "public"."raw_crawl_items" ADD CONSTRAINT "raw_crawl_items_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."raw_crawl_items" VALIDATE CONSTRAINT "raw_crawl_items_source_id_fkey";

ALTER TABLE "public"."source_candidates" ADD CONSTRAINT "source_candidates_accepted_source_id_fkey" FOREIGN KEY (accepted_source_id) REFERENCES sources(id) ON DELETE SET NULL NOT VALID;

ALTER TABLE "public"."source_candidates" VALIDATE CONSTRAINT "source_candidates_accepted_source_id_fkey";

ALTER TABLE "public"."source_state" ADD CONSTRAINT "source_state_source_id_fkey" FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE NOT VALID;

ALTER TABLE "public"."source_state" VALIDATE CONSTRAINT "source_state_source_id_fkey";

CREATE TRIGGER sources_set_updated_at BEFORE UPDATE ON public.sources FOR EACH ROW EXECUTE FUNCTION set_updated_at();
