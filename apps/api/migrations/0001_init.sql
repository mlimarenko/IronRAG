-- IronRAG clean database baseline v1. Fresh-only schema contract.

--
-- PostgreSQL database dump
--


-- Dumped from database version 18.6 (Debian 18.6-1.pgdg12+2)
-- Dumped by pg_dump version 18.6 (Debian 18.6-1.pgdg12+2)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET transaction_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: btree_gin; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS btree_gin WITH SCHEMA public;


--
-- Name: EXTENSION btree_gin; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION btree_gin IS 'support for indexing common datatypes in GIN';


--
-- Name: dblink; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS dblink WITH SCHEMA public;


--
-- Name: EXTENSION dblink; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION dblink IS 'connect to other PostgreSQL databases from within a database';


--
-- Name: pg_trgm; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;


--
-- Name: EXTENSION pg_trgm; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION pg_trgm IS 'text similarity measurement and index searching based on trigrams';


--
-- Name: pgcrypto; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;


--
-- Name: EXTENSION pgcrypto; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION pgcrypto IS 'cryptographic functions';


--
-- Name: unaccent; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS unaccent WITH SCHEMA public;


--
-- Name: EXTENSION unaccent; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION unaccent IS 'text search dictionary that removes accents';


--
-- Name: vector; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public;


--
-- Name: EXTENSION vector; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION vector IS 'vector data type and ivfflat and hnsw access methods';


--
-- Name: ai_account_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_account_state AS ENUM (
    'active',
    'invalid',
    'revoked'
);


--
-- Name: ai_binding_purpose; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_binding_purpose AS ENUM (
    'extract_text',
    'extract_graph',
    'graph_entity_type',
    'embed_chunk',
    'query_compile',
    'query_rerank',
    'query_answer',
    'agent'
);


--
-- Name: ai_binding_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_binding_state AS ENUM (
    'active',
    'invalid',
    'disabled'
);


--
-- Name: ai_model_lifecycle_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_model_lifecycle_state AS ENUM (
    'active',
    'preview',
    'deprecated',
    'disabled'
);


--
-- Name: ai_provider_api_style; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_provider_api_style AS ENUM (
    'openai_compatible',
    'anthropic_messages',
    'gemini_generate_content',
    'typesafe_system_one'
);


--
-- Name: ai_provider_lifecycle_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_provider_lifecycle_state AS ENUM (
    'active',
    'preview',
    'deprecated',
    'disabled'
);


--
-- Name: ai_scope_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_scope_kind AS ENUM (
    'instance',
    'workspace',
    'library'
);


--
-- Name: ai_validation_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ai_validation_state AS ENUM (
    'pending',
    'succeeded',
    'failed'
);


--
-- Name: audit_result_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.audit_result_kind AS ENUM (
    'succeeded',
    'rejected',
    'failed'
);


--
-- Name: billing_call_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.billing_call_state AS ENUM (
    'started',
    'completed',
    'failed',
    'canceled'
);


--
-- Name: billing_owning_execution_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.billing_owning_execution_kind AS ENUM (
    'ingest_attempt',
    'query_execution',
    'graph_extraction_attempt',
    'binding_validation'
);


--
-- Name: billing_unit; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.billing_unit AS ENUM (
    'per_1m_input_tokens',
    'per_1m_output_tokens',
    'per_1m_cached_input_tokens',
    'per_1m_cache_write_input_tokens'
);


--
-- Name: catalog_connector_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.catalog_connector_kind AS ENUM (
    'generic',
    'filesystem',
    'github',
    's3',
    'web'
);


--
-- Name: catalog_connector_sync_mode; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.catalog_connector_sync_mode AS ENUM (
    'manual',
    'scheduled',
    'webhook'
);


--
-- Name: catalog_library_lifecycle_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.catalog_library_lifecycle_state AS ENUM (
    'active',
    'archived'
);


--
-- Name: catalog_workspace_lifecycle_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.catalog_workspace_lifecycle_state AS ENUM (
    'active',
    'archived'
);


--
-- Name: content_document_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.content_document_state AS ENUM (
    'active',
    'deleted'
);


--
-- Name: content_mutation_item_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.content_mutation_item_state AS ENUM (
    'pending',
    'applied',
    'failed',
    'conflicted',
    'skipped'
);


--
-- Name: content_mutation_operation_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.content_mutation_operation_kind AS ENUM (
    'upload',
    'append',
    'replace',
    'edit',
    'reprocess',
    'delete',
    'connector_sync',
    'web_capture',
    'visibility'
);


--
-- Name: content_mutation_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.content_mutation_state AS ENUM (
    'accepted',
    'running',
    'applied',
    'failed',
    'conflicted',
    'canceled'
);


--
-- Name: content_source_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.content_source_kind AS ENUM (
    'upload',
    'append',
    'replace',
    'edit',
    'connector_sync',
    'import',
    'web_page'
);


--
-- Name: extract_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.extract_state AS ENUM (
    'missing',
    'processing',
    'ready',
    'failed'
);


--
-- Name: iam_api_token_status; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.iam_api_token_status AS ENUM (
    'active',
    'disabled',
    'revoked',
    'expired'
);


--
-- Name: iam_grant_resource_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.iam_grant_resource_kind AS ENUM (
    'system',
    'workspace',
    'library',
    'document',
    'query_session',
    'async_operation',
    'connector',
    'provider_credential',
    'library_binding'
);


--
-- Name: iam_membership_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.iam_membership_state AS ENUM (
    'active',
    'invited',
    'suspended',
    'ended'
);


--
-- Name: iam_permission_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.iam_permission_kind AS ENUM (
    'workspace_admin',
    'workspace_read',
    'library_read',
    'library_write',
    'document_read',
    'document_write',
    'connector_admin',
    'credential_admin',
    'binding_admin',
    'query_run',
    'ops_read',
    'audit_read',
    'iam_admin'
);


--
-- Name: iam_principal_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.iam_principal_kind AS ENUM (
    'user',
    'api_token',
    'worker',
    'bootstrap'
);


--
-- Name: iam_principal_status; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.iam_principal_status AS ENUM (
    'active',
    'disabled',
    'revoked'
);


--
-- Name: iam_system_role; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.iam_system_role AS ENUM (
    'viewer',
    'operator',
    'admin'
);


--
-- Name: ingest_attempt_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ingest_attempt_state AS ENUM (
    'leased',
    'running',
    'succeeded',
    'failed',
    'abandoned',
    'canceled'
);


--
-- Name: ingest_job_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ingest_job_kind AS ENUM (
    'content_mutation',
    'connector_sync',
    'reindex',
    'reembed',
    'graph_refresh',
    'web_discovery',
    'web_materialize_page',
    'webhook_delivery'
);


--
-- Name: ingest_queue_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ingest_queue_state AS ENUM (
    'queued',
    'leased',
    'completed',
    'failed',
    'canceled',
    'paused'
);


--
-- Name: ingest_stage_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ingest_stage_state AS ENUM (
    'started',
    'completed',
    'failed',
    'skipped'
);


--
-- Name: maintenance_run_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.maintenance_run_state AS ENUM (
    'pending',
    'leased',
    'completed',
    'failed',
    'dead_letter'
);


--
-- Name: ops_async_operation_status; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ops_async_operation_status AS ENUM (
    'accepted',
    'processing',
    'ready',
    'failed',
    'superseded',
    'canceled'
);


--
-- Name: ops_degraded_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ops_degraded_state AS ENUM (
    'healthy',
    'degraded',
    'failed',
    'rebuilding'
);


--
-- Name: ops_warning_severity; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.ops_warning_severity AS ENUM (
    'info',
    'warn',
    'error'
);


--
-- Name: query_conversation_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.query_conversation_state AS ENUM (
    'active',
    'archived'
);


--
-- Name: query_turn_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.query_turn_kind AS ENUM (
    'user',
    'assistant',
    'system',
    'tool'
);


--
-- Name: runtime_action_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_action_kind AS ENUM (
    'deterministic_step',
    'model_request',
    'tool_request',
    'tool_result',
    'recovery_attempt',
    'persistence_write'
);


--
-- Name: runtime_action_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_action_state AS ENUM (
    'pending',
    'running',
    'completed',
    'recovered',
    'failed',
    'canceled'
);


--
-- Name: runtime_decision_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_decision_kind AS ENUM (
    'allow',
    'reject',
    'terminate'
);


--
-- Name: runtime_decision_target_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_decision_target_kind AS ENUM (
    'model_request',
    'tool_request',
    'tool_result',
    'stage_transition',
    'final_outcome'
);


--
-- Name: runtime_execution_owner_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_execution_owner_kind AS ENUM (
    'query_execution',
    'graph_extraction_attempt',
    'structured_preparation',
    'technical_fact_extraction'
);


--
-- Name: runtime_lifecycle_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_lifecycle_state AS ENUM (
    'accepted',
    'running',
    'completed',
    'recovered',
    'failed',
    'canceled'
);


--
-- Name: runtime_stage_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_stage_kind AS ENUM (
    'plan',
    'retrieve',
    'rerank',
    'assemble_context',
    'answer',
    'verify',
    'extract_graph',
    'structured_prepare',
    'technical_fact_extract',
    'recovery',
    'persist'
);


--
-- Name: runtime_stage_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_stage_state AS ENUM (
    'pending',
    'running',
    'completed',
    'recovered',
    'failed',
    'canceled'
);


--
-- Name: runtime_task_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.runtime_task_kind AS ENUM (
    'query_plan',
    'query_rerank',
    'query_answer',
    'query_verify',
    'graph_extract',
    'structured_prepare',
    'technical_fact_extract'
);


--
-- Name: surface_kind; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.surface_kind AS ENUM (
    'ui',
    'rest',
    'mcp',
    'worker',
    'bootstrap',
    'stream',
    'internal'
);


--
-- Name: web_boundary_policy; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.web_boundary_policy AS ENUM (
    'same_host',
    'allow_external',
    'same_host_and_subdomains'
);


--
-- Name: web_candidate_host_classification; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.web_candidate_host_classification AS ENUM (
    'same_host',
    'external',
    'seed_subdomain'
);


--
-- Name: web_candidate_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.web_candidate_state AS ENUM (
    'discovered',
    'eligible',
    'duplicate',
    'excluded',
    'blocked',
    'queued',
    'processing',
    'materialized',
    'processed',
    'failed',
    'canceled'
);


--
-- Name: web_ingest_mode; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.web_ingest_mode AS ENUM (
    'single_page',
    'recursive_crawl'
);


--
-- Name: web_run_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.web_run_state AS ENUM (
    'accepted',
    'discovering',
    'processing',
    'completed',
    'completed_partial',
    'failed',
    'canceled'
);


--
-- Name: webhook_delivery_state; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.webhook_delivery_state AS ENUM (
    'pending',
    'delivering',
    'delivered',
    'failed',
    'abandoned'
);


--
-- Name: bump_library_source_truth_versions(uuid[]); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.bump_library_source_truth_versions(affected_library_ids uuid[]) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    if coalesce(cardinality(affected_library_ids), 0) = 0 then
        return;
    end if;

    with locked_library as materialized (
        select library.id
        from public.catalog_library library
        where library.id = any(affected_library_ids)
        order by library.id
        for no key update
    )
    update public.catalog_library library
    set source_truth_version = greatest(
            coalesce(library.source_truth_version, 0) + 1,
            (extract(epoch from clock_timestamp()) * 1000000)::bigint
        )
    from locked_library
    where library.id = locked_library.id;
end
$$;


--
-- Name: carry_readable_projection_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.carry_readable_projection_version() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    if new.source_truth_version is distinct from old.source_truth_version
       and new.readable_projection_version is not distinct from old.readable_projection_version
       and old.readable_projection_version is not distinct from old.source_truth_version then
        new.readable_projection_version := new.source_truth_version;
    end if;
    return new;
end
$$;


--
-- Name: enforce_ai_binding_account_scope(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.enforce_ai_binding_account_scope() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    perform 1
    from public.ai_binding binding
    join public.ai_account account on account.id = binding.account_id
    where binding.id = new.id
      and (
          (binding.scope_kind = 'instance' and account.scope_kind = 'instance')
          or (
              binding.scope_kind = 'workspace'
              and (
                  account.scope_kind = 'instance'
                  or (
                      account.scope_kind = 'workspace'
                      and account.workspace_id = binding.workspace_id
                  )
              )
          )
          or (
              binding.scope_kind = 'library'
              and (
                  account.scope_kind = 'instance'
                  or (
                      account.scope_kind = 'workspace'
                      and account.workspace_id = binding.workspace_id
                  )
                  or (
                      account.scope_kind = 'library'
                      and account.library_id = binding.library_id
                  )
              )
          )
      )
    for no key update of account;

    if not found and exists (select 1 from public.ai_binding binding where binding.id = new.id) then
        raise check_violation using
            constraint = 'ai_binding_account_scope_compatible',
            message = 'ai_binding cannot use an ai_account from an unrelated scope';
    end if;
    return new;
end
$$;


--
-- Name: enforce_webhook_delivery_attempt_subscription_scope(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.enforce_webhook_delivery_attempt_subscription_scope() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    if new.job_id is not null then
        perform pg_advisory_xact_lock(hashtextextended(
            'ironrag:webhook-delivery-job:' || new.job_id::text,
            0
        ));
    end if;
    if not exists (
        select 1
        from webhook_subscription subscription
        join catalog_library library
          on library.id = new.library_id
         and library.workspace_id = new.workspace_id
        where subscription.id = new.subscription_id
          and subscription.workspace_id = new.workspace_id
          and (
              subscription.library_id is null
              or subscription.library_id = new.library_id
          )
          and (
              new.job_id is null
              or exists (
                  select 1
                  from ingest_job job
                  where job.id = new.job_id
                    and job.workspace_id = new.workspace_id
                    and job.library_id = new.library_id
                    and job.job_kind = 'webhook_delivery'::ingest_job_kind
              )
          )
          and (
              new.job_id is null
              or not exists (
                  select 1
                  from webhook_delivery_attempt other_delivery
                  where other_delivery.job_id = new.job_id
                    and other_delivery.id <> new.id
              )
          )
    ) then
        raise foreign_key_violation using
            message = 'webhook delivery attempt tenant scope does not match subscription';
    end if;
    return new;
end
$$;


--
-- Name: enforce_webhook_delivery_event_identity(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.enforce_webhook_delivery_event_identity() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    perform pg_advisory_xact_lock(hashtextextended(
        'ironrag:webhook-delivery-event:'
        || new.subscription_id::text || ':' || new.event_id,
        0
    ));
    if exists (
        select 1
        from webhook_delivery_attempt delivery
        where delivery.subscription_id = new.subscription_id
          and delivery.event_id = new.event_id
          and delivery.id <> new.id
    ) then
        raise exception 'webhook delivery event already exists'
            using errcode = '23505',
                  constraint = 'webhook_delivery_attempt_subscription_event_key';
    end if;
    return new;
end
$$;


--
-- Name: enforce_webhook_subscription_workspace_quota(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.enforce_webhook_subscription_workspace_quota() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    new_lock_key bigint;
    old_lock_key bigint;
    active_count bigint;
    total_count bigint;
    enforce_total_quota boolean := false;
begin
    new_lock_key := hashtextextended(
        'ironrag:webhook-subscription-quota:' || new.workspace_id::text,
        0
    );
    if tg_op = 'UPDATE' then
        if old.workspace_id <> new.workspace_id then
            old_lock_key := hashtextextended(
                'ironrag:webhook-subscription-quota:' || old.workspace_id::text,
                0
            );
            perform pg_advisory_xact_lock(least(old_lock_key, new_lock_key));
            perform pg_advisory_xact_lock(greatest(old_lock_key, new_lock_key));
        else
            perform pg_advisory_xact_lock(new_lock_key);
        end if;
    else
        perform pg_advisory_xact_lock(new_lock_key);
    end if;

    -- Total cardinality changes only on INSERT or a workspace move. Do not
    -- block deactivation/repair of a legacy over-quota tenant: those updates
    -- must remain available so operators can drain it safely.
    if tg_op = 'INSERT' then
        enforce_total_quota := true;
    elsif old.workspace_id <> new.workspace_id then
        enforce_total_quota := true;
    end if;
    if enforce_total_quota then
        select count(*)::bigint
        into total_count
        from webhook_subscription subscription
        where subscription.workspace_id = new.workspace_id
          and subscription.id <> new.id;
        if total_count >= 1000 then
            raise exception 'total webhook subscription quota exceeded'
                using errcode = '23514',
                      constraint = 'webhook_subscription_total_workspace_quota';
        end if;
    end if;

    if new.active then
        select count(*)::bigint
        into active_count
        from webhook_subscription subscription
        where subscription.workspace_id = new.workspace_id
          and subscription.active
          and subscription.id <> new.id;
        if active_count >= 100 then
            raise exception 'active webhook subscription quota exceeded'
                using errcode = '23514',
                      constraint = 'webhook_subscription_active_workspace_quota';
        end if;
    end if;
    return new;
end
$$;


--
-- Name: initialize_readable_projection_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.initialize_readable_projection_version() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    new.readable_projection_version := new.source_truth_version;
    return new;
end
$$;


--
-- Name: invalidate_ai_account_delete_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_account_delete_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from public.ai_binding binding
        join old_accounts changed_account on changed_account.id = binding.account_id
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
              when 'instance'::public.ai_scope_kind then true
              when 'workspace'::public.ai_scope_kind then
                  library.workspace_id = binding.workspace_id
              when 'library'::public.ai_scope_kind then
                  library.id = binding.library_id
              else false
          end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_account_update_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_account_update_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from public.ai_binding binding
        join (
            select id from old_accounts
            union
            select id from new_accounts
        ) changed_account on changed_account.id = binding.account_id
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
              when 'instance'::public.ai_scope_kind then true
              when 'workspace'::public.ai_scope_kind then
                  library.workspace_id = binding.workspace_id
              when 'library'::public.ai_scope_kind then
                  library.id = binding.library_id
              else false
          end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_binding_delete_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_binding_delete_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from old_bindings binding
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
            when 'instance'::public.ai_scope_kind then true
            when 'workspace'::public.ai_scope_kind then
                library.workspace_id = binding.workspace_id
            when 'library'::public.ai_scope_kind then
                library.id = binding.library_id
            else false
        end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_binding_insert_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_binding_insert_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from new_bindings binding
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
            when 'instance'::public.ai_scope_kind then true
            when 'workspace'::public.ai_scope_kind then
                library.workspace_id = binding.workspace_id
            when 'library'::public.ai_scope_kind then
                library.id = binding.library_id
            else false
        end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_binding_update_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_binding_update_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from (
            select scope_kind, workspace_id, library_id
            from old_bindings
            where binding_state = 'active'::public.ai_binding_state
            union
            select scope_kind, workspace_id, library_id
            from new_bindings
            where binding_state = 'active'::public.ai_binding_state
        ) binding
        where case binding.scope_kind
            when 'instance'::public.ai_scope_kind then true
            when 'workspace'::public.ai_scope_kind then
                library.workspace_id = binding.workspace_id
            when 'library'::public.ai_scope_kind then
                library.id = binding.library_id
            else false
        end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_model_delete_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_model_delete_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from public.ai_binding binding
        join old_models changed_model on changed_model.id = binding.model_catalog_id
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
              when 'instance'::public.ai_scope_kind then true
              when 'workspace'::public.ai_scope_kind then
                  library.workspace_id = binding.workspace_id
              when 'library'::public.ai_scope_kind then
                  library.id = binding.library_id
              else false
          end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_model_update_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_model_update_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from public.ai_binding binding
        join (
            select id from old_models
            union
            select id from new_models
        ) changed_model on changed_model.id = binding.model_catalog_id
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
              when 'instance'::public.ai_scope_kind then true
              when 'workspace'::public.ai_scope_kind then
                  library.workspace_id = binding.workspace_id
              when 'library'::public.ai_scope_kind then
                  library.id = binding.library_id
              else false
          end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_provider_delete_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_provider_delete_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from public.ai_binding binding
        join public.ai_account account on account.id = binding.account_id
        join public.ai_model_catalog model on model.id = binding.model_catalog_id
        join old_providers changed_provider
          on changed_provider.id = account.provider_catalog_id
          or changed_provider.id = model.provider_catalog_id
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
              when 'instance'::public.ai_scope_kind then true
              when 'workspace'::public.ai_scope_kind then
                  library.workspace_id = binding.workspace_id
              when 'library'::public.ai_scope_kind then
                  library.id = binding.library_id
              else false
          end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_ai_provider_update_source_generations(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_ai_provider_update_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from public.ai_binding binding
        join public.ai_account account on account.id = binding.account_id
        join public.ai_model_catalog model on model.id = binding.model_catalog_id
        join (
            select id from old_providers
            union
            select id from new_providers
        ) changed_provider
          on changed_provider.id = account.provider_catalog_id
          or changed_provider.id = model.provider_catalog_id
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
              when 'instance'::public.ai_scope_kind then true
              when 'workspace'::public.ai_scope_kind then
                  library.workspace_id = binding.workspace_id
              when 'library'::public.ai_scope_kind then
                  library.id = binding.library_id
              else false
          end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;


--
-- Name: invalidate_library_readable_projection(uuid); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.invalidate_library_readable_projection(target_library_id uuid) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    update public.catalog_library
    set readable_projection_version = null
    where id = target_library_id;
end
$$;


--
-- Name: ironrag_array_join(text[]); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.ironrag_array_join(input text[]) RETURNS text
    LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE
    AS $$
begin
    return array_to_string(input, ' ');
end;
$$;


--
-- Name: ironrag_unaccent(text); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.ironrag_unaccent(input text) RETURNS text
    LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE
    AS $$
begin
    return public.unaccent('public.unaccent'::regdictionary, input);
end;
$$;


--
-- Name: FUNCTION ironrag_unaccent(input text); Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON FUNCTION public.ironrag_unaccent(input text) IS 'Immutable wrapper used by generated script-agnostic FTS columns: to_tsvector(''simple'', ironrag_unaccent(...)).';


--
-- Name: maintain_projection_version_from_content_document(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.maintain_projection_version_from_content_document() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    target_library_id uuid;
begin
    if tg_op = 'UPDATE'
       and new.workspace_id is not distinct from old.workspace_id
       and new.library_id is not distinct from old.library_id
       and new.external_key is not distinct from old.external_key
       and new.document_state is not distinct from old.document_state
       and new.deleted_at is not distinct from old.deleted_at
       and new.parent_document_id is not distinct from old.parent_document_id
       and new.document_role is not distinct from old.document_role then
        return new;
    end if;

    target_library_id := case when tg_op = 'DELETE' then old.library_id else new.library_id end;
    perform public.invalidate_library_readable_projection(target_library_id);
    perform public.refresh_library_readable_projection_version(target_library_id);
    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end
$$;


--
-- Name: maintain_projection_version_from_content_head(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.maintain_projection_version_from_content_head() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    target_document_id uuid;
    target_library_id uuid;
begin
    if tg_op = 'UPDATE'
       and new.active_revision_id is not distinct from old.active_revision_id
       and new.readable_revision_id is not distinct from old.readable_revision_id then
        return new;
    end if;

    target_document_id := case when tg_op = 'DELETE' then old.document_id else new.document_id end;
    select document.library_id
    into target_library_id
    from public.content_document as document
    where document.id = target_document_id;

    if target_library_id is not null then
        perform public.invalidate_library_readable_projection(target_library_id);
        perform public.refresh_library_readable_projection_version(target_library_id);
    end if;
    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end
$$;


--
-- Name: maintain_projection_version_from_content_revision(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.maintain_projection_version_from_content_revision() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    target_revision_id uuid;
    target_library_id uuid;
begin
    if tg_op = 'UPDATE'
       and new.document_id is not distinct from old.document_id
       and new.workspace_id is not distinct from old.workspace_id
       and new.library_id is not distinct from old.library_id
       and new.revision_number is not distinct from old.revision_number
       and new.content_source_kind is not distinct from old.content_source_kind
       and new.source_uri is not distinct from old.source_uri
       and new.document_hint is not distinct from old.document_hint
       and new.mime_type is not distinct from old.mime_type
       and new.checksum is not distinct from old.checksum
       and new.title is not distinct from old.title
       and new.byte_size is not distinct from old.byte_size then
        return new;
    end if;

    target_revision_id := case when tg_op = 'DELETE' then old.id else new.id end;
    target_library_id := case when tg_op = 'DELETE' then old.library_id else new.library_id end;
    if exists (
        select 1
        from public.content_document_head as head
        where head.active_revision_id = target_revision_id
           or head.readable_revision_id = target_revision_id
    ) then
        perform public.invalidate_library_readable_projection(target_library_id);
        perform public.refresh_library_readable_projection_version(target_library_id);
    end if;
    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end
$$;


--
-- Name: maintain_projection_version_from_ingest_job(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.maintain_projection_version_from_ingest_job() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    target_library_id uuid;
begin
    if tg_op = 'UPDATE' and new.queue_state is not distinct from old.queue_state then
        return new;
    end if;

    target_library_id := case when tg_op = 'DELETE' then old.library_id else new.library_id end;
    perform public.invalidate_library_readable_projection(target_library_id);
    perform public.refresh_library_readable_projection_version(target_library_id);
    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end
$$;


--
-- Name: maintain_projection_version_from_knowledge_document(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.maintain_projection_version_from_knowledge_document() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    target_library_id uuid;
begin
    if tg_op = 'UPDATE'
       and new.workspace_id is not distinct from old.workspace_id
       and new.library_id is not distinct from old.library_id
       and new.external_key is not distinct from old.external_key
       and new.document_state is not distinct from old.document_state
       and new.active_revision_id is not distinct from old.active_revision_id
       and new.readable_revision_id is not distinct from old.readable_revision_id
       and new.deleted_at is not distinct from old.deleted_at
       and new.parent_document_id is not distinct from old.parent_document_id
       and new.document_role is not distinct from old.document_role then
        return new;
    end if;

    target_library_id := case when tg_op = 'DELETE' then old.library_id else new.library_id end;
    perform public.invalidate_library_readable_projection(target_library_id);
    perform public.refresh_library_readable_projection_version(target_library_id);
    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end
$$;


--
-- Name: maintain_projection_version_from_knowledge_revision(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.maintain_projection_version_from_knowledge_revision() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    target_revision_id uuid;
    target_library_id uuid;
begin
    if tg_op = 'UPDATE'
       and new.document_id is not distinct from old.document_id
       and new.workspace_id is not distinct from old.workspace_id
       and new.library_id is not distinct from old.library_id
       and new.revision_number is not distinct from old.revision_number
       and new.revision_kind is not distinct from old.revision_kind
       and new.source_uri is not distinct from old.source_uri
       and new.document_hint is not distinct from old.document_hint
       and new.mime_type is not distinct from old.mime_type
       and new.checksum is not distinct from old.checksum
       and new.title is not distinct from old.title
       and new.byte_size is not distinct from old.byte_size then
        return new;
    end if;

    target_revision_id := case when tg_op = 'DELETE' then old.revision_id else new.revision_id end;
    target_library_id := case when tg_op = 'DELETE' then old.library_id else new.library_id end;
    if exists (
        select 1
        from public.knowledge_document as document
        where document.library_id = target_library_id
          and (
                document.active_revision_id = target_revision_id
             or document.readable_revision_id = target_revision_id
          )
    ) then
        perform public.invalidate_library_readable_projection(target_library_id);
        perform public.refresh_library_readable_projection_version(target_library_id);
    end if;
    if tg_op = 'DELETE' then
        return old;
    end if;
    return new;
end
$$;


--
-- Name: preserve_content_lineage_owner(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.preserve_content_lineage_owner() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF TG_TABLE_NAME = 'content_document' THEN
        IF (NEW.id, NEW.workspace_id, NEW.library_id) IS DISTINCT FROM (OLD.id, OLD.workspace_id, OLD.library_id) THEN
            RAISE EXCEPTION 'document scope is immutable' USING ERRCODE = '23514';
        END IF;
    ELSIF (NEW.id, NEW.mutation_id, NEW.document_id) IS DISTINCT FROM (OLD.id, OLD.mutation_id, OLD.document_id) THEN
        RAISE EXCEPTION 'mutation item ownership is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;


--
-- Name: preserve_content_mutation_reference(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.preserve_content_mutation_reference() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM 1 FROM public.content_mutation_reference WHERE id=OLD.id FOR UPDATE;
        IF FOUND THEN
            RAISE EXCEPTION 'historical audit deletion requires registry parent deletion' USING ERRCODE = '23514';
        END IF;
        RETURN OLD;
    END IF;
    IF TG_TABLE_NAME = 'content_mutation_predecessor_audit' OR NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'mutation reference and historical audit are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;


--
-- Name: preserve_content_request_identity(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.preserve_content_request_identity() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.request_identity IS NULL OR NEW.request_fingerprint IS NULL
           OR jsonb_typeof(NEW.request_identity) <> 'object' THEN
            RAISE EXCEPTION 'new content mutation requires semantic identity and fingerprint' USING ERRCODE = '23514';
        END IF;
    ELSIF (NEW.id, NEW.workspace_id, NEW.library_id) IS DISTINCT FROM (OLD.id, OLD.workspace_id, OLD.library_id)
       OR NEW.request_identity IS DISTINCT FROM OLD.request_identity
       OR NEW.request_fingerprint IS DISTINCT FROM OLD.request_fingerprint THEN
        RAISE EXCEPTION 'content request identity is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;


--
-- Name: prevent_ai_account_scope_change(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.prevent_ai_account_scope_change() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    if row(new.scope_kind, new.workspace_id, new.library_id)
        is distinct from row(old.scope_kind, old.workspace_id, old.library_id) then
        raise check_violation using
            constraint = 'ai_account_scope_immutable',
            message = 'ai_account scope cannot change after creation';
    end if;
    return new;
end
$$;


--
-- Name: prevent_catalog_provisioning_owner_change(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.prevent_catalog_provisioning_owner_change() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    if new.provisioned_by_principal_id is distinct from old.provisioned_by_principal_id then
        raise exception 'catalog provisioning ownership is immutable';
    end if;
    return new;
end
$$;


--
-- Name: redact_webhook_delivery_diagnostics(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.redact_webhook_delivery_diagnostics() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    new.response_body_excerpt := null;
    if new.delivery_state = 'delivered'::webhook_delivery_state then
        new.error_code := null;
        new.error_message := null;
    elsif new.error_message is not null then
        new.error_message := case new.error_code
            when 'subscription_inactive' then 'Webhook subscription is inactive'
            when 'target_policy_rejected' then 'Webhook target was rejected by outbound network policy'
            when 'target_resolution_failed' then 'Webhook target could not be resolved to an allowed endpoint'
            when 'payload_encoding_failed' then 'Webhook payload could not be encoded'
            when 'credential_unavailable' then 'Protected webhook credentials could not be loaded'
            when 'client_setup_failed' then 'Outbound webhook client could not be initialized'
            when 'transport_timeout' then 'Outbound webhook request timed out'
            when 'transport_connect' then 'Outbound webhook endpoint could not be reached'
            when 'transport_request' then 'Outbound webhook request failed'
            when 'remote_http_status' then 'Remote endpoint returned an unsuccessful HTTP status'
            when 'operator_force_abandoned' then 'Webhook delivery was explicitly abandoned by an operator'
            else 'Webhook delivery failure detail was redacted'
        end;
    end if;
    return new;
end
$$;


--
-- Name: refresh_library_readable_projection_version(uuid); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.refresh_library_readable_projection_version(target_library_id uuid) RETURNS bigint
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    refreshed_version bigint;
begin
    update public.catalog_library as library
    set readable_projection_version = case
        when not exists (
            select 1
            from public.ingest_job as job
            where job.library_id = target_library_id
              and job.queue_state in ('queued', 'leased', 'paused')
        )
        and not exists (
            select 1
            from public.content_document as stored_document
            left join public.content_document_head as stored_head
              on stored_head.document_id = stored_document.id
            left join public.knowledge_document as projected_document
              on projected_document.document_id = stored_document.id
             and projected_document.library_id = stored_document.library_id
            where stored_document.library_id = target_library_id
              and stored_document.document_state = 'active'
              and stored_document.deleted_at is null
              and (
                    stored_head.document_id is null
                 or stored_head.active_revision_id is distinct from stored_head.readable_revision_id
                 or projected_document.document_id is null
                 or stored_document.workspace_id is distinct from projected_document.workspace_id
                 or stored_document.external_key is distinct from projected_document.external_key
                 or stored_document.parent_document_id is distinct from projected_document.parent_document_id
                 or stored_document.document_role::text is distinct from projected_document.document_role
                 or stored_head.active_revision_id is distinct from projected_document.active_revision_id
                 or stored_head.readable_revision_id is distinct from projected_document.readable_revision_id
                 or projected_document.document_state is distinct from 'active'
                 or projected_document.deleted_at is not null
              )
        )
        and not exists (
            select 1
            from public.knowledge_document as projected_document
            left join public.content_document as stored_document
              on stored_document.id = projected_document.document_id
             and stored_document.library_id = projected_document.library_id
            where projected_document.library_id = target_library_id
              and projected_document.document_state = 'active'
              and projected_document.deleted_at is null
              and (
                    stored_document.id is null
                 or stored_document.document_state is distinct from 'active'
                 or stored_document.deleted_at is not null
              )
        )
        and not exists (
            select 1
            from public.content_document as stored_document
            join public.content_document_head as stored_head
              on stored_head.document_id = stored_document.id
            join public.content_revision as stored_revision
              on stored_revision.id = stored_head.readable_revision_id
            left join public.knowledge_revision as projected_revision
              on projected_revision.revision_id = stored_revision.id
             and projected_revision.library_id = stored_revision.library_id
            where stored_document.library_id = target_library_id
              and stored_document.document_state = 'active'
              and stored_document.deleted_at is null
              and (
                    projected_revision.revision_id is null
                 or stored_revision.document_id is distinct from projected_revision.document_id
                 or stored_revision.workspace_id is distinct from projected_revision.workspace_id
                 or stored_revision.revision_number::bigint is distinct from projected_revision.revision_number
                 or stored_revision.content_source_kind::text is distinct from projected_revision.revision_kind
                 or stored_revision.source_uri is distinct from projected_revision.source_uri
                 or stored_revision.document_hint is distinct from projected_revision.document_hint
                 or stored_revision.mime_type is distinct from projected_revision.mime_type
                 or stored_revision.checksum is distinct from projected_revision.checksum
                 or stored_revision.title is distinct from projected_revision.title
                 or stored_revision.byte_size is distinct from projected_revision.byte_size
              )
        )
        then library.source_truth_version
        else null
    end
    where library.id = target_library_id
    returning library.readable_projection_version into refreshed_version;

    return refreshed_version;
end
$$;


--
-- Name: serialize_ai_config_generation_writes(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.serialize_ai_config_generation_writes() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    perform pg_advisory_xact_lock(
        hashtextextended('ironrag:ai-config-generation', 0)
    );
    return null;
end
$$;


--
-- Name: uuidv7(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.uuidv7() RETURNS uuid
    LANGUAGE plpgsql
    AS $$
declare
    unix_ts_ms bigint;
    uuid_bytes bytea;
begin
    unix_ts_ms = (extract(epoch from clock_timestamp()) * 1000)::bigint;
    uuid_bytes = set_byte(
        set_byte(
            overlay(
                uuid_send(gen_random_uuid())
                placing substring(int8send(unix_ts_ms) from 3)
                from 1 for 6
            ),
            6, (get_byte(uuid_send(gen_random_uuid()), 6) & 15) | 112
        ),
        8, (get_byte(uuid_send(gen_random_uuid()), 8) & 63) | 128
    );
    return encode(uuid_bytes, 'hex')::uuid;
end
$$;


--
-- Name: verify_content_mutation_lineage(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.verify_content_mutation_lineage() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    affected jsonb := to_jsonb(NEW);
    mutation uuid := (affected->>'mutation_id')::uuid;
    affected_document uuid := (affected->>'document_id')::uuid;
    item_id uuid;
    job_id uuid;
BEGIN
    IF TG_TABLE_NAME = 'content_mutation_item' THEN item_id := NEW.id;
    ELSIF TG_TABLE_NAME = 'ingest_job' THEN item_id := NEW.mutation_item_id; job_id := NEW.id;
    ELSE mutation := NEW.latest_mutation_id;
    END IF;
    IF EXISTS (SELECT 1 FROM public.content_mutation_item item
        JOIN public.content_mutation_reference owner ON owner.id = item.mutation_id
        JOIN public.content_document document ON document.id = item.document_id
        WHERE item.id = item_id AND (owner.workspace_id, owner.library_id) <> (document.workspace_id, document.library_id))
       OR EXISTS (SELECT 1 FROM public.content_mutation_item item
        LEFT JOIN public.content_revision result ON result.id=item.result_revision_id
        LEFT JOIN public.content_revision base ON base.id=item.base_revision_id
        WHERE item.id=item_id AND ((result.id IS NOT NULL AND result.document_id<>item.document_id)
            OR (base.id IS NOT NULL AND base.document_id<>item.document_id)))
       OR EXISTS (SELECT 1 FROM public.content_document_head head
        JOIN public.content_mutation_reference owner ON owner.id = head.latest_mutation_id
        JOIN public.content_document document ON document.id = head.document_id
        WHERE head.document_id = affected_document AND (owner.workspace_id, owner.library_id) <> (document.workspace_id, document.library_id))
       OR EXISTS (SELECT 1 FROM public.ingest_job job
        JOIN public.content_mutation_item item ON item.id = job.mutation_item_id
        WHERE (job.id = job_id OR item.id = item_id)
          AND (job.mutation_id IS DISTINCT FROM item.mutation_id
           OR job.knowledge_document_id IS DISTINCT FROM item.document_id))
       OR EXISTS (SELECT 1 FROM public.content_mutation_predecessor_audit audit
        JOIN public.content_mutation_item item ON item.mutation_id = audit.id
        WHERE audit.id = mutation AND (item.item_state NOT IN ('applied', 'failed', 'conflicted', 'skipped') OR item.visibility_intent_id IS NOT NULL)) THEN
        RAISE EXCEPTION 'mutation lineage scope or historical item state is invalid' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;


--
-- Name: verify_content_mutation_representation(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.verify_content_mutation_representation() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE mutation uuid; representations integer;
BEGIN
    mutation := COALESCE(NEW.id, OLD.id);
    PERFORM 1 FROM public.content_mutation_reference WHERE id = mutation FOR UPDATE;
    IF NOT FOUND THEN RETURN NULL; END IF;
    SELECT (SELECT count(*) FROM public.content_mutation WHERE id = mutation)
         + (SELECT count(*) FROM public.content_mutation_predecessor_audit WHERE id = mutation)
        INTO representations;
    IF representations <> 1 THEN
        RAISE EXCEPTION 'mutation reference requires exactly one representation' USING ERRCODE = '23514';
    END IF;
    IF EXISTS (SELECT 1 FROM public.content_mutation_predecessor_audit audit
               JOIN public.content_mutation_item item ON item.mutation_id = audit.id
               WHERE audit.id = mutation AND (item.item_state NOT IN ('applied', 'failed', 'conflicted', 'skipped')
                 OR item.visibility_intent_id IS NOT NULL)) THEN
        RAISE EXCEPTION 'historical mutation items must be terminal and unbound' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: ai_account; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ai_account (
    id uuid DEFAULT uuidv7() CONSTRAINT ai_provider_credential_id_not_null NOT NULL,
    workspace_id uuid,
    provider_catalog_id uuid CONSTRAINT ai_provider_credential_provider_catalog_id_not_null NOT NULL,
    label text CONSTRAINT ai_provider_credential_label_not_null NOT NULL,
    api_key text,
    base_url text,
    credential_state public.ai_account_state DEFAULT 'active'::public.ai_account_state CONSTRAINT ai_provider_credential_credential_state_not_null NOT NULL,
    scope_kind public.ai_scope_kind CONSTRAINT ai_provider_credential_scope_kind_not_null NOT NULL,
    library_id uuid,
    created_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() CONSTRAINT ai_provider_credential_created_at_not_null NOT NULL,
    updated_at timestamp with time zone DEFAULT now() CONSTRAINT ai_provider_credential_updated_at_not_null NOT NULL,
    CONSTRAINT ai_account_scope_check CHECK ((((scope_kind = 'instance'::public.ai_scope_kind) AND (workspace_id IS NULL) AND (library_id IS NULL)) OR ((scope_kind = 'workspace'::public.ai_scope_kind) AND (workspace_id IS NOT NULL) AND (library_id IS NULL)) OR ((scope_kind = 'library'::public.ai_scope_kind) AND (workspace_id IS NOT NULL) AND (library_id IS NOT NULL))))
);


--
-- Name: ai_binding; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ai_binding (
    id uuid DEFAULT uuidv7() CONSTRAINT ai_binding_assignment_id_not_null NOT NULL,
    workspace_id uuid,
    library_id uuid,
    binding_purpose public.ai_binding_purpose CONSTRAINT ai_binding_assignment_binding_purpose_not_null NOT NULL,
    account_id uuid CONSTRAINT ai_binding_assignment_provider_credential_id_not_null NOT NULL,
    provider_catalog_id uuid NOT NULL,
    binding_state public.ai_binding_state DEFAULT 'active'::public.ai_binding_state CONSTRAINT ai_binding_assignment_binding_state_not_null NOT NULL,
    scope_kind public.ai_scope_kind CONSTRAINT ai_binding_assignment_scope_kind_not_null NOT NULL,
    updated_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() CONSTRAINT ai_binding_assignment_created_at_not_null NOT NULL,
    updated_at timestamp with time zone DEFAULT now() CONSTRAINT ai_binding_assignment_updated_at_not_null NOT NULL,
    model_catalog_id uuid NOT NULL,
    system_prompt text,
    temperature double precision,
    top_p double precision,
    max_output_tokens_override integer,
    extra_parameters_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    CONSTRAINT ai_binding_scope_check CHECK ((((scope_kind = 'instance'::public.ai_scope_kind) AND (workspace_id IS NULL) AND (library_id IS NULL)) OR ((scope_kind = 'workspace'::public.ai_scope_kind) AND (workspace_id IS NOT NULL) AND (library_id IS NULL)) OR ((scope_kind = 'library'::public.ai_scope_kind) AND (workspace_id IS NOT NULL) AND (library_id IS NOT NULL))))
);


--
-- Name: ai_binding_validation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ai_binding_validation (
    id uuid DEFAULT uuidv7() NOT NULL,
    binding_id uuid NOT NULL,
    validation_state public.ai_validation_state NOT NULL,
    checked_at timestamp with time zone DEFAULT now() NOT NULL,
    failure_code text,
    message text
);


--
-- Name: ai_model_binding_purpose; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ai_model_binding_purpose (
    model_catalog_id uuid NOT NULL,
    binding_purpose public.ai_binding_purpose NOT NULL
);


--
-- Name: ai_model_catalog; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ai_model_catalog (
    id uuid NOT NULL,
    provider_catalog_id uuid NOT NULL,
    model_name text NOT NULL,
    context_window integer,
    max_output_tokens integer,
    lifecycle_state public.ai_model_lifecycle_state DEFAULT 'active'::public.ai_model_lifecycle_state NOT NULL,
    metadata_json jsonb NOT NULL,
    currency_code text DEFAULT 'USD'::text NOT NULL,
    input_price_per_1m numeric(18,8) DEFAULT 0 NOT NULL,
    cached_input_price_per_1m numeric(18,8) DEFAULT 0 NOT NULL,
    cache_write_input_price_per_1m numeric(18,8) DEFAULT 0 NOT NULL,
    output_price_per_1m numeric(18,8) DEFAULT 0 NOT NULL,
    long_context_threshold_tokens integer,
    long_context_input_price_per_1m numeric(18,8),
    long_context_cached_input_price_per_1m numeric(18,8),
    long_context_output_price_per_1m numeric(18,8),
    CONSTRAINT ai_model_catalog_long_context_all_or_none CHECK ((((long_context_threshold_tokens IS NULL) AND (long_context_input_price_per_1m IS NULL) AND (long_context_cached_input_price_per_1m IS NULL) AND (long_context_output_price_per_1m IS NULL)) OR ((long_context_threshold_tokens IS NOT NULL) AND (long_context_threshold_tokens > 0) AND (long_context_input_price_per_1m IS NOT NULL) AND (long_context_input_price_per_1m >= (0)::numeric) AND (long_context_cached_input_price_per_1m IS NOT NULL) AND (long_context_cached_input_price_per_1m >= (0)::numeric) AND (long_context_output_price_per_1m IS NOT NULL) AND (long_context_output_price_per_1m >= (0)::numeric)))),
    CONSTRAINT ai_model_catalog_prices_nonnegative CHECK (((input_price_per_1m >= (0)::numeric) AND (cached_input_price_per_1m >= (0)::numeric) AND (cache_write_input_price_per_1m >= (0)::numeric) AND (output_price_per_1m >= (0)::numeric)))
);


--
-- Name: ai_provider_catalog; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ai_provider_catalog (
    id uuid NOT NULL,
    provider_kind text NOT NULL,
    display_name text NOT NULL,
    api_style public.ai_provider_api_style NOT NULL,
    lifecycle_state public.ai_provider_lifecycle_state DEFAULT 'active'::public.ai_provider_lifecycle_state NOT NULL,
    default_base_url text,
    capability_flags_json jsonb DEFAULT '{}'::jsonb NOT NULL
);


--
-- Name: audit_event; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.audit_event (
    id uuid DEFAULT uuidv7() NOT NULL,
    actor_principal_id uuid,
    surface_kind public.surface_kind NOT NULL,
    action_kind text NOT NULL,
    request_id text,
    trace_id text,
    result_kind public.audit_result_kind NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    redacted_message text,
    internal_message text
);


--
-- Name: audit_event_subject; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.audit_event_subject (
    audit_event_id uuid NOT NULL,
    subject_kind text NOT NULL,
    subject_id uuid NOT NULL,
    workspace_id uuid,
    library_id uuid,
    document_id uuid
);


--
-- Name: billing_charge; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.billing_charge (
    id uuid DEFAULT uuidv7() NOT NULL,
    usage_id uuid NOT NULL,
    currency_code text NOT NULL,
    unit_price numeric(18,8) NOT NULL,
    total_price numeric(18,8) NOT NULL,
    priced_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: billing_execution_cost; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.billing_execution_cost (
    id uuid DEFAULT uuidv7() NOT NULL,
    owning_execution_kind public.billing_owning_execution_kind NOT NULL,
    owning_execution_id uuid NOT NULL,
    total_cost numeric(18,8) DEFAULT 0 NOT NULL,
    currency_code text NOT NULL,
    provider_call_count integer DEFAULT 0 NOT NULL,
    unpriced_provider_call_count integer DEFAULT 0 NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    knowledge_document_id uuid
);


--
-- Name: billing_execution_cost_rollup_state; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.billing_execution_cost_rollup_state (
    owning_execution_kind public.billing_owning_execution_kind CONSTRAINT billing_execution_cost_rollup_st_owning_execution_kind_not_null NOT NULL,
    owning_execution_id uuid CONSTRAINT billing_execution_cost_rollup_stat_owning_execution_id_not_null NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    dirty_generation bigint DEFAULT 1 NOT NULL,
    applied_generation bigint DEFAULT 0 NOT NULL,
    dirty_at timestamp with time zone DEFAULT now() NOT NULL,
    applied_at timestamp with time zone,
    repair_attempts integer DEFAULT 0 NOT NULL,
    next_attempt_at timestamp with time zone DEFAULT now() NOT NULL,
    last_error text,
    terminal_error_code text,
    CONSTRAINT billing_execution_cost_rollup_state_attempts_nonnegative CHECK ((repair_attempts >= 0)),
    CONSTRAINT billing_execution_cost_rollup_state_generations CHECK (((dirty_generation >= 1) AND (applied_generation >= 0) AND (applied_generation <= dirty_generation))),
    CONSTRAINT billing_execution_cost_rollup_state_last_error_length CHECK (((last_error IS NULL) OR (char_length(last_error) <= 512))),
    CONSTRAINT billing_execution_cost_rollup_state_terminal_error CHECK (((terminal_error_code IS NULL) OR ((terminal_error_code = 'mixed_currency'::text) AND (applied_generation = dirty_generation) AND (applied_at IS NOT NULL))))
);


--
-- Name: billing_provider_call; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.billing_provider_call (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    binding_id uuid,
    owning_execution_kind public.billing_owning_execution_kind NOT NULL,
    owning_execution_id uuid NOT NULL,
    provider_catalog_id uuid NOT NULL,
    model_catalog_id uuid NOT NULL,
    call_kind text NOT NULL,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    call_state public.billing_call_state DEFAULT 'started'::public.billing_call_state NOT NULL,
    runtime_execution_id uuid,
    runtime_task_kind public.runtime_task_kind,
    CONSTRAINT chk_billing_provider_call_runtime_attribution CHECK ((((runtime_execution_id IS NULL) AND (runtime_task_kind IS NULL)) OR ((runtime_execution_id IS NOT NULL) AND (runtime_task_kind IS NOT NULL))))
);


--
-- Name: billing_usage; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.billing_usage (
    id uuid DEFAULT uuidv7() NOT NULL,
    provider_call_id uuid NOT NULL,
    usage_kind text NOT NULL,
    billing_unit public.billing_unit NOT NULL,
    quantity numeric(18,6) NOT NULL,
    observed_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT billing_usage_quantity_nonnegative CHECK ((quantity >= (0)::numeric))
);


--
-- Name: catalog_library; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.catalog_library (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    slug text NOT NULL,
    display_name text NOT NULL,
    description text,
    lifecycle_state public.catalog_library_lifecycle_state DEFAULT 'active'::public.catalog_library_lifecycle_state NOT NULL,
    source_truth_version bigint DEFAULT 1 NOT NULL,
    readable_projection_version bigint,
    extraction_prompt text,
    ai_summary text,
    created_by_principal_id uuid,
    provisioned_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    web_ingest_policy jsonb DEFAULT '{"crawlFilter": {"allowPatterns": [], "blockPatterns": []}, "materializationFilter": {"allowPatterns": [], "blockPatterns": []}}'::jsonb NOT NULL,
    chunking_template text DEFAULT 'naive'::text NOT NULL,
    recognition_policy jsonb DEFAULT '{"rasterImageEngine": "vision"}'::jsonb NOT NULL,
    include_document_hint_in_mcp_answers boolean DEFAULT true NOT NULL,
    retrieval_config jsonb DEFAULT '{"lexical": {"textSearchConfig": "simple"}}'::jsonb NOT NULL,
    CONSTRAINT catalog_library_recognition_policy_object_check CHECK ((jsonb_typeof(recognition_policy) = 'object'::text)),
    CONSTRAINT catalog_library_recognition_policy_raster_engine_check CHECK (((recognition_policy ? 'rasterImageEngine'::text) AND ((recognition_policy ->> 'rasterImageEngine'::text) = ANY (ARRAY['docling'::text, 'vision'::text])) AND ((recognition_policy - 'rasterImageEngine'::text) = '{}'::jsonb))),
    CONSTRAINT catalog_library_retrieval_config_object_check CHECK ((jsonb_typeof(retrieval_config) = 'object'::text)),
    CONSTRAINT catalog_library_slug_check CHECK ((slug ~ '^[a-z0-9]+(?:[-_][a-z0-9]+)*$'::text)),
    CONSTRAINT catalog_library_web_ingest_policy_object_check CHECK ((jsonb_typeof(web_ingest_policy) = 'object'::text))
);


--
-- Name: catalog_library_connector; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.catalog_library_connector (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    connector_kind public.catalog_connector_kind NOT NULL,
    display_name text NOT NULL,
    configuration_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    sync_mode public.catalog_connector_sync_mode DEFAULT 'manual'::public.catalog_connector_sync_mode NOT NULL,
    last_sync_requested_at timestamp with time zone,
    created_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: catalog_workspace; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.catalog_workspace (
    id uuid DEFAULT uuidv7() NOT NULL,
    slug text NOT NULL,
    display_name text NOT NULL,
    lifecycle_state public.catalog_workspace_lifecycle_state DEFAULT 'active'::public.catalog_workspace_lifecycle_state NOT NULL,
    created_by_principal_id uuid,
    provisioned_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT catalog_workspace_slug_check CHECK ((slug ~ '^[a-z0-9]+(?:[-_][a-z0-9]+)*$'::text))
);


--
-- Name: content_chunk; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_chunk (
    id uuid DEFAULT uuidv7() NOT NULL,
    revision_id uuid NOT NULL,
    chunk_index integer NOT NULL,
    start_offset integer NOT NULL,
    end_offset integer NOT NULL,
    token_count integer,
    normalized_text text NOT NULL,
    text_checksum text NOT NULL,
    source_page integer,
    source_bbox real[],
    window_text text,
    occurred_at timestamp with time zone,
    occurred_until timestamp with time zone,
    CONSTRAINT content_chunk_check CHECK ((end_offset >= start_offset)),
    CONSTRAINT content_chunk_occurred_range_check CHECK (((occurred_at IS NULL) OR (occurred_until IS NULL) OR (occurred_until >= occurred_at))),
    CONSTRAINT content_chunk_start_offset_check CHECK ((start_offset >= 0))
);


--
-- Name: content_document; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_document (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    external_key text NOT NULL,
    document_state public.content_document_state DEFAULT 'active'::public.content_document_state NOT NULL,
    created_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    deleted_at timestamp with time zone,
    parent_document_id uuid,
    parent_external_key text,
    document_role text DEFAULT 'primary'::text NOT NULL,
    source_file_name text,
    revision_sequence integer DEFAULT 0 NOT NULL,
    CONSTRAINT content_document_no_self_parent_ck CHECK (((parent_document_id IS NULL) OR (parent_document_id <> id))),
    CONSTRAINT content_document_revision_sequence_check CHECK ((revision_sequence >= 0)),
    CONSTRAINT content_document_role_ck CHECK ((document_role = ANY (ARRAY['primary'::text, 'attachment'::text, 'attached_context'::text])))
);


--
-- Name: content_document_head; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_document_head (
    document_id uuid NOT NULL,
    active_revision_id uuid,
    readable_revision_id uuid,
    latest_mutation_id uuid,
    latest_successful_attempt_id uuid,
    document_summary text,
    head_updated_at timestamp with time zone DEFAULT now() NOT NULL,
    recovery_attempts_count integer DEFAULT 0 NOT NULL,
    last_recovery_error_code text,
    last_recovery_attempt_at timestamp with time zone,
    dead_letter_at timestamp with time zone,
    visibility_intent_id uuid
);


--
-- Name: content_mutation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_mutation (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    operation_kind public.content_mutation_operation_kind NOT NULL,
    requested_by_principal_id uuid,
    source_identity text,
    mutation_state public.content_mutation_state DEFAULT 'accepted'::public.content_mutation_state NOT NULL,
    requested_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    failure_code text,
    conflict_code text,
    request_fingerprint text,
    request_identity jsonb NOT NULL,
    CONSTRAINT content_mutation_identity_object_check CHECK ((jsonb_typeof(request_identity) = 'object'::text))
);


--
-- Name: COLUMN content_mutation.request_fingerprint; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.content_mutation.request_fingerprint IS 'Versioned SHA-256 fingerprint of the complete structural mutation request (target, source identity, and payload identity).';


--
-- Name: content_mutation_item; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_mutation_item (
    id uuid DEFAULT uuidv7() NOT NULL,
    mutation_id uuid NOT NULL,
    document_id uuid NOT NULL,
    base_revision_id uuid,
    result_revision_id uuid,
    item_state public.content_mutation_item_state DEFAULT 'pending'::public.content_mutation_item_state NOT NULL,
    message text,
    visibility_intent_id uuid
);


--
-- Name: content_revision; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_revision (
    id uuid DEFAULT uuidv7() NOT NULL,
    document_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    revision_number integer NOT NULL,
    parent_revision_id uuid,
    content_source_kind public.content_source_kind NOT NULL,
    checksum text NOT NULL,
    mime_type text NOT NULL,
    byte_size bigint NOT NULL,
    title text,
    language_code text,
    source_uri text,
    storage_key text,
    created_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    document_hint text,
    admission_identity jsonb,
    CONSTRAINT content_revision_document_hint_check CHECK (((document_hint IS NULL) OR (length(document_hint) <= 1024)))
);


--
-- Name: content_visibility_intent; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_visibility_intent (
    mutation_item_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision_floor integer NOT NULL,
    CONSTRAINT content_visibility_intent_revision_floor_check CHECK ((revision_floor >= 0))
);


--
-- Name: knowledge_revision; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_revision (
    revision_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision_number bigint NOT NULL,
    revision_state text NOT NULL,
    revision_kind text NOT NULL,
    storage_ref text,
    source_uri text,
    document_hint text,
    mime_type text NOT NULL,
    checksum text NOT NULL,
    title text,
    byte_size bigint NOT NULL,
    normalized_text text,
    text_checksum text,
    image_checksum text,
    text_state text NOT NULL,
    vector_state text NOT NULL,
    graph_state text NOT NULL,
    text_readable_at timestamp with time zone,
    vector_ready_at timestamp with time zone,
    graph_ready_at timestamp with time zone,
    superseded_by_revision_id uuid,
    created_at timestamp with time zone NOT NULL,
    CONSTRAINT knowledge_revision_graph_state_canonical_check CHECK ((graph_state = ANY (ARRAY['accepted'::text, 'processing'::text, 'ready'::text, 'graph_degraded'::text, 'failed'::text]))),
    CONSTRAINT knowledge_revision_text_state_canonical_check CHECK ((text_state = ANY (ARRAY['accepted'::text, 'extracting_text'::text, 'text_readable'::text, 'failed'::text]))),
    CONSTRAINT knowledge_revision_vector_state_canonical_check CHECK ((vector_state = ANY (ARRAY['accepted'::text, 'processing'::text, 'ready'::text, 'failed'::text])))
);


--
-- Name: TABLE knowledge_revision; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_revision IS 'Replaces Arango collection knowledge_revision and collapses knowledge_document_revision_edge into document_id.';


--
-- Name: content_current_revision; Type: VIEW; Schema: public; Owner: -
--

CREATE FUNCTION public.preserve_revision_admission_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE identity jsonb := NEW.admission_identity; replacement jsonb; provenance jsonb;
BEGIN
    IF TG_OP = 'UPDATE' AND (NEW.admission_identity IS DISTINCT FROM OLD.admission_identity
        OR (NEW.id, NEW.document_id, NEW.workspace_id, NEW.library_id) IS DISTINCT FROM
           (OLD.id, OLD.document_id, OLD.workspace_id, OLD.library_id)) THEN
        RAISE EXCEPTION 'revision admission and ownership are immutable' USING ERRCODE = '23514';
    END IF;
    IF identity IS NULL THEN RETURN NEW; END IF;
    replacement := identity->'replacement';
    IF NOT COALESCE(jsonb_typeof(identity) = 'object'
        AND jsonb_typeof(replacement) = 'object'
        AND replacement ?& ARRAY['checksum','byteSize','mimeType','fileName','title','languageCode','sourceUri','documentHint']
        AND replacement - ARRAY['checksum','byteSize','mimeType','fileName','title','languageCode','sourceUri','documentHint'] = '{}'::jsonb
        AND replacement->>'checksum' = NEW.checksum
        AND replacement->'byteSize' = to_jsonb(NEW.byte_size)
        AND replacement->>'mimeType' = NEW.mime_type
        AND jsonb_typeof(replacement->'checksum') = 'string'
        AND replacement->>'checksum' ~ '^sha256:[0-9a-f]{64}$'
        AND NEW.byte_size >= 0
        AND jsonb_typeof(replacement->'mimeType') = 'string'
        AND NEW.mime_type <> '' AND NEW.mime_type = lower(btrim(NEW.mime_type))
        AND jsonb_typeof(replacement->'fileName') = 'string'
        AND replacement->>'fileName' ~ '[^[:space:]]'
        AND replacement->>'fileName' = btrim(replacement->>'fileName')
        AND replacement->>'fileName' !~ '[/\\[:cntrl:]]'
        AND replacement->>'fileName' NOT IN ('.', '..')
        AND replacement->'title' = COALESCE(to_jsonb(NEW.title), 'null'::jsonb)
        AND replacement->'languageCode' = COALESCE(to_jsonb(NEW.language_code), 'null'::jsonb)
        AND replacement->'sourceUri' = COALESCE(to_jsonb(NEW.source_uri), 'null'::jsonb)
        AND jsonb_typeof(replacement->'documentHint') IN ('string','null'), false) THEN
        RAISE EXCEPTION 'revision admission descriptor is invalid' USING ERRCODE = '23514';
    END IF;
    IF identity->>'origin' = 'mutation' THEN
        IF NOT COALESCE(identity ?& ARRAY['origin','replacement','sourceIdentity','materializationToken','visibilityIntentId']
            AND identity - ARRAY['origin','replacement','sourceIdentity','materializationToken','visibilityIntentId'] = '{}'::jsonb
            AND jsonb_typeof(identity->'sourceIdentity') IN ('string','null')
            AND jsonb_typeof(identity->'materializationToken') IN ('string','null')
            AND (identity->'visibilityIntentId' = 'null'::jsonb OR (
                jsonb_typeof(identity->'visibilityIntentId') = 'string'
                AND identity->>'visibilityIntentId' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                AND jsonb_typeof(identity->'sourceIdentity') = 'string'
                AND identity->>'sourceIdentity' ~ '[^[:space:]]')), false) THEN
            RAISE EXCEPTION 'revision mutation admission is invalid' USING ERRCODE = '23514';
        END IF;
    ELSIF identity->>'origin' = 'certified_import' THEN
        provenance := identity->'provenance';
        IF NOT COALESCE(identity - ARRAY['origin','replacement','provenance'] = '{}'::jsonb
            AND jsonb_typeof(provenance) = 'object'
            AND provenance ?& ARRAY['recoverySnapshotId','identityCertificateSha256','sourceBaselineSha384','admittedBy']
            AND provenance - ARRAY['recoverySnapshotId','identityCertificateSha256','sourceBaselineSha384','admittedBy'] = '{}'::jsonb
            AND jsonb_typeof(provenance->'recoverySnapshotId') = 'string'
            AND provenance->>'recoverySnapshotId' ~ '^[0-9a-f]{64}$'
            AND jsonb_typeof(provenance->'identityCertificateSha256') = 'string'
            AND provenance->>'identityCertificateSha256' ~ '^[0-9a-f]{64}$'
            AND jsonb_typeof(provenance->'sourceBaselineSha384') = 'string'
            AND provenance->>'sourceBaselineSha384' ~ '^[0-9a-f]{96}$'
            AND jsonb_typeof(provenance->'admittedBy') = 'string'
            AND provenance->>'admittedBy' ~ '[^[:space:]]'
            AND provenance->>'admittedBy' !~ '[[:cntrl:]]', false) THEN
            RAISE EXCEPTION 'revision import provenance is invalid' USING ERRCODE = '23514';
        END IF;
    ELSE
        RAISE EXCEPTION 'unknown revision admission origin' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER content_revision_admission_immutable BEFORE INSERT OR UPDATE ON public.content_revision
    FOR EACH ROW EXECUTE FUNCTION public.preserve_revision_admission_identity();
CREATE FUNCTION public.verify_head_revision_admission() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM public.content_document_head head
        JOIN public.content_document document ON document.id = head.document_id
        LEFT JOIN public.content_revision revision ON revision.id = head.readable_revision_id
        WHERE head.document_id = NEW.document_id AND head.readable_revision_id IS NOT NULL
          AND (revision.id IS NULL OR revision.admission_identity IS NULL
            OR (revision.document_id, revision.workspace_id, revision.library_id) IS DISTINCT FROM
               (document.id, document.workspace_id, document.library_id))) THEN
        RAISE EXCEPTION 'readable head requires an admitted revision in its scope' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER content_document_head_revision_admission AFTER INSERT OR UPDATE ON public.content_document_head
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_head_revision_admission();


CREATE VIEW public.content_current_revision AS
SELECT d.id AS document_id, d.workspace_id, d.library_id, r.id AS revision_id,
       h.visibility_intent_id,
       r.admission_identity->'replacement'->>'fileName' AS source_file_name
FROM public.content_document d
JOIN public.content_document_head h ON h.document_id = d.id
JOIN public.content_revision r ON r.id = h.readable_revision_id AND r.document_id = d.id
    AND r.workspace_id = d.workspace_id AND r.library_id = d.library_id
JOIN public.knowledge_revision k ON k.revision_id = r.id AND k.document_id = d.id
    AND k.workspace_id = d.workspace_id AND k.library_id = d.library_id
LEFT JOIN public.content_visibility_intent intent
    ON intent.mutation_item_id = h.visibility_intent_id AND intent.document_id = d.id
LEFT JOIN public.content_mutation_item intent_item ON intent_item.id = intent.mutation_item_id
LEFT JOIN public.content_mutation intent_mutation ON intent_mutation.id = intent_item.mutation_id
WHERE d.document_state = 'active' AND d.deleted_at IS NULL
  AND jsonb_typeof(r.admission_identity) = 'object'
  AND k.text_state = 'text_readable' AND k.text_readable_at IS NOT NULL
  AND (h.visibility_intent_id IS NULL OR (
      intent.mutation_item_id IS NOT NULL
      AND r.admission_identity->>'origin' = 'mutation'
      AND r.admission_identity->>'visibilityIntentId' = intent.mutation_item_id::text
      AND r.revision_number > intent.revision_floor
      AND intent_mutation.workspace_id = d.workspace_id AND intent_mutation.library_id = d.library_id
      AND k.vector_state = 'ready' AND k.vector_ready_at IS NOT NULL
      AND k.graph_state IN ('ready', 'graph_degraded')
      AND r.admission_identity->'replacement'->>'checksum' = r.checksum
      AND r.admission_identity->'replacement'->'byteSize' = to_jsonb(r.byte_size)
      AND r.admission_identity->'replacement'->>'mimeType' = r.mime_type
      AND jsonb_typeof(r.admission_identity->'replacement'->'fileName') = 'string'
      AND r.admission_identity->'replacement'->>'fileName' ~ '[^[:space:]]'
      AND r.admission_identity->'replacement'->>'title' IS NOT DISTINCT FROM r.title
      AND r.admission_identity->'replacement'->>'languageCode' IS NOT DISTINCT FROM r.language_code
      AND r.admission_identity->'replacement'->>'sourceUri' IS NOT DISTINCT FROM r.source_uri
      AND r.admission_identity->'replacement'->>'documentHint' IS NOT DISTINCT FROM r.document_hint
      AND r.admission_identity->'replacement' = intent_mutation.request_identity->'replacement'
      AND r.admission_identity->'sourceIdentity' = intent_mutation.request_identity->'sourceIdentity'
      AND r.admission_identity->'materializationToken' = intent_mutation.request_identity->'materializationToken'
  ));


--
-- Name: content_mutation_predecessor_audit; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_mutation_predecessor_audit (
    id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    operation_kind public.content_mutation_operation_kind NOT NULL,
    requested_by_principal_id uuid,
    source_identity text,
    mutation_state public.content_mutation_state NOT NULL,
    requested_at timestamp with time zone NOT NULL,
    completed_at timestamp with time zone,
    failure_code text,
    conflict_code text,
    request_fingerprint text,
    request_identity jsonb,
    predecessor_baseline_sha384 bytea CONSTRAINT content_mutation_predecesso_predecessor_baseline_sha38_not_null NOT NULL,
    origin_workspace_id uuid NOT NULL,
    origin_library_id uuid NOT NULL,
    CONSTRAINT content_mutation_predecessor_audit_certificate_check CHECK ((predecessor_baseline_sha384 = decode('bc9ee0ed6d3b14c32ced3759377f9323d7944e2e18cebb09f8e841dfaa9a20b123bd3532654b8d4edba8efaf2e4768bb'::text, 'hex'::text))),
    CONSTRAINT content_mutation_predecessor_audit_identity_check CHECK ((request_identity IS NULL)),
    CONSTRAINT content_mutation_predecessor_audit_terminal_check CHECK ((mutation_state = ANY (ARRAY['applied'::public.content_mutation_state, 'failed'::public.content_mutation_state, 'conflicted'::public.content_mutation_state, 'canceled'::public.content_mutation_state])))
);


--
-- Name: content_mutation_reference; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_mutation_reference (
    id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    request_surface public.surface_kind NOT NULL,
    idempotency_scope text,
    idempotency_key text,
    reservation_principal_id uuid,
    CONSTRAINT content_mutation_reference_reservable_check CHECK (((idempotency_key IS NULL) OR (idempotency_scope IS NOT NULL) OR (reservation_principal_id IS NOT NULL)))
);


--
-- Name: content_revision_ingest_unit; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_revision_ingest_unit (
    revision_id uuid NOT NULL,
    stage_name text NOT NULL,
    unit_ordinal integer NOT NULL,
    unit_kind text NOT NULL,
    range_start integer NOT NULL,
    range_end integer NOT NULL,
    unit_state text NOT NULL,
    content_text text,
    structure_hints_json jsonb,
    source_metadata_json jsonb,
    source_map_json jsonb,
    warnings_json jsonb DEFAULT '[]'::jsonb NOT NULL,
    usage_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    provider_kind text,
    model_name text,
    content_checksum text,
    details_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    attempt_id uuid,
    elapsed_ms bigint,
    failure_code text,
    failure_message text,
    started_at timestamp with time zone,
    completed_at timestamp with time zone,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT content_revision_ingest_unit_check CHECK ((range_end >= range_start)),
    CONSTRAINT content_revision_ingest_unit_range_start_check CHECK ((range_start > 0)),
    CONSTRAINT content_revision_ingest_unit_unit_state_check CHECK ((unit_state = ANY (ARRAY['started'::text, 'completed'::text, 'failed'::text])))
);


--
-- Name: content_web_discovered_page; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_web_discovered_page (
    id uuid DEFAULT uuidv7() NOT NULL,
    run_id uuid NOT NULL,
    discovered_url text,
    normalized_url text NOT NULL,
    final_url text,
    canonical_url text,
    depth integer NOT NULL,
    referrer_candidate_id uuid,
    host_classification public.web_candidate_host_classification NOT NULL,
    candidate_state public.web_candidate_state DEFAULT 'discovered'::public.web_candidate_state NOT NULL,
    classification_reason text,
    content_type text,
    http_status integer,
    discovered_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    document_id uuid,
    result_revision_id uuid,
    mutation_item_id uuid,
    snapshot_storage_key text,
    classification_detail text,
    CONSTRAINT content_web_discovered_page_depth_check CHECK ((depth >= 0))
);


--
-- Name: content_web_ingest_run; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.content_web_ingest_run (
    id uuid DEFAULT uuidv7() NOT NULL,
    mutation_id uuid NOT NULL,
    async_operation_id uuid,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    mode public.web_ingest_mode NOT NULL,
    seed_url text NOT NULL,
    normalized_seed_url text NOT NULL,
    boundary_policy public.web_boundary_policy NOT NULL,
    max_depth integer NOT NULL,
    max_pages integer NOT NULL,
    run_state public.web_run_state DEFAULT 'accepted'::public.web_run_state NOT NULL,
    requested_by_principal_id uuid,
    requested_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    failure_code text,
    cancel_requested_at timestamp with time zone,
    crawl_allow_patterns jsonb DEFAULT '[]'::jsonb NOT NULL,
    crawl_block_patterns jsonb DEFAULT '[]'::jsonb NOT NULL,
    materialization_allow_patterns jsonb DEFAULT '[]'::jsonb NOT NULL,
    materialization_block_patterns jsonb DEFAULT '[]'::jsonb NOT NULL,
    CONSTRAINT content_web_ingest_run_crawl_allow_patterns_array_check CHECK ((jsonb_typeof(crawl_allow_patterns) = 'array'::text)),
    CONSTRAINT content_web_ingest_run_crawl_block_patterns_array_check CHECK ((jsonb_typeof(crawl_block_patterns) = 'array'::text)),
    CONSTRAINT content_web_ingest_run_materialization_allow_patterns_array_che CHECK ((jsonb_typeof(materialization_allow_patterns) = 'array'::text)),
    CONSTRAINT content_web_ingest_run_materialization_block_patterns_array_che CHECK ((jsonb_typeof(materialization_block_patterns) = 'array'::text)),
    CONSTRAINT content_web_ingest_run_max_depth_check CHECK ((max_depth >= 0)),
    CONSTRAINT content_web_ingest_run_max_pages_check CHECK ((max_pages >= 1))
);


--
-- Name: entity_dedup_verification_cache; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.entity_dedup_verification_cache (
    cache_key bytea NOT NULL,
    library_id uuid NOT NULL,
    entity_a_key text NOT NULL,
    entity_b_key text NOT NULL,
    verdict boolean NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: extract_chunk_result; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.extract_chunk_result (
    id uuid DEFAULT uuidv7() NOT NULL,
    chunk_id uuid NOT NULL,
    attempt_id uuid NOT NULL,
    extract_state public.extract_state NOT NULL,
    provider_call_id uuid,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    finished_at timestamp with time zone,
    failure_code text
);


--
-- Name: extract_edge_candidate; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.extract_edge_candidate (
    id uuid DEFAULT uuidv7() NOT NULL,
    chunk_result_id uuid NOT NULL,
    canonical_key text NOT NULL,
    edge_kind text NOT NULL,
    from_canonical_key text NOT NULL,
    to_canonical_key text NOT NULL,
    summary text
);


--
-- Name: extract_node_candidate; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.extract_node_candidate (
    id uuid DEFAULT uuidv7() NOT NULL,
    chunk_result_id uuid NOT NULL,
    canonical_key text NOT NULL,
    node_kind text NOT NULL,
    display_label text NOT NULL,
    summary text
);


--
-- Name: extract_resume_cursor; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.extract_resume_cursor (
    attempt_id uuid NOT NULL,
    last_completed_chunk_index integer DEFAULT '-1'::integer NOT NULL,
    replay_count integer DEFAULT 0 NOT NULL,
    downgrade_level integer DEFAULT 0 NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: iam_api_token; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_api_token (
    principal_id uuid NOT NULL,
    workspace_id uuid,
    label text NOT NULL,
    token_prefix text NOT NULL,
    status public.iam_api_token_status DEFAULT 'active'::public.iam_api_token_status NOT NULL,
    expires_at timestamp with time zone,
    revoked_at timestamp with time zone,
    issued_by_principal_id uuid,
    last_used_at timestamp with time zone
);


--
-- Name: iam_api_token_secret; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_api_token_secret (
    token_principal_id uuid NOT NULL,
    secret_version integer NOT NULL,
    secret_hash text NOT NULL,
    issued_at timestamp with time zone DEFAULT now() NOT NULL,
    revoked_at timestamp with time zone
);


--
-- Name: iam_grant; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_grant (
    id uuid DEFAULT uuidv7() NOT NULL,
    principal_id uuid NOT NULL,
    resource_kind public.iam_grant_resource_kind NOT NULL,
    resource_id uuid NOT NULL,
    permission_kind public.iam_permission_kind NOT NULL,
    granted_by_principal_id uuid,
    granted_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone
);


--
-- Name: iam_principal; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_principal (
    id uuid DEFAULT uuidv7() NOT NULL,
    principal_kind public.iam_principal_kind NOT NULL,
    display_label text NOT NULL,
    status public.iam_principal_status DEFAULT 'active'::public.iam_principal_status NOT NULL,
    parent_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    disabled_at timestamp with time zone
);


--
-- Name: iam_service_identity; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_service_identity (
    principal_id uuid NOT NULL,
    owner_auth_provider_kind text NOT NULL,
    owner_external_subject text NOT NULL,
    created_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    deleted_at timestamp with time zone,
    provisioning_authority text,
    CONSTRAINT iam_service_identity_oidc_owner_check CHECK (((owner_auth_provider_kind ~~ 'oidc:%'::text) AND (btrim(owner_external_subject) <> ''::text))),
    CONSTRAINT iam_service_identity_provisioning_authority_check CHECK (((provisioning_authority IS NULL) OR (provisioning_authority = 'integration'::text)))
);


--
-- Name: iam_service_identity_operation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_service_identity_operation (
    id uuid DEFAULT uuidv7() NOT NULL,
    service_identity_principal_id uuid CONSTRAINT iam_service_identity_operat_service_identity_principal_not_null NOT NULL,
    owner_auth_provider_kind text CONSTRAINT iam_service_identity_operatio_owner_auth_provider_kind_not_null NOT NULL,
    owner_external_subject text NOT NULL,
    actor_principal_id uuid,
    operation_kind text NOT NULL,
    idempotency_key text NOT NULL,
    request_fingerprint text NOT NULL,
    credential_secret_version integer,
    encrypted_credential text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    superseded_at timestamp with time zone,
    CONSTRAINT iam_service_identity_operation_credential_check CHECK ((((operation_kind = 'revoke'::text) AND (credential_secret_version IS NULL) AND (encrypted_credential IS NULL) AND (superseded_at IS NULL)) OR ((operation_kind = ANY (ARRAY['create'::text, 'rotate'::text])) AND (credential_secret_version IS NOT NULL) AND (((encrypted_credential IS NOT NULL) AND (superseded_at IS NULL)) OR ((encrypted_credential IS NULL) AND (superseded_at IS NOT NULL)))))),
    CONSTRAINT iam_service_identity_operation_fingerprint_check CHECK ((request_fingerprint ~ '^[0-9a-f]{64}$'::text)),
    CONSTRAINT iam_service_identity_operation_idempotency_key_check CHECK (((char_length(idempotency_key) >= 1) AND (char_length(idempotency_key) <= 128))),
    CONSTRAINT iam_service_identity_operation_kind_check CHECK ((operation_kind = ANY (ARRAY['create'::text, 'rotate'::text, 'revoke'::text])))
);


--
-- Name: iam_session; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_session (
    id uuid DEFAULT uuidv7() NOT NULL,
    principal_id uuid NOT NULL,
    session_secret_hash text NOT NULL,
    issued_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: iam_user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_user (
    principal_id uuid NOT NULL,
    login text NOT NULL,
    email text NOT NULL,
    display_name text NOT NULL,
    password_hash text NOT NULL,
    auth_provider_kind text DEFAULT 'password'::text NOT NULL,
    external_subject text,
    role public.iam_system_role DEFAULT 'viewer'::public.iam_system_role NOT NULL
);


--
-- Name: iam_workspace_membership; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.iam_workspace_membership (
    workspace_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    membership_state public.iam_membership_state NOT NULL,
    joined_at timestamp with time zone DEFAULT now() NOT NULL,
    ended_at timestamp with time zone
);


--
-- Name: ingest_attempt; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ingest_attempt (
    id uuid DEFAULT uuidv7() NOT NULL,
    job_id uuid NOT NULL,
    attempt_number integer NOT NULL,
    worker_principal_id uuid,
    lease_token text,
    knowledge_generation_id uuid,
    attempt_state public.ingest_attempt_state NOT NULL,
    current_stage text,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    heartbeat_at timestamp with time zone,
    finished_at timestamp with time zone,
    failure_class text,
    failure_code text,
    retryable boolean DEFAULT false NOT NULL,
    failure_message text,
    progress_percent integer DEFAULT 0 NOT NULL,
    CONSTRAINT ingest_attempt_progress_percent_range CHECK (((progress_percent >= 0) AND (progress_percent <= 100)))
);


--
-- Name: ingest_job; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ingest_job (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    mutation_id uuid,
    connector_id uuid,
    async_operation_id uuid,
    knowledge_document_id uuid,
    knowledge_revision_id uuid,
    job_kind public.ingest_job_kind NOT NULL,
    queue_state public.ingest_queue_state DEFAULT 'queued'::public.ingest_queue_state NOT NULL,
    priority integer DEFAULT 100 NOT NULL,
    dedupe_key text,
    queued_at timestamp with time zone DEFAULT now() NOT NULL,
    available_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    queue_rank bigint DEFAULT ((EXTRACT(epoch FROM clock_timestamp()) * (1000000)::numeric))::bigint NOT NULL,
    queue_leased_at timestamp with time zone,
    queue_lease_token text,
    queue_lease_owner text,
    mutation_item_id uuid
);


--
-- Name: COLUMN ingest_job.mutation_item_id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.ingest_job.mutation_item_id IS 'Explicit content-mutation item owning this queue job. Nullable during expand/backfill rollout.';


--
-- Name: ingest_stage_event; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ingest_stage_event (
    id uuid DEFAULT uuidv7() NOT NULL,
    attempt_id uuid NOT NULL,
    stage_name text NOT NULL,
    stage_state public.ingest_stage_state NOT NULL,
    ordinal integer NOT NULL,
    message text,
    details_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    provider_kind text,
    model_name text,
    prompt_tokens integer,
    completion_tokens integer,
    total_tokens integer,
    cached_tokens integer,
    estimated_cost numeric(18,8),
    currency_code text,
    elapsed_ms bigint,
    started_at timestamp with time zone,
    recorded_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: ingest_stage_provider_call; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ingest_stage_provider_call (
    id uuid DEFAULT uuidv7() NOT NULL,
    stage_event_id uuid NOT NULL,
    call_sequence integer NOT NULL,
    provider_kind text NOT NULL,
    model_name text NOT NULL,
    prompt_tokens integer,
    completion_tokens integer,
    total_tokens integer,
    cached_tokens integer,
    estimated_cost numeric(18,8),
    currency_code text,
    elapsed_ms bigint,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone
);


--
-- Name: knowledge_bundle_chunk; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_bundle_chunk (
    bundle_id uuid NOT NULL,
    chunk_id uuid NOT NULL,
    library_id uuid NOT NULL,
    rank integer NOT NULL,
    score double precision NOT NULL,
    inclusion_reason text,
    created_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_bundle_chunk; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_bundle_chunk IS 'Replaces Arango edge collection knowledge_bundle_chunk_edge; a context-bundle child table, not a graph traversal edge.';


--
-- Name: knowledge_bundle_entity; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_bundle_entity (
    bundle_id uuid NOT NULL,
    entity_id uuid NOT NULL,
    library_id uuid NOT NULL,
    rank integer NOT NULL,
    score double precision NOT NULL,
    inclusion_reason text,
    created_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_bundle_entity; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_bundle_entity IS 'Replaces Arango edge collection knowledge_bundle_entity_edge; a context-bundle child table, not a graph traversal edge.';


--
-- Name: knowledge_bundle_evidence; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_bundle_evidence (
    bundle_id uuid NOT NULL,
    evidence_id uuid NOT NULL,
    library_id uuid NOT NULL,
    rank integer NOT NULL,
    score double precision NOT NULL,
    inclusion_reason text,
    created_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_bundle_evidence; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_bundle_evidence IS 'Replaces Arango edge collection knowledge_bundle_evidence_edge; a context-bundle child table, not a graph traversal edge.';


--
-- Name: knowledge_bundle_relation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_bundle_relation (
    bundle_id uuid NOT NULL,
    relation_id uuid NOT NULL,
    library_id uuid NOT NULL,
    rank integer NOT NULL,
    score double precision NOT NULL,
    inclusion_reason text,
    created_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_bundle_relation; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_bundle_relation IS 'Replaces Arango edge collection knowledge_bundle_relation_edge; a context-bundle child table, not a graph traversal edge.';


--
-- Name: knowledge_chunk; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_chunk (
    chunk_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    primary_block_id uuid,
    chunk_index integer NOT NULL,
    chunk_kind text,
    content_text text NOT NULL,
    normalized_text text NOT NULL,
    span_start integer,
    span_end integer,
    token_count integer,
    support_block_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    section_path text[] DEFAULT '{}'::text[] NOT NULL,
    heading_trail text[] DEFAULT '{}'::text[] NOT NULL,
    literal_digest text,
    chunk_state text NOT NULL,
    text_generation bigint,
    vector_generation bigint,
    quality_score real,
    window_text text,
    occurred_at timestamp with time zone,
    occurred_until timestamp with time zone,
    search_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, public.ironrag_unaccent(((((((((COALESCE(content_text, ''::text) || ' '::text) || COALESCE(normalized_text, ''::text)) || ' '::text) || COALESCE(window_text, ''::text)) || ' '::text) || COALESCE(public.ironrag_array_join(section_path), ''::text)) || ' '::text) || COALESCE(public.ironrag_array_join(heading_trail), ''::text))))) STORED
);


--
-- Name: TABLE knowledge_chunk; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_chunk IS 'Replaces Arango collection knowledge_chunk and collapses knowledge_revision_chunk_edge into revision_id. primary_block_id/support_block_ids replace knowledge_block_chunk_edge without a separate edge table.';


--
-- Name: knowledge_chunk_entity_mention; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_chunk_entity_mention (
    from_id uuid NOT NULL,
    to_id uuid NOT NULL,
    relation_type text DEFAULT 'mentions'::text NOT NULL,
    support bigint DEFAULT 1 NOT NULL,
    library_id uuid NOT NULL,
    rank integer,
    score double precision,
    inclusion_reason text,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_chunk_entity_mention; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_chunk_entity_mention IS 'Replaces Arango edge collection knowledge_chunk_mentions_entity_edge; retained as an indexed many-to-many edge because chunks may mention many entities and entities may be mentioned by many chunks.';


--
-- Name: knowledge_context_bundle; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_context_bundle (
    bundle_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    query_execution_id uuid,
    bundle_state text NOT NULL,
    bundle_strategy text NOT NULL,
    requested_mode text NOT NULL,
    resolved_mode text NOT NULL,
    selected_fact_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    verification_state text NOT NULL,
    verification_warnings jsonb DEFAULT '[]'::jsonb NOT NULL,
    freshness_snapshot jsonb DEFAULT '{}'::jsonb NOT NULL,
    candidate_summary jsonb DEFAULT '{}'::jsonb NOT NULL,
    assembly_diagnostics jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_context_bundle; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_context_bundle IS 'Replaces Arango collection knowledge_context_bundle; bundle edges become four ranked child tables with ON DELETE CASCADE.';


--
-- Name: knowledge_document; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_document (
    document_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    external_key text NOT NULL,
    file_name text,
    title text,
    document_state text NOT NULL,
    active_revision_id uuid,
    readable_revision_id uuid,
    latest_revision_no bigint,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    deleted_at timestamp with time zone,
    parent_document_id uuid,
    document_role text DEFAULT 'primary'::text NOT NULL
);


--
-- Name: TABLE knowledge_document; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_document IS 'Replaces Arango document collection knowledge_document; active/readable revision pointers replace document_revision edge lookups.';


--
-- Name: knowledge_entity; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_entity (
    entity_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    canonical_label text NOT NULL,
    aliases text[] DEFAULT '{}'::text[] NOT NULL,
    entity_type text NOT NULL,
    entity_sub_type text,
    summary text,
    confidence double precision,
    support_count bigint NOT NULL,
    freshness_generation bigint NOT NULL,
    entity_state text NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    search_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, public.ironrag_unaccent(((((COALESCE(canonical_label, ''::text) || ' '::text) || COALESCE(summary, ''::text)) || ' '::text) || COALESCE(public.ironrag_array_join(aliases), ''::text))))) STORED
);


--
-- Name: TABLE knowledge_entity; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_entity IS 'Replaces Arango collection knowledge_entity for the materialized knowledge-plane entity rows. Canonical query graph remains runtime_graph_node.';


--
-- Name: knowledge_entity_candidate; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_entity_candidate (
    candidate_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    chunk_id uuid,
    candidate_label text NOT NULL,
    candidate_type text NOT NULL,
    candidate_sub_type text,
    normalization_key text NOT NULL,
    confidence double precision,
    extraction_method text NOT NULL,
    candidate_state text NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_entity_candidate; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_entity_candidate IS 'Replaces Arango collection knowledge_entity_candidate; candidate staging rows are revision/chunk scoped.';


--
-- Name: knowledge_evidence; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_evidence (
    evidence_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    chunk_id uuid,
    block_id uuid,
    fact_id uuid,
    span_start integer,
    span_end integer,
    quote_text text DEFAULT ''::text NOT NULL,
    literal_spans_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    summary text,
    evidence_kind text DEFAULT ''::text NOT NULL,
    extraction_method text NOT NULL,
    confidence double precision,
    evidence_state text NOT NULL,
    freshness_generation bigint NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    search_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, public.ironrag_unaccent(((COALESCE(quote_text, ''::text) || ' '::text) || COALESCE(summary, ''::text))))) STORED
);


--
-- Name: TABLE knowledge_evidence; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_evidence IS 'Replaces Arango collection knowledge_evidence and collapses evidence_source_edge/fact_evidence_edge into document_id/revision_id/chunk_id/block_id/fact_id. TODO W1 review: bootstrap declared evidence_source_edge as EVIDENCE->CHUNK but writes used EVIDENCE->REVISION; this table keeps both revision_id and optional chunk_id.';


--
-- Name: knowledge_evidence_entity_support; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_evidence_entity_support (
    from_id uuid NOT NULL,
    to_id uuid NOT NULL,
    relation_type text DEFAULT 'supports_entity'::text NOT NULL,
    support bigint DEFAULT 1 NOT NULL,
    library_id uuid NOT NULL,
    rank integer,
    score double precision,
    inclusion_reason text,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_evidence_entity_support; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_evidence_entity_support IS 'Replaces Arango edge collection knowledge_evidence_supports_entity_edge; retained as an indexed many-to-many provenance edge with rank/score.';


--
-- Name: knowledge_evidence_relation_support; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_evidence_relation_support (
    from_id uuid NOT NULL,
    to_id uuid NOT NULL,
    relation_type text DEFAULT 'supports_relation'::text NOT NULL,
    support bigint DEFAULT 1 NOT NULL,
    library_id uuid NOT NULL,
    rank integer,
    score double precision,
    inclusion_reason text,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_evidence_relation_support; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_evidence_relation_support IS 'Replaces Arango edge collection knowledge_evidence_supports_relation_edge; retained as an indexed many-to-many provenance edge with rank/score.';


--
-- Name: knowledge_relation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_relation (
    relation_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    subject_entity_id uuid,
    object_entity_id uuid,
    predicate text NOT NULL,
    normalized_assertion text NOT NULL,
    summary text,
    confidence double precision,
    support_count bigint NOT NULL,
    contradiction_state text NOT NULL,
    freshness_generation bigint NOT NULL,
    relation_state text NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    search_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, public.ironrag_unaccent(((((COALESCE(predicate, ''::text) || ' '::text) || COALESCE(normalized_assertion, ''::text)) || ' '::text) || COALESCE(summary, ''::text))))) STORED
);


--
-- Name: TABLE knowledge_relation; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_relation IS 'Replaces Arango collection knowledge_relation and collapses relation_subject/object edges into subject_entity_id/object_entity_id. Canonical query graph remains runtime_graph_edge.';


--
-- Name: knowledge_relation_candidate; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_relation_candidate (
    candidate_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    chunk_id uuid,
    subject_label text DEFAULT ''::text NOT NULL,
    subject_candidate_key text NOT NULL,
    predicate text NOT NULL,
    object_label text DEFAULT ''::text NOT NULL,
    object_candidate_key text NOT NULL,
    normalized_assertion text NOT NULL,
    confidence double precision,
    extraction_method text NOT NULL,
    candidate_state text NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_relation_candidate; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_relation_candidate IS 'Replaces Arango collection knowledge_relation_candidate; candidate staging rows are revision/chunk scoped.';


--
-- Name: knowledge_retrieval_trace; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_retrieval_trace (
    trace_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    query_execution_id uuid,
    bundle_id uuid NOT NULL,
    trace_state text NOT NULL,
    retrieval_strategy text NOT NULL,
    candidate_counts jsonb DEFAULT '{}'::jsonb NOT NULL,
    dropped_reasons jsonb DEFAULT '{}'::jsonb NOT NULL,
    timing_breakdown jsonb DEFAULT '{}'::jsonb NOT NULL,
    diagnostics_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_retrieval_trace; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_retrieval_trace IS 'Replaces Arango collection knowledge_retrieval_trace; stores retrieval diagnostics and bundle snapshots.';


--
-- Name: knowledge_structured_block; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_structured_block (
    block_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    ordinal integer NOT NULL,
    block_kind text NOT NULL,
    text text NOT NULL,
    normalized_text text NOT NULL,
    heading_trail text[] DEFAULT '{}'::text[] NOT NULL,
    section_path text[] DEFAULT '{}'::text[] NOT NULL,
    page_number integer,
    span_start integer,
    span_end integer,
    parent_block_id uuid,
    table_coordinates_json jsonb,
    code_language text,
    occurred_at timestamp with time zone,
    occurred_until timestamp with time zone,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    search_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, public.ironrag_unaccent(((((((COALESCE(text, ''::text) || ' '::text) || COALESCE(normalized_text, ''::text)) || ' '::text) || COALESCE(public.ironrag_array_join(heading_trail), ''::text)) || ' '::text) || COALESCE(public.ironrag_array_join(section_path), ''::text))))) STORED
);


--
-- Name: TABLE knowledge_structured_block; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_structured_block IS 'Replaces Arango collection knowledge_structured_block and collapses knowledge_revision_block_edge into revision_id. occurred_at/occurred_until replace REGEX_EXTRACT temporal parsing from normalized_text.';


--
-- Name: knowledge_structured_revision; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_structured_revision (
    revision_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    preparation_state text NOT NULL,
    normalization_profile text NOT NULL,
    source_format text NOT NULL,
    language_code text,
    block_count bigint NOT NULL,
    chunk_count bigint NOT NULL,
    typed_fact_count bigint NOT NULL,
    outline_json jsonb NOT NULL,
    prepared_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL
);


--
-- Name: TABLE knowledge_structured_revision; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_structured_revision IS 'Replaces Arango collection knowledge_structured_revision; revision/document relationship is a direct FK.';


--
-- Name: knowledge_technical_fact; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_technical_fact (
    fact_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    fact_kind text NOT NULL,
    canonical_value_text text NOT NULL,
    canonical_value_exact text NOT NULL,
    canonical_value_json jsonb NOT NULL,
    display_value text NOT NULL,
    qualifiers_json jsonb NOT NULL,
    support_block_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    support_chunk_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    confidence double precision,
    extraction_kind text NOT NULL,
    conflict_group_id text,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    search_tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, public.ironrag_unaccent(((COALESCE(canonical_value_text, ''::text) || ' '::text) || COALESCE(display_value, ''::text))))) STORED
);


--
-- Name: TABLE knowledge_technical_fact; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_technical_fact IS 'Replaces Arango collection knowledge_technical_fact; fact_evidence is represented by knowledge_evidence.fact_id when evidence is fact-backed.';


--
-- Name: knowledge_vector_relation_manifest; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.knowledge_vector_relation_manifest (
    library_id uuid NOT NULL,
    dim integer NOT NULL,
    vector_kind text NOT NULL,
    embedding_model_key text NOT NULL,
    relation_name text NOT NULL,
    is_default boolean DEFAULT false NOT NULL,
    row_count bigint DEFAULT 0 NOT NULL,
    promoted boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: TABLE knowledge_vector_relation_manifest; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.knowledge_vector_relation_manifest IS 'Static W0 vector manifest. Per-dimension vector tables, LIST partitions, child relation routing, and HNSW/halfvec indexes are runtime DDL created by bootstrap/ingest/promotion because active dimensions are not known at migration time. Partition DDL must be batched <=250 partitions per transaction per the W0 ADR. Legacy Arango vector persistent indexes (knowledge_chunk_vector_* and knowledge_entity_vector_*) are runtime vector-table index contracts, not static baseline tables.';


--
-- Name: maintenance_job_run; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.maintenance_job_run (
    id uuid DEFAULT uuidv7() NOT NULL,
    class text NOT NULL,
    scope_kind text NOT NULL,
    scope_id uuid,
    owner_node text,
    state public.maintenance_run_state DEFAULT 'pending'::public.maintenance_run_state NOT NULL,
    cursor_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    last_started_at timestamp with time zone,
    heartbeat_at timestamp with time zone,
    last_completed_at timestamp with time zone,
    rows_removed_total bigint DEFAULT 0 NOT NULL,
    bytes_reclaimed_total bigint DEFAULT 0 NOT NULL,
    error_code text,
    error_text text,
    next_due_at timestamp with time zone DEFAULT now() NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT maintenance_job_run_check CHECK ((((scope_kind = 'instance'::text) AND (scope_id IS NULL)) OR ((scope_kind = 'library'::text) AND (scope_id IS NOT NULL)))),
    CONSTRAINT maintenance_job_run_scope_kind_check CHECK ((scope_kind = ANY (ARRAY['instance'::text, 'library'::text])))
);


--
-- Name: ops_async_operation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ops_async_operation (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid,
    operation_kind text NOT NULL,
    surface_kind public.surface_kind NOT NULL,
    requested_by_principal_id uuid,
    status public.ops_async_operation_status DEFAULT 'accepted'::public.ops_async_operation_status NOT NULL,
    subject_kind text NOT NULL,
    subject_id uuid,
    parent_async_operation_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    failure_code text,
    failure_disposition text,
    result_json jsonb
);


--
-- Name: ops_library_state; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ops_library_state (
    library_id uuid NOT NULL,
    queue_depth integer DEFAULT 0 NOT NULL,
    running_attempts integer DEFAULT 0 NOT NULL,
    readable_document_count integer DEFAULT 0 NOT NULL,
    failed_document_count integer DEFAULT 0 NOT NULL,
    degraded_state public.ops_degraded_state DEFAULT 'healthy'::public.ops_degraded_state NOT NULL,
    last_recomputed_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: ops_library_warning; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ops_library_warning (
    id uuid DEFAULT uuidv7() NOT NULL,
    library_id uuid NOT NULL,
    warning_kind text NOT NULL,
    severity public.ops_warning_severity NOT NULL,
    message text NOT NULL,
    source_operation_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    resolved_at timestamp with time zone
);


--
-- Name: query_chunk_reference; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_chunk_reference (
    execution_id uuid NOT NULL,
    chunk_id uuid NOT NULL,
    rank integer NOT NULL,
    score double precision NOT NULL
);


--
-- Name: query_conversation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_conversation (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    created_by_principal_id uuid,
    title text,
    conversation_state public.query_conversation_state DEFAULT 'active'::public.query_conversation_state NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    request_surface public.surface_kind DEFAULT 'ui'::public.surface_kind NOT NULL
);


--
-- Name: query_execution; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_execution (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    context_bundle_id uuid NOT NULL,
    request_turn_id uuid,
    response_turn_id uuid,
    binding_id uuid,
    query_text text NOT NULL,
    failure_code text,
    runtime_execution_id uuid NOT NULL,
    started_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: query_execution_replay; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_execution_replay (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    conversation_id uuid NOT NULL,
    request_turn_id uuid NOT NULL,
    response_turn_id uuid NOT NULL,
    source_execution_id uuid NOT NULL,
    cache_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: query_ir_cache; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_ir_cache (
    library_id uuid NOT NULL,
    question_hash text NOT NULL,
    schema_version smallint DEFAULT 1 NOT NULL,
    query_ir_json jsonb NOT NULL,
    provider_kind text,
    model_name text,
    usage_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    compiled_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: query_llm_context_snapshot; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_llm_context_snapshot (
    execution_id uuid NOT NULL,
    snapshot_json jsonb NOT NULL,
    captured_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: query_result_cache; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_result_cache (
    cache_key text NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    source_execution_id uuid NOT NULL,
    readable_content_fingerprint text NOT NULL,
    graph_projection_version bigint NOT NULL,
    graph_topology_generation bigint NOT NULL,
    binding_fingerprint text NOT NULL,
    hit_count integer DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: query_turn; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.query_turn (
    id uuid DEFAULT uuidv7() NOT NULL,
    conversation_id uuid NOT NULL,
    turn_index integer NOT NULL,
    turn_kind public.query_turn_kind NOT NULL,
    author_principal_id uuid,
    content_text text NOT NULL,
    execution_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_action_record; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_action_record (
    id uuid DEFAULT uuidv7() NOT NULL,
    runtime_execution_id uuid NOT NULL,
    stage_record_id uuid NOT NULL,
    action_kind public.runtime_action_kind NOT NULL,
    action_ordinal integer NOT NULL,
    action_state public.runtime_action_state NOT NULL,
    provider_binding_id uuid,
    tool_name text,
    usage_json jsonb,
    summary_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_execution; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_execution (
    id uuid DEFAULT uuidv7() NOT NULL,
    owner_kind public.runtime_execution_owner_kind NOT NULL,
    owner_id uuid NOT NULL,
    task_kind public.runtime_task_kind NOT NULL,
    surface_kind public.surface_kind NOT NULL,
    contract_name text NOT NULL,
    contract_version text NOT NULL,
    lifecycle_state public.runtime_lifecycle_state NOT NULL,
    active_stage public.runtime_stage_kind,
    turn_budget integer NOT NULL,
    turn_count integer DEFAULT 0 NOT NULL,
    parallel_action_limit integer NOT NULL,
    failure_code text,
    failure_summary_redacted text,
    accepted_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    parent_execution_id uuid,
    CONSTRAINT chk_runtime_execution_lifecycle_completion CHECK ((((lifecycle_state = ANY (ARRAY['accepted'::public.runtime_lifecycle_state, 'running'::public.runtime_lifecycle_state])) AND (completed_at IS NULL)) OR ((lifecycle_state = ANY (ARRAY['completed'::public.runtime_lifecycle_state, 'recovered'::public.runtime_lifecycle_state, 'failed'::public.runtime_lifecycle_state, 'canceled'::public.runtime_lifecycle_state])) AND (completed_at IS NOT NULL))))
);


--
-- Name: runtime_graph_canonical_summary; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_canonical_summary (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    target_kind text NOT NULL,
    target_id uuid NOT NULL,
    summary_text text NOT NULL,
    confidence_status text NOT NULL,
    support_count integer DEFAULT 0 NOT NULL,
    source_truth_version bigint NOT NULL,
    generated_from_mutation_id uuid,
    warning_text text,
    generated_at timestamp with time zone DEFAULT now() NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_graph_community; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_community (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    library_id uuid NOT NULL,
    projection_version bigint NOT NULL,
    member_node_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    summary_title text,
    summary_text text
);


--
-- Name: runtime_graph_edge; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_edge (
    id uuid DEFAULT uuidv7() NOT NULL,
    library_id uuid NOT NULL,
    from_node_id uuid NOT NULL,
    to_node_id uuid NOT NULL,
    relation_type text NOT NULL,
    canonical_key text NOT NULL,
    summary text,
    weight double precision,
    support_count integer DEFAULT 0 NOT NULL,
    metadata_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    rank integer,
    edge_score double precision,
    inclusion_reason text,
    support_edge_score double precision,
    projection_version bigint NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: COLUMN runtime_graph_edge.rank; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_edge.rank IS 'W6 ingest-merge must populate this provenance rank for PostgreSQL context traversal parity with Arango graph edges.';


--
-- Name: COLUMN runtime_graph_edge.edge_score; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_edge.edge_score IS 'W6 ingest-merge must populate this score for context.rs edge_score answer-path reads.';


--
-- Name: COLUMN runtime_graph_edge.inclusion_reason; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_edge.inclusion_reason IS 'W6 ingest-merge must populate this inclusion reason for context.rs inclusion_reason answer-path reads.';


--
-- Name: COLUMN runtime_graph_edge.support_edge_score; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_edge.support_edge_score IS 'W6 ingest-merge must populate this support score for context.rs support_edge_score answer-path reads.';


--
-- Name: runtime_graph_evidence; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_evidence (
    id uuid DEFAULT uuidv7() NOT NULL,
    library_id uuid NOT NULL,
    evidence_identity_key text NOT NULL,
    target_kind text NOT NULL,
    target_id uuid NOT NULL,
    document_id uuid,
    revision_id uuid,
    activated_by_attempt_id uuid,
    chunk_id uuid,
    source_file_name text,
    page_ref text,
    evidence_text text NOT NULL,
    confidence_score double precision,
    rank integer,
    score double precision,
    inclusion_reason text,
    support_edge_score double precision,
    support_edge_inclusion_reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: COLUMN runtime_graph_evidence.rank; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_evidence.rank IS 'W6 ingest-merge must populate this provenance rank for relation evidence lookup parity.';


--
-- Name: COLUMN runtime_graph_evidence.score; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_evidence.score IS 'W6 ingest-merge must populate this score for relation evidence lookup parity.';


--
-- Name: COLUMN runtime_graph_evidence.inclusion_reason; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_evidence.inclusion_reason IS 'W6 ingest-merge must populate this inclusion reason for context bundle evidence references.';


--
-- Name: COLUMN runtime_graph_evidence.support_edge_score; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_evidence.support_edge_score IS 'W6 ingest-merge must populate this support score for context.rs support_edge_score answer-path reads.';


--
-- Name: COLUMN runtime_graph_evidence.support_edge_inclusion_reason; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.runtime_graph_evidence.support_edge_inclusion_reason IS 'W6 ingest-merge must populate this support inclusion reason for relation evidence lookup parity.';


--
-- Name: runtime_graph_extraction; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_extraction (
    id uuid DEFAULT uuidv7() NOT NULL,
    runtime_execution_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    chunk_id uuid NOT NULL,
    provider_kind text NOT NULL,
    model_name text NOT NULL,
    extraction_version text NOT NULL,
    prompt_hash text NOT NULL,
    status text NOT NULL,
    raw_output_json jsonb NOT NULL,
    normalized_output_json jsonb NOT NULL,
    glean_pass_count integer DEFAULT 0 NOT NULL,
    error_message text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_graph_filtered_artifact; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_filtered_artifact (
    id uuid DEFAULT uuidv7() NOT NULL,
    library_id uuid NOT NULL,
    ingestion_run_id uuid,
    revision_id uuid,
    target_kind text NOT NULL,
    candidate_key text NOT NULL,
    source_node_key text,
    target_node_key text,
    relation_type text,
    filter_reason text NOT NULL,
    summary text,
    metadata_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_graph_node; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_node (
    id uuid DEFAULT uuidv7() NOT NULL,
    library_id uuid NOT NULL,
    canonical_key text NOT NULL,
    label text NOT NULL,
    node_type text NOT NULL,
    aliases_json jsonb DEFAULT '[]'::jsonb NOT NULL,
    summary text,
    metadata_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    support_count integer DEFAULT 0 NOT NULL,
    projection_version bigint NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    document_id uuid,
    CONSTRAINT runtime_graph_node_document_identity_ck CHECK ((((node_type = 'document'::text) AND (document_id IS NOT NULL)) OR ((node_type <> 'document'::text) AND (document_id IS NULL))))
);


--
-- Name: runtime_graph_snapshot; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_graph_snapshot (
    library_id uuid NOT NULL,
    graph_status text NOT NULL,
    projection_version bigint NOT NULL,
    node_count integer DEFAULT 0 NOT NULL,
    edge_count integer DEFAULT 0 NOT NULL,
    provenance_coverage_percent double precision,
    last_built_at timestamp with time zone,
    last_error_message text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    topology_generation bigint DEFAULT 0 NOT NULL,
    build_epoch uuid DEFAULT uuidv7() NOT NULL
);


--
-- Name: runtime_policy_decision; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_policy_decision (
    id uuid DEFAULT uuidv7() NOT NULL,
    runtime_execution_id uuid NOT NULL,
    stage_record_id uuid,
    action_record_id uuid,
    target_kind public.runtime_decision_target_kind NOT NULL,
    decision_kind public.runtime_decision_kind NOT NULL,
    reason_code text NOT NULL,
    reason_summary_redacted text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_provider_profile; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_provider_profile (
    library_id uuid NOT NULL,
    indexing_provider_kind text NOT NULL,
    indexing_model_name text NOT NULL,
    embedding_provider_kind text NOT NULL,
    embedding_model_name text NOT NULL,
    answer_provider_kind text NOT NULL,
    answer_model_name text NOT NULL,
    vision_provider_kind text NOT NULL,
    vision_model_name text NOT NULL,
    last_validated_at timestamp with time zone,
    last_validation_status text,
    last_validation_error text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_provider_validation_log; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_provider_validation_log (
    id uuid DEFAULT uuidv7() NOT NULL,
    library_id uuid,
    provider_kind text NOT NULL,
    model_name text NOT NULL,
    capability text NOT NULL,
    status text NOT NULL,
    error_message text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: runtime_stage_record; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.runtime_stage_record (
    id uuid DEFAULT uuidv7() NOT NULL,
    runtime_execution_id uuid NOT NULL,
    stage_kind public.runtime_stage_kind NOT NULL,
    stage_ordinal integer NOT NULL,
    attempt_no integer NOT NULL,
    stage_state public.runtime_stage_state NOT NULL,
    deterministic boolean DEFAULT false NOT NULL,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    input_summary_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    output_summary_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    failure_code text,
    failure_summary_redacted text
);


--
-- Name: webhook_delivery_attempt; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_delivery_attempt (
    id uuid DEFAULT uuidv7() NOT NULL,
    subscription_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid,
    event_type text NOT NULL,
    event_id text NOT NULL,
    payload_json jsonb NOT NULL,
    target_url text NOT NULL,
    attempt_number integer DEFAULT 0 NOT NULL,
    delivery_state public.webhook_delivery_state DEFAULT 'pending'::public.webhook_delivery_state NOT NULL,
    response_status integer,
    response_body_excerpt text,
    error_message text,
    job_id uuid,
    next_attempt_at timestamp with time zone,
    delivered_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    delivery_lease_token uuid,
    error_code text,
    occurred_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT webhook_delivery_attempt_error_code_format CHECK (((error_code IS NULL) OR (error_code ~ '^[a-z][a-z0-9_]{0,63}$'::text))),
    CONSTRAINT webhook_delivery_attempt_error_message_length CHECK (((error_message IS NULL) OR (char_length(error_message) <= 512)))
);


--
-- Name: webhook_subscription; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_subscription (
    id uuid DEFAULT uuidv7() NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid,
    display_name text NOT NULL,
    target_url text NOT NULL,
    secret text NOT NULL,
    event_types text[] DEFAULT '{}'::text[] NOT NULL,
    custom_headers_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    active boolean DEFAULT true NOT NULL,
    created_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    delete_requested_at timestamp with time zone,
    CONSTRAINT webhook_subscription_event_types_nonempty CHECK ((cardinality(event_types) > 0)),
    CONSTRAINT webhook_subscription_target_url_format CHECK (((target_url ~~ 'http://%'::text) OR (target_url ~~ 'https://%'::text)))
);


--
-- Name: webhook_delivery_attempt_tenant_integrity_audit; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.webhook_delivery_attempt_tenant_integrity_audit AS
 SELECT delivery.id AS delivery_attempt_id,
        CASE
            WHEN (subscription.id IS NULL) THEN 'subscription_missing'::text
            WHEN (subscription.workspace_id <> delivery.workspace_id) THEN 'subscription_workspace_mismatch'::text
            WHEN (library.id IS NULL) THEN 'library_scope_missing'::text
            WHEN ((subscription.library_id IS NOT NULL) AND (subscription.library_id <> delivery.library_id)) THEN 'subscription_library_mismatch'::text
            WHEN (delivery.job_id IS NULL) THEN 'job_missing'::text
            WHEN (job.id IS NULL) THEN 'job_scope_mismatch'::text
            WHEN (job.job_kind <> 'webhook_delivery'::public.ingest_job_kind) THEN 'job_kind_mismatch'::text
            WHEN (EXISTS ( SELECT 1
               FROM public.webhook_delivery_attempt other_delivery
              WHERE ((other_delivery.job_id = delivery.job_id) AND (other_delivery.id <> delivery.id)))) THEN 'duplicate_job_link'::text
            ELSE 'unknown_scope_mismatch'::text
        END AS issue_code
   FROM (((public.webhook_delivery_attempt delivery
     LEFT JOIN public.webhook_subscription subscription ON ((subscription.id = delivery.subscription_id)))
     LEFT JOIN public.catalog_library library ON (((library.id = delivery.library_id) AND (library.workspace_id = delivery.workspace_id))))
     LEFT JOIN public.ingest_job job ON (((job.id = delivery.job_id) AND (job.workspace_id = delivery.workspace_id) AND (job.library_id = delivery.library_id))))
  WHERE ((subscription.id IS NULL) OR (subscription.workspace_id <> delivery.workspace_id) OR (library.id IS NULL) OR ((subscription.library_id IS NOT NULL) AND (subscription.library_id <> delivery.library_id)) OR (delivery.job_id IS NULL) OR (job.id IS NULL) OR (job.job_kind <> 'webhook_delivery'::public.ingest_job_kind) OR (EXISTS ( SELECT 1
           FROM public.webhook_delivery_attempt other_delivery
          WHERE ((other_delivery.job_id = delivery.job_id) AND (other_delivery.id <> delivery.id)))));


--
-- Name: webhook_delivery_event_identity_audit; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.webhook_delivery_event_identity_audit AS
 SELECT subscription_id,
    count(*) AS duplicate_count,
    'duplicate_subscription_event'::text AS issue_code
   FROM public.webhook_delivery_attempt
  GROUP BY subscription_id, event_id
 HAVING (count(*) > 1);


--
-- Name: webhook_delivery_lease_shape_audit; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.webhook_delivery_lease_shape_audit AS
 SELECT id AS delivery_attempt_id,
    'delivery_lease_shape_mismatch'::text AS issue_code
   FROM public.webhook_delivery_attempt
  WHERE ((delivery_state = 'delivering'::public.webhook_delivery_state) <> (delivery_lease_token IS NOT NULL));


--
-- Name: webhook_lifecycle_outbox; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_lifecycle_outbox (
    id uuid DEFAULT uuidv7() NOT NULL,
    event_id text NOT NULL,
    event_type text NOT NULL,
    occurred_at timestamp with time zone NOT NULL,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    payload_json jsonb NOT NULL,
    dispatch_state text DEFAULT 'pending'::text NOT NULL,
    dispatch_attempts integer DEFAULT 0 NOT NULL,
    available_at timestamp with time zone DEFAULT now() NOT NULL,
    lease_owner text,
    lease_token uuid,
    leased_at timestamp with time zone,
    lease_expires_at timestamp with time zone,
    last_error_code text,
    last_error text,
    dispatched_at timestamp with time zone,
    resolution_reason_code text,
    resolved_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT webhook_lifecycle_outbox_attempts_nonnegative CHECK ((dispatch_attempts >= 0)),
    CONSTRAINT webhook_lifecycle_outbox_dead_letter_error_code CHECK (((dispatch_state <> 'dead_letter'::text) OR (last_error_code IS NOT NULL))),
    CONSTRAINT webhook_lifecycle_outbox_dispatch_state CHECK ((dispatch_state = ANY (ARRAY['pending'::text, 'dispatching'::text, 'dispatched'::text, 'dead_letter'::text, 'resolved'::text]))),
    CONSTRAINT webhook_lifecycle_outbox_dispatched_shape CHECK (((dispatch_state = 'dispatched'::text) = (dispatched_at IS NOT NULL))),
    CONSTRAINT webhook_lifecycle_outbox_event_id_length CHECK (((char_length(event_id) >= 1) AND (char_length(event_id) <= 512))),
    CONSTRAINT webhook_lifecycle_outbox_event_type CHECK ((event_type = ANY (ARRAY['revision.ready'::text, 'document.deleted'::text]))),
    CONSTRAINT webhook_lifecycle_outbox_last_error_code_format CHECK (((last_error_code IS NULL) OR (last_error_code ~ '^[a-z][a-z0-9_]{0,63}$'::text))),
    CONSTRAINT webhook_lifecycle_outbox_last_error_length CHECK (((last_error IS NULL) OR (char_length(last_error) <= 2000))),
    CONSTRAINT webhook_lifecycle_outbox_lease_owner_length CHECK (((lease_owner IS NULL) OR ((char_length(lease_owner) >= 1) AND (char_length(lease_owner) <= 255)))),
    CONSTRAINT webhook_lifecycle_outbox_lease_shape CHECK ((((dispatch_state = 'dispatching'::text) AND (lease_owner IS NOT NULL) AND (lease_token IS NOT NULL) AND (leased_at IS NOT NULL) AND (lease_expires_at IS NOT NULL)) OR ((dispatch_state <> 'dispatching'::text) AND (lease_owner IS NULL) AND (lease_token IS NULL) AND (leased_at IS NULL) AND (lease_expires_at IS NULL)))),
    CONSTRAINT webhook_lifecycle_outbox_payload_object CHECK ((jsonb_typeof(payload_json) = 'object'::text)),
    CONSTRAINT webhook_lifecycle_outbox_resolution_reason_code_format CHECK (((resolution_reason_code IS NULL) OR (resolution_reason_code ~ '^[a-z][a-z0-9_]{0,63}$'::text))),
    CONSTRAINT webhook_lifecycle_outbox_resolved_shape CHECK ((((dispatch_state = 'resolved'::text) AND (resolution_reason_code IS NOT NULL) AND (resolved_at IS NOT NULL)) OR ((dispatch_state <> 'resolved'::text) AND (resolution_reason_code IS NULL) AND (resolved_at IS NULL))))
);


--
-- Name: webhook_lifecycle_outbox_recipient; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_lifecycle_outbox_recipient (
    outbox_id uuid NOT NULL,
    subscription_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: webhook_subscription_contract_audit; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.webhook_subscription_contract_audit AS
 SELECT id AS subscription_id,
        CASE
            WHEN ((char_length(btrim(display_name)) < 1) OR (char_length(btrim(display_name)) > 128) OR ((octet_length(target_url) < 1) OR (octet_length(target_url) > 2048)) OR ((octet_length(secret) < 1) OR (octet_length(secret) > 8192)) OR (octet_length((custom_headers_json)::text) > 9000)) THEN 'bounded_field_violation'::text
            WHEN ((cardinality(event_types) < 1) OR (cardinality(event_types) > 2) OR (NOT (event_types <@ ARRAY['revision.ready'::text, 'document.deleted'::text])) OR (cardinality(event_types) <> (
            CASE
                WHEN ('revision.ready'::text = ANY (event_types)) THEN 1
                ELSE 0
            END +
            CASE
                WHEN ('document.deleted'::text = ANY (event_types)) THEN 1
                ELSE 0
            END))) THEN 'event_catalog_violation'::text
            WHEN (jsonb_typeof(custom_headers_json) <> ALL (ARRAY['object'::text, 'string'::text, 'null'::text])) THEN 'custom_headers_shape_violation'::text
            ELSE 'unknown_contract_violation'::text
        END AS issue_code
   FROM public.webhook_subscription
  WHERE ((char_length(btrim(display_name)) < 1) OR (char_length(btrim(display_name)) > 128) OR ((octet_length(target_url) < 1) OR (octet_length(target_url) > 2048)) OR ((octet_length(secret) < 1) OR (octet_length(secret) > 8192)) OR (octet_length((custom_headers_json)::text) > 9000) OR ((cardinality(event_types) < 1) OR (cardinality(event_types) > 2)) OR (NOT (event_types <@ ARRAY['revision.ready'::text, 'document.deleted'::text])) OR (cardinality(event_types) <> (
        CASE
            WHEN ('revision.ready'::text = ANY (event_types)) THEN 1
            ELSE 0
        END +
        CASE
            WHEN ('document.deleted'::text = ANY (event_types)) THEN 1
            ELSE 0
        END)) OR (jsonb_typeof(custom_headers_json) <> ALL (ARRAY['object'::text, 'string'::text, 'null'::text])));


--
-- Name: webhook_subscription_quota_audit; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.webhook_subscription_quota_audit AS
 SELECT workspace_id,
    count(*) AS active_subscription_count,
    'active_subscription_quota_exceeded'::text AS issue_code
   FROM public.webhook_subscription
  WHERE active
  GROUP BY workspace_id
 HAVING (count(*) > 100);


--
-- Name: webhook_subscription_tenant_integrity_audit; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.webhook_subscription_tenant_integrity_audit AS
 SELECT subscription.id AS subscription_id,
    'library_workspace_mismatch'::text AS issue_code
   FROM (public.webhook_subscription subscription
     LEFT JOIN public.catalog_library library ON (((library.id = subscription.library_id) AND (library.workspace_id = subscription.workspace_id))))
  WHERE ((subscription.library_id IS NOT NULL) AND (library.id IS NULL));


--
-- Name: webhook_subscription_total_quota_audit; Type: VIEW; Schema: public; Owner: -
--

CREATE VIEW public.webhook_subscription_total_quota_audit AS
 SELECT workspace_id,
    count(*) AS total_subscription_count,
    'total_subscription_quota_exceeded'::text AS issue_code
   FROM public.webhook_subscription
  GROUP BY workspace_id
 HAVING (count(*) > 1000);


--
-- Name: ai_account ai_account_id_provider_catalog_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_account
    ADD CONSTRAINT ai_account_id_provider_catalog_id_key UNIQUE (id, provider_catalog_id);


--
-- Name: ai_binding ai_binding_assignment_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding
    ADD CONSTRAINT ai_binding_assignment_pkey PRIMARY KEY (id);


--
-- Name: ai_binding_validation ai_binding_validation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding_validation
    ADD CONSTRAINT ai_binding_validation_pkey PRIMARY KEY (id);


--
-- Name: ai_model_binding_purpose ai_model_binding_purpose_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_model_binding_purpose
    ADD CONSTRAINT ai_model_binding_purpose_pkey PRIMARY KEY (model_catalog_id, binding_purpose);


--
-- Name: ai_model_catalog ai_model_catalog_id_provider_catalog_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_model_catalog
    ADD CONSTRAINT ai_model_catalog_id_provider_catalog_id_key UNIQUE (id, provider_catalog_id);


--
-- Name: ai_model_catalog ai_model_catalog_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_model_catalog
    ADD CONSTRAINT ai_model_catalog_pkey PRIMARY KEY (id);


--
-- Name: ai_model_catalog ai_model_catalog_provider_catalog_id_model_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_model_catalog
    ADD CONSTRAINT ai_model_catalog_provider_catalog_id_model_name_key UNIQUE (provider_catalog_id, model_name);


--
-- Name: ai_provider_catalog ai_provider_catalog_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_provider_catalog
    ADD CONSTRAINT ai_provider_catalog_pkey PRIMARY KEY (id);


--
-- Name: ai_provider_catalog ai_provider_catalog_provider_kind_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_provider_catalog
    ADD CONSTRAINT ai_provider_catalog_provider_kind_key UNIQUE (provider_kind);


--
-- Name: ai_account ai_provider_credential_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_account
    ADD CONSTRAINT ai_provider_credential_pkey PRIMARY KEY (id);


--
-- Name: audit_event audit_event_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_event
    ADD CONSTRAINT audit_event_pkey PRIMARY KEY (id);


--
-- Name: audit_event_subject audit_event_subject_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_event_subject
    ADD CONSTRAINT audit_event_subject_pkey PRIMARY KEY (audit_event_id, subject_kind, subject_id);


--
-- Name: billing_charge billing_charge_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_charge
    ADD CONSTRAINT billing_charge_pkey PRIMARY KEY (id);


--
-- Name: billing_execution_cost billing_execution_cost_owning_execution_kind_owning_executi_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_execution_cost
    ADD CONSTRAINT billing_execution_cost_owning_execution_kind_owning_executi_key UNIQUE (owning_execution_kind, owning_execution_id);


--
-- Name: billing_execution_cost billing_execution_cost_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_execution_cost
    ADD CONSTRAINT billing_execution_cost_pkey PRIMARY KEY (id);


--
-- Name: billing_execution_cost_rollup_state billing_execution_cost_rollup_state_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_execution_cost_rollup_state
    ADD CONSTRAINT billing_execution_cost_rollup_state_pkey PRIMARY KEY (owning_execution_kind, owning_execution_id);


--
-- Name: billing_provider_call billing_provider_call_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_provider_call
    ADD CONSTRAINT billing_provider_call_pkey PRIMARY KEY (id);


--
-- Name: billing_usage billing_usage_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_usage
    ADD CONSTRAINT billing_usage_pkey PRIMARY KEY (id);


--
-- Name: catalog_library_connector catalog_library_connector_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library_connector
    ADD CONSTRAINT catalog_library_connector_pkey PRIMARY KEY (id);


--
-- Name: catalog_library catalog_library_id_workspace_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library
    ADD CONSTRAINT catalog_library_id_workspace_id_key UNIQUE (id, workspace_id);


--
-- Name: catalog_library catalog_library_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library
    ADD CONSTRAINT catalog_library_pkey PRIMARY KEY (id);


--
-- Name: catalog_library catalog_library_workspace_id_slug_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library
    ADD CONSTRAINT catalog_library_workspace_id_slug_key UNIQUE (workspace_id, slug);


--
-- Name: catalog_workspace catalog_workspace_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_workspace
    ADD CONSTRAINT catalog_workspace_pkey PRIMARY KEY (id);


--
-- Name: catalog_workspace catalog_workspace_slug_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_workspace
    ADD CONSTRAINT catalog_workspace_slug_key UNIQUE (slug);


--
-- Name: content_chunk content_chunk_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_chunk
    ADD CONSTRAINT content_chunk_pkey PRIMARY KEY (id);


--
-- Name: content_chunk content_chunk_revision_id_chunk_index_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_chunk
    ADD CONSTRAINT content_chunk_revision_id_chunk_index_key UNIQUE (revision_id, chunk_index);


--
-- Name: content_document_head content_document_head_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document_head
    ADD CONSTRAINT content_document_head_pkey PRIMARY KEY (document_id);


--
-- Name: content_document content_document_id_workspace_id_library_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document
    ADD CONSTRAINT content_document_id_workspace_id_library_id_key UNIQUE (id, workspace_id, library_id);


--
-- Name: content_document content_document_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document
    ADD CONSTRAINT content_document_pkey PRIMARY KEY (id);


--
-- Name: content_mutation_item content_mutation_item_id_document_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_id_document_key UNIQUE (id, document_id);


--
-- Name: content_mutation_item content_mutation_item_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_pkey PRIMARY KEY (id);


--
-- Name: content_mutation_item content_mutation_item_published_lineage_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_published_lineage_key UNIQUE (id, document_id, result_revision_id);


--
-- Name: content_mutation content_mutation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation
    ADD CONSTRAINT content_mutation_pkey PRIMARY KEY (id);


--
-- Name: content_mutation_predecessor_audit content_mutation_predecessor_audit_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_predecessor_audit
    ADD CONSTRAINT content_mutation_predecessor_audit_pkey PRIMARY KEY (id);


--
-- Name: content_mutation_reference content_mutation_reference_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_reference
    ADD CONSTRAINT content_mutation_reference_pkey PRIMARY KEY (id);


--
-- Name: content_mutation_reference content_mutation_reference_scope_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_reference
    ADD CONSTRAINT content_mutation_reference_scope_key UNIQUE (id, workspace_id, library_id);


--
-- Name: content_revision content_revision_document_id_revision_number_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision
    ADD CONSTRAINT content_revision_document_id_revision_number_key UNIQUE (document_id, revision_number);


--
-- Name: content_revision content_revision_id_workspace_id_library_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision
    ADD CONSTRAINT content_revision_id_workspace_id_library_id_key UNIQUE (id, workspace_id, library_id);


--
-- Name: content_revision_ingest_unit content_revision_ingest_unit_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision_ingest_unit
    ADD CONSTRAINT content_revision_ingest_unit_pkey PRIMARY KEY (revision_id, stage_name, unit_ordinal);


--
-- Name: content_revision content_revision_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision
    ADD CONSTRAINT content_revision_pkey PRIMARY KEY (id);


--
-- Name: content_visibility_intent content_visibility_intent_document_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_visibility_intent
    ADD CONSTRAINT content_visibility_intent_document_key UNIQUE (mutation_item_id, document_id);


--
-- Name: content_visibility_intent content_visibility_intent_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_visibility_intent
    ADD CONSTRAINT content_visibility_intent_pkey PRIMARY KEY (mutation_item_id);


--
-- Name: content_web_discovered_page content_web_discovered_page_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_discovered_page
    ADD CONSTRAINT content_web_discovered_page_pkey PRIMARY KEY (id);


--
-- Name: content_web_ingest_run content_web_ingest_run_async_operation_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_async_operation_id_key UNIQUE (async_operation_id);


--
-- Name: content_web_ingest_run content_web_ingest_run_mutation_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_mutation_id_key UNIQUE (mutation_id);


--
-- Name: content_web_ingest_run content_web_ingest_run_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_pkey PRIMARY KEY (id);


--
-- Name: entity_dedup_verification_cache entity_dedup_verification_cache_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entity_dedup_verification_cache
    ADD CONSTRAINT entity_dedup_verification_cache_pkey PRIMARY KEY (cache_key);


--
-- Name: extract_chunk_result extract_chunk_result_chunk_id_attempt_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_chunk_result
    ADD CONSTRAINT extract_chunk_result_chunk_id_attempt_id_key UNIQUE (chunk_id, attempt_id);


--
-- Name: extract_chunk_result extract_chunk_result_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_chunk_result
    ADD CONSTRAINT extract_chunk_result_pkey PRIMARY KEY (id);


--
-- Name: extract_edge_candidate extract_edge_candidate_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_edge_candidate
    ADD CONSTRAINT extract_edge_candidate_pkey PRIMARY KEY (id);


--
-- Name: extract_node_candidate extract_node_candidate_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_node_candidate
    ADD CONSTRAINT extract_node_candidate_pkey PRIMARY KEY (id);


--
-- Name: extract_resume_cursor extract_resume_cursor_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_resume_cursor
    ADD CONSTRAINT extract_resume_cursor_pkey PRIMARY KEY (attempt_id);


--
-- Name: iam_api_token iam_api_token_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_api_token
    ADD CONSTRAINT iam_api_token_pkey PRIMARY KEY (principal_id);


--
-- Name: iam_api_token_secret iam_api_token_secret_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_api_token_secret
    ADD CONSTRAINT iam_api_token_secret_pkey PRIMARY KEY (token_principal_id, secret_version);


--
-- Name: iam_grant iam_grant_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_grant
    ADD CONSTRAINT iam_grant_pkey PRIMARY KEY (id);


--
-- Name: iam_grant iam_grant_principal_id_resource_kind_resource_id_permission_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_grant
    ADD CONSTRAINT iam_grant_principal_id_resource_kind_resource_id_permission_key UNIQUE (principal_id, resource_kind, resource_id, permission_kind);


--
-- Name: iam_principal iam_principal_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_principal
    ADD CONSTRAINT iam_principal_pkey PRIMARY KEY (id);


--
-- Name: iam_service_identity_operation iam_service_identity_operation_owner_idempotency_key_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_owner_idempotency_key_key UNIQUE (owner_auth_provider_kind, owner_external_subject, idempotency_key);


--
-- Name: iam_service_identity_operation iam_service_identity_operation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_pkey PRIMARY KEY (id);


--
-- Name: iam_service_identity iam_service_identity_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_pkey PRIMARY KEY (principal_id);


--
-- Name: iam_service_identity iam_service_identity_principal_id_owner_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_principal_id_owner_key UNIQUE (principal_id, owner_auth_provider_kind, owner_external_subject);


--
-- Name: iam_session iam_session_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_session
    ADD CONSTRAINT iam_session_pkey PRIMARY KEY (id);


--
-- Name: iam_user iam_user_email_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_user
    ADD CONSTRAINT iam_user_email_key UNIQUE (email);


--
-- Name: iam_user iam_user_login_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_user
    ADD CONSTRAINT iam_user_login_key UNIQUE (login);


--
-- Name: iam_user iam_user_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_user
    ADD CONSTRAINT iam_user_pkey PRIMARY KEY (principal_id);


--
-- Name: iam_workspace_membership iam_workspace_membership_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_workspace_membership
    ADD CONSTRAINT iam_workspace_membership_pkey PRIMARY KEY (workspace_id, principal_id);


--
-- Name: ingest_attempt ingest_attempt_job_id_attempt_number_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_attempt
    ADD CONSTRAINT ingest_attempt_job_id_attempt_number_key UNIQUE (job_id, attempt_number);


--
-- Name: ingest_attempt ingest_attempt_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_attempt
    ADD CONSTRAINT ingest_attempt_pkey PRIMARY KEY (id);


--
-- Name: ingest_job ingest_job_id_workspace_id_library_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_job
    ADD CONSTRAINT ingest_job_id_workspace_id_library_id_key UNIQUE (id, workspace_id, library_id);


--
-- Name: ingest_job ingest_job_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_job
    ADD CONSTRAINT ingest_job_pkey PRIMARY KEY (id);


--
-- Name: ingest_stage_event ingest_stage_event_attempt_id_ordinal_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_stage_event
    ADD CONSTRAINT ingest_stage_event_attempt_id_ordinal_key UNIQUE (attempt_id, ordinal);


--
-- Name: ingest_stage_event ingest_stage_event_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_stage_event
    ADD CONSTRAINT ingest_stage_event_pkey PRIMARY KEY (id);


--
-- Name: ingest_stage_provider_call ingest_stage_provider_call_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_stage_provider_call
    ADD CONSTRAINT ingest_stage_provider_call_pkey PRIMARY KEY (id);


--
-- Name: ingest_stage_provider_call ingest_stage_provider_call_stage_event_id_call_sequence_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_stage_provider_call
    ADD CONSTRAINT ingest_stage_provider_call_stage_event_id_call_sequence_key UNIQUE (stage_event_id, call_sequence);


--
-- Name: knowledge_bundle_chunk knowledge_bundle_chunk_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_chunk
    ADD CONSTRAINT knowledge_bundle_chunk_pkey PRIMARY KEY (bundle_id, chunk_id);


--
-- Name: knowledge_bundle_entity knowledge_bundle_entity_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_entity
    ADD CONSTRAINT knowledge_bundle_entity_pkey PRIMARY KEY (bundle_id, entity_id);


--
-- Name: knowledge_bundle_evidence knowledge_bundle_evidence_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_evidence
    ADD CONSTRAINT knowledge_bundle_evidence_pkey PRIMARY KEY (bundle_id, evidence_id);


--
-- Name: knowledge_bundle_relation knowledge_bundle_relation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_relation
    ADD CONSTRAINT knowledge_bundle_relation_pkey PRIMARY KEY (bundle_id, relation_id);


--
-- Name: knowledge_chunk_entity_mention knowledge_chunk_entity_mention_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_chunk_entity_mention
    ADD CONSTRAINT knowledge_chunk_entity_mention_pkey PRIMARY KEY (from_id, to_id, relation_type);


--
-- Name: knowledge_chunk knowledge_chunk_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_chunk
    ADD CONSTRAINT knowledge_chunk_pkey PRIMARY KEY (chunk_id);


--
-- Name: knowledge_context_bundle knowledge_context_bundle_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_context_bundle
    ADD CONSTRAINT knowledge_context_bundle_pkey PRIMARY KEY (bundle_id);


--
-- Name: knowledge_document knowledge_document_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_document
    ADD CONSTRAINT knowledge_document_pkey PRIMARY KEY (document_id);


--
-- Name: knowledge_entity_candidate knowledge_entity_candidate_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_entity_candidate
    ADD CONSTRAINT knowledge_entity_candidate_pkey PRIMARY KEY (candidate_id);


--
-- Name: knowledge_entity knowledge_entity_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_entity
    ADD CONSTRAINT knowledge_entity_pkey PRIMARY KEY (entity_id);


--
-- Name: knowledge_evidence_entity_support knowledge_evidence_entity_support_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence_entity_support
    ADD CONSTRAINT knowledge_evidence_entity_support_pkey PRIMARY KEY (from_id, to_id, relation_type);


--
-- Name: knowledge_evidence knowledge_evidence_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence
    ADD CONSTRAINT knowledge_evidence_pkey PRIMARY KEY (evidence_id);


--
-- Name: knowledge_evidence_relation_support knowledge_evidence_relation_support_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence_relation_support
    ADD CONSTRAINT knowledge_evidence_relation_support_pkey PRIMARY KEY (from_id, to_id, relation_type);


--
-- Name: knowledge_relation_candidate knowledge_relation_candidate_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_relation_candidate
    ADD CONSTRAINT knowledge_relation_candidate_pkey PRIMARY KEY (candidate_id);


--
-- Name: knowledge_relation knowledge_relation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_relation
    ADD CONSTRAINT knowledge_relation_pkey PRIMARY KEY (relation_id);


--
-- Name: knowledge_retrieval_trace knowledge_retrieval_trace_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_retrieval_trace
    ADD CONSTRAINT knowledge_retrieval_trace_pkey PRIMARY KEY (trace_id);


--
-- Name: knowledge_revision knowledge_revision_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_revision
    ADD CONSTRAINT knowledge_revision_pkey PRIMARY KEY (revision_id);


--
-- Name: knowledge_structured_block knowledge_structured_block_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_structured_block
    ADD CONSTRAINT knowledge_structured_block_pkey PRIMARY KEY (block_id);


--
-- Name: knowledge_structured_revision knowledge_structured_revision_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_structured_revision
    ADD CONSTRAINT knowledge_structured_revision_pkey PRIMARY KEY (revision_id);


--
-- Name: knowledge_technical_fact knowledge_technical_fact_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_technical_fact
    ADD CONSTRAINT knowledge_technical_fact_pkey PRIMARY KEY (fact_id);


--
-- Name: knowledge_vector_relation_manifest knowledge_vector_relation_manifest_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_vector_relation_manifest
    ADD CONSTRAINT knowledge_vector_relation_manifest_pkey PRIMARY KEY (library_id, dim, vector_kind, embedding_model_key);


--
-- Name: maintenance_job_run maintenance_job_run_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.maintenance_job_run
    ADD CONSTRAINT maintenance_job_run_pkey PRIMARY KEY (id);


--
-- Name: ops_async_operation ops_async_operation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_async_operation
    ADD CONSTRAINT ops_async_operation_pkey PRIMARY KEY (id);


--
-- Name: ops_library_state ops_library_state_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_library_state
    ADD CONSTRAINT ops_library_state_pkey PRIMARY KEY (library_id);


--
-- Name: ops_library_warning ops_library_warning_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_library_warning
    ADD CONSTRAINT ops_library_warning_pkey PRIMARY KEY (id);


--
-- Name: query_chunk_reference query_chunk_reference_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_chunk_reference
    ADD CONSTRAINT query_chunk_reference_pkey PRIMARY KEY (execution_id, chunk_id);


--
-- Name: query_conversation query_conversation_id_workspace_id_library_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_conversation
    ADD CONSTRAINT query_conversation_id_workspace_id_library_id_key UNIQUE (id, workspace_id, library_id);


--
-- Name: query_conversation query_conversation_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_conversation
    ADD CONSTRAINT query_conversation_pkey PRIMARY KEY (id);


--
-- Name: query_execution query_execution_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution
    ADD CONSTRAINT query_execution_pkey PRIMARY KEY (id);


--
-- Name: query_execution_replay query_execution_replay_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution_replay
    ADD CONSTRAINT query_execution_replay_pkey PRIMARY KEY (id);


--
-- Name: query_ir_cache query_ir_cache_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_ir_cache
    ADD CONSTRAINT query_ir_cache_pkey PRIMARY KEY (library_id, question_hash);


--
-- Name: query_llm_context_snapshot query_llm_context_snapshot_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_llm_context_snapshot
    ADD CONSTRAINT query_llm_context_snapshot_pkey PRIMARY KEY (execution_id);


--
-- Name: query_result_cache query_result_cache_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_result_cache
    ADD CONSTRAINT query_result_cache_pkey PRIMARY KEY (cache_key);


--
-- Name: query_turn query_turn_conversation_id_turn_index_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_turn
    ADD CONSTRAINT query_turn_conversation_id_turn_index_key UNIQUE (conversation_id, turn_index);


--
-- Name: query_turn query_turn_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_turn
    ADD CONSTRAINT query_turn_pkey PRIMARY KEY (id);


--
-- Name: runtime_action_record runtime_action_record_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_action_record
    ADD CONSTRAINT runtime_action_record_pkey PRIMARY KEY (id);


--
-- Name: runtime_action_record runtime_action_record_runtime_execution_id_action_ordinal_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_action_record
    ADD CONSTRAINT runtime_action_record_runtime_execution_id_action_ordinal_key UNIQUE (runtime_execution_id, action_ordinal);


--
-- Name: runtime_execution runtime_execution_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_execution
    ADD CONSTRAINT runtime_execution_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_canonical_summary runtime_graph_canonical_summa_library_id_target_kind_target_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_canonical_summary
    ADD CONSTRAINT runtime_graph_canonical_summa_library_id_target_kind_target_key UNIQUE (library_id, target_kind, target_id, source_truth_version);


--
-- Name: runtime_graph_canonical_summary runtime_graph_canonical_summary_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_canonical_summary
    ADD CONSTRAINT runtime_graph_canonical_summary_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_community runtime_graph_community_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_community
    ADD CONSTRAINT runtime_graph_community_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_edge runtime_graph_edge_library_id_canonical_key_projection_vers_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_edge
    ADD CONSTRAINT runtime_graph_edge_library_id_canonical_key_projection_vers_key UNIQUE (library_id, canonical_key, projection_version);


--
-- Name: runtime_graph_edge runtime_graph_edge_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_edge
    ADD CONSTRAINT runtime_graph_edge_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_evidence runtime_graph_evidence_library_id_evidence_identity_key_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_evidence
    ADD CONSTRAINT runtime_graph_evidence_library_id_evidence_identity_key_key UNIQUE (library_id, evidence_identity_key);


--
-- Name: runtime_graph_evidence runtime_graph_evidence_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_evidence
    ADD CONSTRAINT runtime_graph_evidence_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_extraction runtime_graph_extraction_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_extraction
    ADD CONSTRAINT runtime_graph_extraction_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_filtered_artifact runtime_graph_filtered_artifact_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_filtered_artifact
    ADD CONSTRAINT runtime_graph_filtered_artifact_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_node runtime_graph_node_library_id_canonical_key_projection_vers_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_node
    ADD CONSTRAINT runtime_graph_node_library_id_canonical_key_projection_vers_key UNIQUE (library_id, canonical_key, projection_version);


--
-- Name: runtime_graph_node runtime_graph_node_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_node
    ADD CONSTRAINT runtime_graph_node_pkey PRIMARY KEY (id);


--
-- Name: runtime_graph_snapshot runtime_graph_snapshot_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_snapshot
    ADD CONSTRAINT runtime_graph_snapshot_pkey PRIMARY KEY (library_id);


--
-- Name: runtime_policy_decision runtime_policy_decision_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_policy_decision
    ADD CONSTRAINT runtime_policy_decision_pkey PRIMARY KEY (id);


--
-- Name: runtime_provider_profile runtime_provider_profile_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_provider_profile
    ADD CONSTRAINT runtime_provider_profile_pkey PRIMARY KEY (library_id);


--
-- Name: runtime_provider_validation_log runtime_provider_validation_log_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_provider_validation_log
    ADD CONSTRAINT runtime_provider_validation_log_pkey PRIMARY KEY (id);


--
-- Name: runtime_stage_record runtime_stage_record_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_stage_record
    ADD CONSTRAINT runtime_stage_record_pkey PRIMARY KEY (id);


--
-- Name: runtime_stage_record runtime_stage_record_runtime_execution_id_stage_ordinal_att_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_stage_record
    ADD CONSTRAINT runtime_stage_record_runtime_execution_id_stage_ordinal_att_key UNIQUE (runtime_execution_id, stage_ordinal, attempt_no);


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_pkey PRIMARY KEY (id);


--
-- Name: webhook_lifecycle_outbox webhook_lifecycle_outbox_event_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_lifecycle_outbox
    ADD CONSTRAINT webhook_lifecycle_outbox_event_id_key UNIQUE (event_id);


--
-- Name: webhook_lifecycle_outbox webhook_lifecycle_outbox_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_lifecycle_outbox
    ADD CONSTRAINT webhook_lifecycle_outbox_pkey PRIMARY KEY (id);


--
-- Name: webhook_lifecycle_outbox_recipient webhook_lifecycle_outbox_recipient_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_lifecycle_outbox_recipient
    ADD CONSTRAINT webhook_lifecycle_outbox_recipient_pkey PRIMARY KEY (outbox_id, subscription_id);


--
-- Name: webhook_subscription webhook_subscription_id_workspace_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_subscription
    ADD CONSTRAINT webhook_subscription_id_workspace_id_key UNIQUE (id, workspace_id);


--
-- Name: webhook_subscription webhook_subscription_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_subscription
    ADD CONSTRAINT webhook_subscription_pkey PRIMARY KEY (id);


--
-- Name: ai_account_instance_label_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ai_account_instance_label_key ON public.ai_account USING btree (provider_catalog_id, label) WHERE (scope_kind = 'instance'::public.ai_scope_kind);


--
-- Name: ai_account_library_label_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ai_account_library_label_key ON public.ai_account USING btree (library_id, provider_catalog_id, label) WHERE (scope_kind = 'library'::public.ai_scope_kind);


--
-- Name: ai_account_scope_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ai_account_scope_idx ON public.ai_account USING btree (scope_kind, workspace_id, library_id, provider_catalog_id);


--
-- Name: ai_account_workspace_label_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ai_account_workspace_label_key ON public.ai_account USING btree (workspace_id, provider_catalog_id, label) WHERE (scope_kind = 'workspace'::public.ai_scope_kind);


--
-- Name: ai_binding_account_provider_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ai_binding_account_provider_idx ON public.ai_binding USING btree (account_id, provider_catalog_id);


--
-- Name: ai_binding_instance_purpose_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ai_binding_instance_purpose_key ON public.ai_binding USING btree (binding_purpose) WHERE (scope_kind = 'instance'::public.ai_scope_kind);


--
-- Name: ai_binding_library_purpose_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ai_binding_library_purpose_key ON public.ai_binding USING btree (library_id, binding_purpose) WHERE (scope_kind = 'library'::public.ai_scope_kind);


--
-- Name: ai_binding_model_provider_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ai_binding_model_provider_idx ON public.ai_binding USING btree (model_catalog_id, provider_catalog_id);


--
-- Name: ai_binding_model_purpose_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ai_binding_model_purpose_idx ON public.ai_binding USING btree (model_catalog_id, binding_purpose);


--
-- Name: ai_binding_scope_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ai_binding_scope_idx ON public.ai_binding USING btree (scope_kind, workspace_id, library_id, binding_purpose, binding_state);


--
-- Name: ai_binding_workspace_purpose_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ai_binding_workspace_purpose_key ON public.ai_binding USING btree (workspace_id, binding_purpose) WHERE (scope_kind = 'workspace'::public.ai_scope_kind);


--
-- Name: content_document_library_id_external_key_active_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX content_document_library_id_external_key_active_idx ON public.content_document USING btree (library_id, external_key) WHERE (document_state = 'active'::public.content_document_state);


--
-- Name: content_mutation_item_document_result_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX content_mutation_item_document_result_idx ON public.content_mutation_item USING btree (document_id, result_revision_id);


--
-- Name: content_revision_ingest_unit_state_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX content_revision_ingest_unit_state_idx ON public.content_revision_ingest_unit USING btree (revision_id, stage_name, unit_state, unit_ordinal);


--
-- Name: entity_dedup_verification_cache_lib_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX entity_dedup_verification_cache_lib_idx ON public.entity_dedup_verification_cache USING btree (library_id, created_at DESC);


--
-- Name: idx_ai_account_provider_generation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ai_account_provider_generation ON public.ai_account USING btree (provider_catalog_id);


--
-- Name: idx_ai_binding_active_account_generation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ai_binding_active_account_generation ON public.ai_binding USING btree (account_id) WHERE (binding_state = 'active'::public.ai_binding_state);


--
-- Name: idx_ai_binding_active_model_generation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ai_binding_active_model_generation ON public.ai_binding USING btree (model_catalog_id) WHERE (binding_state = 'active'::public.ai_binding_state);


--
-- Name: idx_ai_binding_library_purpose; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ai_binding_library_purpose ON public.ai_binding USING btree (library_id, binding_purpose, binding_state);


--
-- Name: idx_audit_event_actor_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_event_actor_created_at ON public.audit_event USING btree (actor_principal_id, created_at DESC);


--
-- Name: idx_audit_event_request_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_audit_event_request_id ON public.audit_event USING btree (request_id) WHERE (request_id IS NOT NULL);


--
-- Name: idx_billing_charge_usage; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_charge_usage ON public.billing_charge USING btree (usage_id);


--
-- Name: idx_billing_execution_cost_library; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_library ON public.billing_execution_cost USING btree (library_id);


--
-- Name: idx_billing_execution_cost_library_document; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_library_document ON public.billing_execution_cost USING btree (library_id, knowledge_document_id) WHERE (knowledge_document_id IS NOT NULL);


--
-- Name: idx_billing_execution_cost_rollup_state_due; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_rollup_state_due ON public.billing_execution_cost_rollup_state USING btree (next_attempt_at, dirty_at, owning_execution_id) WHERE (applied_generation < dirty_generation);


--
-- Name: idx_billing_execution_cost_rollup_state_library_dirty; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_rollup_state_library_dirty ON public.billing_execution_cost_rollup_state USING btree (library_id, dirty_at, owning_execution_id) WHERE (applied_generation < dirty_generation);


--
-- Name: idx_billing_execution_cost_rollup_state_library_terminal; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_rollup_state_library_terminal ON public.billing_execution_cost_rollup_state USING btree (library_id, owning_execution_id) WHERE (terminal_error_code IS NOT NULL);


--
-- Name: idx_billing_execution_cost_rollup_state_workspace_dirty; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_rollup_state_workspace_dirty ON public.billing_execution_cost_rollup_state USING btree (workspace_id, dirty_at, owning_execution_id) WHERE (applied_generation < dirty_generation);


--
-- Name: idx_billing_execution_cost_rollup_state_workspace_terminal; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_rollup_state_workspace_terminal ON public.billing_execution_cost_rollup_state USING btree (workspace_id, owning_execution_id) WHERE (terminal_error_code IS NOT NULL);


--
-- Name: idx_billing_execution_cost_workspace; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_execution_cost_workspace ON public.billing_execution_cost USING btree (workspace_id, library_id, knowledge_document_id) INCLUDE (total_cost, currency_code, provider_call_count, unpriced_provider_call_count);


--
-- Name: idx_billing_provider_call_library; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_provider_call_library ON public.billing_provider_call USING btree (library_id);


--
-- Name: idx_billing_provider_call_owner; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_provider_call_owner ON public.billing_provider_call USING btree (owning_execution_kind, owning_execution_id);


--
-- Name: idx_billing_provider_call_runtime_execution_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_provider_call_runtime_execution_id ON public.billing_provider_call USING btree (runtime_execution_id) WHERE (runtime_execution_id IS NOT NULL);


--
-- Name: idx_billing_provider_call_started_at_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_provider_call_started_at_pending ON public.billing_provider_call USING btree (started_at, id) WHERE (call_state = 'started'::public.billing_call_state);


--
-- Name: idx_billing_usage_provider_call; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_billing_usage_provider_call ON public.billing_usage USING btree (provider_call_id);


--
-- Name: idx_catalog_library_connector_library_sync_mode; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_catalog_library_connector_library_sync_mode ON public.catalog_library_connector USING btree (library_id, sync_mode, last_sync_requested_at);


--
-- Name: idx_catalog_library_provisioning_owner; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_catalog_library_provisioning_owner ON public.catalog_library USING btree (provisioned_by_principal_id, workspace_id) WHERE (provisioned_by_principal_id IS NOT NULL);


--
-- Name: idx_catalog_library_workspace_lifecycle; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_catalog_library_workspace_lifecycle ON public.catalog_library USING btree (workspace_id, lifecycle_state);


--
-- Name: idx_catalog_workspace_provisioning_owner; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_catalog_workspace_provisioning_owner ON public.catalog_workspace USING btree (provisioned_by_principal_id) WHERE (provisioned_by_principal_id IS NOT NULL);


--
-- Name: idx_content_chunk_occurred_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_chunk_occurred_at ON public.content_chunk USING btree (revision_id, occurred_at, occurred_until) WHERE (occurred_at IS NOT NULL);


--
-- Name: idx_content_chunk_revision_page; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_chunk_revision_page ON public.content_chunk USING btree (revision_id, source_page) WHERE (source_page IS NOT NULL);


--
-- Name: idx_content_chunk_text_checksum_revision; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_chunk_text_checksum_revision ON public.content_chunk USING btree (text_checksum, revision_id, id);


--
-- Name: idx_content_document_external_key_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_document_external_key_trgm ON public.content_document USING gin (lower(external_key) public.gin_trgm_ops);


--
-- Name: idx_content_document_head_recovery_candidates; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_document_head_recovery_candidates ON public.content_document_head USING btree (last_recovery_attempt_at NULLS FIRST) WHERE ((readable_revision_id IS NULL) AND (active_revision_id IS NULL) AND (dead_letter_at IS NULL));


--
-- Name: idx_content_document_library_created_cursor; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_document_library_created_cursor ON public.content_document USING btree (library_id, created_at DESC, id DESC);


--
-- Name: idx_content_document_library_parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_document_library_parent ON public.content_document USING btree (library_id, parent_document_id) WHERE (parent_document_id IS NOT NULL);


--
-- Name: idx_content_document_library_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_document_library_state ON public.content_document USING btree (library_id, document_state);


--
-- Name: idx_content_mutation_idempotency; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_content_mutation_idempotency ON public.content_mutation_reference USING btree (reservation_principal_id, request_surface, idempotency_key) WHERE (idempotency_key IS NOT NULL);


--
-- Name: idx_content_mutation_item_head_parentage; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_mutation_item_head_parentage ON public.content_mutation_item USING btree (mutation_id, document_id) INCLUDE (base_revision_id, result_revision_id);


--
-- Name: idx_content_mutation_library_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_mutation_library_state ON public.content_mutation USING btree (library_id, mutation_state, requested_at DESC);


--
-- Name: idx_content_mutation_request_fingerprint; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_mutation_request_fingerprint ON public.content_mutation USING btree (request_fingerprint) WHERE (request_fingerprint IS NOT NULL);


--
-- Name: idx_content_mutation_scoped_idempotency; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_content_mutation_scoped_idempotency ON public.content_mutation_reference USING btree (idempotency_scope, request_surface, idempotency_key) WHERE ((idempotency_scope IS NOT NULL) AND (idempotency_key IS NOT NULL));


--
-- Name: idx_content_revision_document_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_revision_document_created_at ON public.content_revision USING btree (document_id, created_at DESC);


--
-- Name: idx_content_revision_library_checksum; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_revision_library_checksum ON public.content_revision USING btree (library_id, checksum);


--
-- Name: idx_content_web_discovered_page_document; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_web_discovered_page_document ON public.content_web_discovered_page USING btree (document_id);


--
-- Name: idx_content_web_discovered_page_mutation_item; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_content_web_discovered_page_mutation_item ON public.content_web_discovered_page USING btree (mutation_item_id) WHERE (mutation_item_id IS NOT NULL);


--
-- Name: idx_content_web_discovered_page_run_canonical_url; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_web_discovered_page_run_canonical_url ON public.content_web_discovered_page USING btree (run_id, canonical_url) WHERE (canonical_url IS NOT NULL);


--
-- Name: idx_content_web_discovered_page_run_normalized_url; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_content_web_discovered_page_run_normalized_url ON public.content_web_discovered_page USING btree (run_id, normalized_url);


--
-- Name: idx_content_web_discovered_page_run_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_web_discovered_page_run_state ON public.content_web_discovered_page USING btree (run_id, candidate_state, depth, discovered_at);


--
-- Name: idx_content_web_ingest_run_library_requested; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_web_ingest_run_library_requested ON public.content_web_ingest_run USING btree (library_id, requested_at DESC);


--
-- Name: idx_content_web_ingest_run_library_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_web_ingest_run_library_state ON public.content_web_ingest_run USING btree (library_id, run_state, requested_at DESC);


--
-- Name: idx_content_web_ingest_run_mutation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_content_web_ingest_run_mutation ON public.content_web_ingest_run USING btree (mutation_id);


--
-- Name: idx_edge_bundle_chunk_bundle_rank; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_edge_bundle_chunk_bundle_rank ON public.knowledge_bundle_chunk USING btree (bundle_id, rank, created_at);


--
-- Name: idx_edge_bundle_entity_bundle_rank; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_edge_bundle_entity_bundle_rank ON public.knowledge_bundle_entity USING btree (bundle_id, rank, created_at);


--
-- Name: idx_edge_bundle_evi_bundle_rank; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_edge_bundle_evi_bundle_rank ON public.knowledge_bundle_evidence USING btree (bundle_id, rank, created_at);


--
-- Name: idx_edge_bundle_rel_bundle_rank; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_edge_bundle_rel_bundle_rank ON public.knowledge_bundle_relation USING btree (bundle_id, rank, created_at);


--
-- Name: idx_extract_chunk_result_attempt_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_extract_chunk_result_attempt_state ON public.extract_chunk_result USING btree (attempt_id, extract_state);


--
-- Name: idx_extract_edge_candidate_chunk_result_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_extract_edge_candidate_chunk_result_id ON public.extract_edge_candidate USING btree (chunk_result_id);


--
-- Name: idx_extract_node_candidate_chunk_result_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_extract_node_candidate_chunk_result_id ON public.extract_node_candidate USING btree (chunk_result_id);


--
-- Name: idx_iam_api_token_secret_latest_active; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_iam_api_token_secret_latest_active ON public.iam_api_token_secret USING btree (token_principal_id) WHERE (revoked_at IS NULL);


--
-- Name: idx_iam_grant_principal_resource; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_iam_grant_principal_resource ON public.iam_grant USING btree (principal_id, resource_kind, resource_id);


--
-- Name: idx_iam_service_identity_active_credential; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_iam_service_identity_active_credential ON public.iam_service_identity_operation USING btree (service_identity_principal_id) WHERE (encrypted_credential IS NOT NULL);


--
-- Name: idx_iam_service_identity_owner; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_iam_service_identity_owner ON public.iam_service_identity USING btree (owner_auth_provider_kind, owner_external_subject, created_at, principal_id);


--
-- Name: idx_iam_service_identity_provisioning_authority; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_iam_service_identity_provisioning_authority ON public.iam_service_identity USING btree (provisioning_authority) WHERE (provisioning_authority IS NOT NULL);


--
-- Name: idx_ingest_attempt_job_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_attempt_job_state ON public.ingest_attempt USING btree (job_id, attempt_state, started_at DESC);


--
-- Name: idx_ingest_job_active_queue_rank; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_job_active_queue_rank ON public.ingest_job USING btree (queue_state, queue_rank, priority, available_at, queued_at, id) WHERE (queue_state = ANY (ARRAY['queued'::public.ingest_queue_state, 'leased'::public.ingest_queue_state, 'paused'::public.ingest_queue_state]));


--
-- Name: idx_ingest_job_content_mutation_item; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_ingest_job_content_mutation_item ON public.ingest_job USING btree (mutation_item_id) WHERE ((mutation_item_id IS NOT NULL) AND (job_kind = 'content_mutation'::public.ingest_job_kind));


--
-- Name: idx_ingest_job_dedupe_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_ingest_job_dedupe_key ON public.ingest_job USING btree (library_id, dedupe_key) WHERE (dedupe_key IS NOT NULL);


--
-- Name: idx_ingest_job_knowledge_document; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_job_knowledge_document ON public.ingest_job USING btree (knowledge_document_id, queue_state, queued_at DESC) WHERE (knowledge_document_id IS NOT NULL);


--
-- Name: idx_ingest_job_library_queue; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_job_library_queue ON public.ingest_job USING btree (library_id, queue_state, priority, available_at);


--
-- Name: idx_ingest_job_mutation_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_job_mutation_id ON public.ingest_job USING btree (mutation_id) WHERE (mutation_id IS NOT NULL);


--
-- Name: idx_ingest_job_queue_lease_recovery; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_job_queue_lease_recovery ON public.ingest_job USING btree (queue_state, queue_leased_at, queued_at, id) WHERE (queue_state = 'leased'::public.ingest_queue_state);


--
-- Name: idx_ingest_stage_event_attempt_ordinal; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_stage_event_attempt_ordinal ON public.ingest_stage_event USING btree (attempt_id, ordinal);


--
-- Name: idx_ingest_stage_event_recorded_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_stage_event_recorded_at ON public.ingest_stage_event USING btree (recorded_at);


--
-- Name: idx_ingest_stage_provider_call_stage_event; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ingest_stage_provider_call_stage_event ON public.ingest_stage_provider_call USING btree (stage_event_id);


--
-- Name: idx_knowledge_bundle_chunk_chunk_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_bundle_chunk_chunk_fk ON public.knowledge_bundle_chunk USING btree (chunk_id);


--
-- Name: idx_knowledge_chunk_document_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_chunk_document_fk ON public.knowledge_chunk USING btree (document_id);


--
-- Name: idx_knowledge_chunk_primary_block_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_chunk_primary_block_fk ON public.knowledge_chunk USING btree (primary_block_id) WHERE (primary_block_id IS NOT NULL);


--
-- Name: idx_knowledge_context_bundle_query_execution_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_knowledge_context_bundle_query_execution_unique ON public.knowledge_context_bundle USING btree (query_execution_id) WHERE (query_execution_id IS NOT NULL);


--
-- Name: idx_knowledge_document_library_parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_document_library_parent ON public.knowledge_document USING btree (library_id, parent_document_id) WHERE (parent_document_id IS NOT NULL);


--
-- Name: idx_knowledge_entity_candidate_chunk_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_entity_candidate_chunk_fk ON public.knowledge_entity_candidate USING btree (chunk_id) WHERE (chunk_id IS NOT NULL);


--
-- Name: idx_knowledge_entity_candidate_revision_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_entity_candidate_revision_fk ON public.knowledge_entity_candidate USING btree (revision_id);


--
-- Name: idx_knowledge_evidence_block_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_evidence_block_fk ON public.knowledge_evidence USING btree (block_id) WHERE (block_id IS NOT NULL);


--
-- Name: idx_knowledge_evidence_chunk_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_evidence_chunk_fk ON public.knowledge_evidence USING btree (chunk_id) WHERE (chunk_id IS NOT NULL);


--
-- Name: idx_knowledge_evidence_document_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_evidence_document_fk ON public.knowledge_evidence USING btree (document_id);


--
-- Name: idx_knowledge_evidence_revision_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_evidence_revision_fk ON public.knowledge_evidence USING btree (revision_id);


--
-- Name: idx_knowledge_relation_candidate_chunk_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_relation_candidate_chunk_fk ON public.knowledge_relation_candidate USING btree (chunk_id) WHERE (chunk_id IS NOT NULL);


--
-- Name: idx_knowledge_relation_candidate_revision_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_relation_candidate_revision_fk ON public.knowledge_relation_candidate USING btree (revision_id);


--
-- Name: idx_knowledge_structured_block_document_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_structured_block_document_fk ON public.knowledge_structured_block USING btree (document_id);


--
-- Name: idx_knowledge_structured_block_parent_block_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_structured_block_parent_block_fk ON public.knowledge_structured_block USING btree (parent_block_id) WHERE (parent_block_id IS NOT NULL);


--
-- Name: idx_knowledge_structured_block_setup_order; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_structured_block_setup_order ON public.knowledge_structured_block USING btree (revision_id, ordinal, block_id) WHERE (block_kind = ANY (ARRAY['table'::text, 'table_row'::text, 'code_block'::text, 'source_unit'::text]));


--
-- Name: idx_knowledge_structured_revision_document_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_structured_revision_document_fk ON public.knowledge_structured_revision USING btree (document_id);


--
-- Name: idx_knowledge_technical_fact_document_fk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_knowledge_technical_fact_document_fk ON public.knowledge_technical_fact USING btree (document_id);


--
-- Name: idx_ops_async_operation_active_library_delete; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_ops_async_operation_active_library_delete ON public.ops_async_operation USING btree (library_id, operation_kind, subject_kind, subject_id) WHERE ((library_id IS NOT NULL) AND (operation_kind = 'delete_library'::text) AND (status = ANY (ARRAY['accepted'::public.ops_async_operation_status, 'processing'::public.ops_async_operation_status])));


--
-- Name: idx_ops_async_operation_active_workspace_delete; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_ops_async_operation_active_workspace_delete ON public.ops_async_operation USING btree (workspace_id, operation_kind, subject_kind, subject_id) WHERE ((library_id IS NULL) AND (operation_kind = 'delete_workspace'::text) AND (status = ANY (ARRAY['accepted'::public.ops_async_operation_status, 'processing'::public.ops_async_operation_status])));


--
-- Name: idx_ops_async_operation_library_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ops_async_operation_library_status ON public.ops_async_operation USING btree (library_id, status, created_at DESC);


--
-- Name: idx_ops_async_operation_parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ops_async_operation_parent ON public.ops_async_operation USING btree (parent_async_operation_id) WHERE (parent_async_operation_id IS NOT NULL);


--
-- Name: idx_ops_async_operation_subject; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_ops_async_operation_subject ON public.ops_async_operation USING btree (subject_kind, subject_id, created_at DESC, id DESC) WHERE (subject_id IS NOT NULL);


--
-- Name: idx_query_conversation_library_surface_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_conversation_library_surface_updated ON public.query_conversation USING btree (library_id, request_surface, updated_at DESC);


--
-- Name: idx_query_conversation_library_updated_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_conversation_library_updated_at ON public.query_conversation USING btree (library_id, updated_at DESC);


--
-- Name: idx_query_execution_conversation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_execution_conversation ON public.query_execution USING btree (conversation_id, id);


--
-- Name: idx_query_execution_library_started_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_execution_library_started_at ON public.query_execution USING btree (library_id, started_at DESC);


--
-- Name: idx_query_execution_replay_conversation; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_execution_replay_conversation ON public.query_execution_replay USING btree (conversation_id, created_at DESC);


--
-- Name: idx_query_execution_replay_request_turn; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_query_execution_replay_request_turn ON public.query_execution_replay USING btree (request_turn_id);


--
-- Name: idx_query_execution_replay_response_turn; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_query_execution_replay_response_turn ON public.query_execution_replay USING btree (response_turn_id);


--
-- Name: idx_query_execution_replay_source_execution; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_execution_replay_source_execution ON public.query_execution_replay USING btree (source_execution_id, created_at DESC);


--
-- Name: idx_query_execution_runtime_execution_id; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_query_execution_runtime_execution_id ON public.query_execution USING btree (runtime_execution_id);


--
-- Name: idx_query_ir_cache_compiled_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_ir_cache_compiled_at ON public.query_ir_cache USING btree (compiled_at DESC);


--
-- Name: idx_query_llm_context_snapshot_captured; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_llm_context_snapshot_captured ON public.query_llm_context_snapshot USING btree (captured_at DESC);


--
-- Name: idx_query_result_cache_gc_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_result_cache_gc_updated ON public.query_result_cache USING btree (updated_at, cache_key);


--
-- Name: idx_query_result_cache_library_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_result_cache_library_updated ON public.query_result_cache USING btree (library_id, updated_at DESC);


--
-- Name: idx_query_result_cache_source_execution; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_query_result_cache_source_execution ON public.query_result_cache USING btree (source_execution_id);


--
-- Name: idx_runtime_action_record_execution; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_action_record_execution ON public.runtime_action_record USING btree (runtime_execution_id, action_ordinal);


--
-- Name: idx_runtime_execution_owner; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_execution_owner ON public.runtime_execution USING btree (owner_kind, owner_id, accepted_at DESC);


--
-- Name: idx_runtime_graph_community_projection_size; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_community_projection_size ON public.runtime_graph_community USING btree (library_id, projection_version, cardinality(member_node_ids) DESC, id);


--
-- Name: idx_runtime_graph_edge_context_score; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_context_score ON public.runtime_graph_edge USING btree (library_id, projection_version, edge_score DESC, rank) WHERE (edge_score IS NOT NULL);


--
-- Name: idx_runtime_graph_edge_from_node_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_from_node_id ON public.runtime_graph_edge USING btree (from_node_id);


--
-- Name: idx_runtime_graph_edge_library_projection; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_library_projection ON public.runtime_graph_edge USING btree (library_id, projection_version, created_at);


--
-- Name: idx_runtime_graph_edge_library_projection_nodes; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_library_projection_nodes ON public.runtime_graph_edge USING btree (library_id, projection_version, from_node_id, to_node_id);


--
-- Name: idx_runtime_graph_edge_projection_relation_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_projection_relation_created ON public.runtime_graph_edge USING btree (library_id, projection_version, relation_type, created_at) WHERE ((btrim(relation_type) <> ''::text) AND (from_node_id <> to_node_id));


--
-- Name: idx_runtime_graph_edge_projection_support_admitted; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_projection_support_admitted ON public.runtime_graph_edge USING btree (library_id, projection_version, support_count DESC, relation_type, created_at, id) WHERE ((btrim(relation_type) <> ''::text) AND (from_node_id <> to_node_id));


--
-- Name: idx_runtime_graph_edge_to_node_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_to_node_id ON public.runtime_graph_edge USING btree (to_node_id);


--
-- Name: idx_runtime_graph_edge_traverse_in; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_traverse_in ON public.runtime_graph_edge USING btree (library_id, projection_version, to_node_id, support_count DESC) WHERE ((btrim(relation_type) <> ''::text) AND (from_node_id <> to_node_id));


--
-- Name: idx_runtime_graph_edge_traverse_out; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_edge_traverse_out ON public.runtime_graph_edge USING btree (library_id, projection_version, from_node_id, support_count DESC) WHERE ((btrim(relation_type) <> ''::text) AND (from_node_id <> to_node_id));


--
-- Name: idx_runtime_graph_evidence_chunk_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_chunk_id ON public.runtime_graph_evidence USING btree (chunk_id) WHERE (chunk_id IS NOT NULL);


--
-- Name: idx_runtime_graph_evidence_context_score; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_context_score ON public.runtime_graph_evidence USING btree (library_id, target_kind, target_id, score DESC, rank) WHERE (score IS NOT NULL);


--
-- Name: idx_runtime_graph_evidence_library_body_literal_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_library_body_literal_trgm ON public.runtime_graph_evidence USING gin (library_id, lower(evidence_text) public.gin_trgm_ops) WHERE (btrim(evidence_text) <> ''::text);


--
-- Name: idx_runtime_graph_evidence_library_body_text_search; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_library_body_text_search ON public.runtime_graph_evidence USING gin (library_id, to_tsvector('simple'::regconfig, evidence_text)) WHERE (btrim(evidence_text) <> ''::text);


--
-- Name: idx_runtime_graph_evidence_library_document; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_library_document ON public.runtime_graph_evidence USING btree (library_id, document_id, created_at DESC);


--
-- Name: idx_runtime_graph_evidence_library_document_revision; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_library_document_revision ON public.runtime_graph_evidence USING btree (library_id, document_id, revision_id, created_at DESC);


--
-- Name: idx_runtime_graph_evidence_library_target; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_library_target ON public.runtime_graph_evidence USING btree (library_id, target_kind, target_id, created_at DESC);


--
-- Name: idx_runtime_graph_evidence_library_target_document; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_library_target_document ON public.runtime_graph_evidence USING btree (library_id, target_kind, document_id, target_id) WHERE (document_id IS NOT NULL);


--
-- Name: idx_runtime_graph_evidence_library_text_ready; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_evidence_library_text_ready ON public.runtime_graph_evidence USING btree (library_id, created_at DESC, id DESC) WHERE (btrim(evidence_text) <> ''::text);


--
-- Name: idx_runtime_graph_extraction_cache_lookup; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_extraction_cache_lookup ON public.runtime_graph_extraction USING btree (library_id, extraction_version, provider_kind, model_name, prompt_hash, status, created_at DESC);


--
-- Name: idx_runtime_graph_extraction_document_revision_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_extraction_document_revision_status ON public.runtime_graph_extraction USING btree (document_id, ((raw_output_json #>> '{lifecycle,revision_id}'::text[])), status);


--
-- Name: idx_runtime_graph_extraction_runtime_execution_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_extraction_runtime_execution_id ON public.runtime_graph_extraction USING btree (runtime_execution_id);


--
-- Name: idx_runtime_graph_filtered_artifact_library_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_filtered_artifact_library_created_at ON public.runtime_graph_filtered_artifact USING btree (library_id, created_at DESC);


--
-- Name: idx_runtime_graph_node_entity_aliases_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_entity_aliases_trgm ON public.runtime_graph_node USING gin (lower((aliases_json)::text) public.gin_trgm_ops) WHERE (node_type <> 'document'::text);


--
-- Name: idx_runtime_graph_node_entity_label_exact; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_entity_label_exact ON public.runtime_graph_node USING btree (library_id, projection_version, lower(label), support_count DESC, created_at, id) WHERE (node_type <> 'document'::text);


--
-- Name: idx_runtime_graph_node_entity_label_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_entity_label_trgm ON public.runtime_graph_node USING gin (lower(label) public.gin_trgm_ops) WHERE (node_type <> 'document'::text);


--
-- Name: idx_runtime_graph_node_entity_summary_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_entity_summary_trgm ON public.runtime_graph_node USING gin (lower(COALESCE(summary, ''::text)) public.gin_trgm_ops) WHERE ((node_type <> 'document'::text) AND (summary IS NOT NULL) AND (btrim(summary) <> ''::text));


--
-- Name: idx_runtime_graph_node_library_projection; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_library_projection ON public.runtime_graph_node USING btree (library_id, projection_version, created_at);


--
-- Name: idx_runtime_graph_node_library_projection_support_zero; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_library_projection_support_zero ON public.runtime_graph_node USING btree (library_id, projection_version) WHERE (support_count <= 0);


--
-- Name: idx_runtime_graph_node_library_projection_type; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_library_projection_type ON public.runtime_graph_node USING btree (library_id, projection_version, node_type);


--
-- Name: idx_runtime_graph_node_projection_entity_support; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_projection_entity_support ON public.runtime_graph_node USING btree (library_id, projection_version, support_count DESC, label, created_at, id) WHERE (node_type <> 'document'::text);


--
-- Name: idx_runtime_graph_node_projection_type_label_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_graph_node_projection_type_label_created ON public.runtime_graph_node USING btree (library_id, projection_version, node_type, label, created_at);


--
-- Name: idx_runtime_graph_node_unique_document; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_runtime_graph_node_unique_document ON public.runtime_graph_node USING btree (library_id, document_id, projection_version) WHERE (node_type = 'document'::text);


--
-- Name: idx_runtime_policy_decision_execution; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_policy_decision_execution ON public.runtime_policy_decision USING btree (runtime_execution_id, created_at);


--
-- Name: idx_runtime_provider_validation_log_library_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_provider_validation_log_library_created_at ON public.runtime_provider_validation_log USING btree (library_id, created_at DESC);


--
-- Name: idx_runtime_provider_validation_log_provider_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_provider_validation_log_provider_created_at ON public.runtime_provider_validation_log USING btree (provider_kind, model_name, capability, created_at DESC);


--
-- Name: idx_runtime_stage_record_execution; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_runtime_stage_record_execution ON public.runtime_stage_record USING btree (runtime_execution_id, stage_ordinal, attempt_no);


--
-- Name: idx_webhook_delivery_catalog_blockers; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_catalog_blockers ON public.webhook_delivery_attempt USING btree (library_id, id) WHERE ((delivery_state = ANY (ARRAY['pending'::public.webhook_delivery_state, 'delivering'::public.webhook_delivery_state])) OR ((delivery_state = 'failed'::public.webhook_delivery_state) AND (next_attempt_at IS NOT NULL)));


--
-- Name: idx_webhook_delivery_event; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_event ON public.webhook_delivery_attempt USING btree (event_type, event_id);


--
-- Name: idx_webhook_delivery_job; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_job ON public.webhook_delivery_attempt USING btree (job_id) WHERE (job_id IS NOT NULL);


--
-- Name: idx_webhook_delivery_library_linked_job; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_library_linked_job ON public.webhook_delivery_attempt USING btree (library_id, job_id) WHERE (job_id IS NOT NULL);


--
-- Name: idx_webhook_delivery_stale_lease; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_stale_lease ON public.webhook_delivery_attempt USING btree (updated_at, id) WHERE (delivery_state = 'delivering'::public.webhook_delivery_state);


--
-- Name: idx_webhook_delivery_subscription_keyset; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_subscription_keyset ON public.webhook_delivery_attempt USING btree (subscription_id, created_at DESC, id DESC);


--
-- Name: idx_webhook_delivery_subscription_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_subscription_state ON public.webhook_delivery_attempt USING btree (subscription_id, delivery_state, next_attempt_at);


--
-- Name: idx_webhook_delivery_subscription_state_keyset; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_delivery_subscription_state_keyset ON public.webhook_delivery_attempt USING btree (subscription_id, delivery_state, created_at DESC, id DESC);


--
-- Name: idx_webhook_lifecycle_outbox_dead_letter; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_dead_letter ON public.webhook_lifecycle_outbox USING btree (updated_at, id) WHERE (dispatch_state = 'dead_letter'::text);


--
-- Name: idx_webhook_lifecycle_outbox_dispatched_retention; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_dispatched_retention ON public.webhook_lifecycle_outbox USING btree (dispatched_at, id) WHERE (dispatch_state = 'dispatched'::text);


--
-- Name: idx_webhook_lifecycle_outbox_due; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_due ON public.webhook_lifecycle_outbox USING btree (available_at, created_at, id) WHERE (dispatch_state = 'pending'::text);


--
-- Name: idx_webhook_lifecycle_outbox_expired_lease; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_expired_lease ON public.webhook_lifecycle_outbox USING btree (lease_expires_at, created_at, id) WHERE (dispatch_state = 'dispatching'::text);


--
-- Name: idx_webhook_lifecycle_outbox_library_blockers; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_library_blockers ON public.webhook_lifecycle_outbox USING btree (library_id, id) WHERE (dispatch_state <> 'dispatched'::text);


--
-- Name: idx_webhook_lifecycle_outbox_library_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_library_created ON public.webhook_lifecycle_outbox USING btree (library_id, created_at, id);


--
-- Name: idx_webhook_lifecycle_outbox_library_dead_letter; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_library_dead_letter ON public.webhook_lifecycle_outbox USING btree (library_id, updated_at DESC, id DESC) WHERE (dispatch_state = 'dead_letter'::text);


--
-- Name: idx_webhook_lifecycle_outbox_library_unresolved_blockers; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_library_unresolved_blockers ON public.webhook_lifecycle_outbox USING btree (library_id, id) WHERE (dispatch_state <> ALL (ARRAY['dispatched'::text, 'resolved'::text]));


--
-- Name: idx_webhook_lifecycle_outbox_recipient_subscription; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_lifecycle_outbox_recipient_subscription ON public.webhook_lifecycle_outbox_recipient USING btree (subscription_id, outbox_id);


--
-- Name: idx_webhook_subscription_library_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_subscription_library_active ON public.webhook_subscription USING btree (library_id, active) WHERE (active AND (library_id IS NOT NULL));


--
-- Name: idx_webhook_subscription_workspace_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_subscription_workspace_active ON public.webhook_subscription USING btree (workspace_id, active) WHERE active;


--
-- Name: idx_webhook_subscription_workspace_keyset; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_webhook_subscription_workspace_keyset ON public.webhook_subscription USING btree (workspace_id, created_at, id);


--
-- Name: knowledge_bundle_chunk_rank_score_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_bundle_chunk_rank_score_index ON public.knowledge_bundle_chunk USING btree (bundle_id, rank, score DESC, chunk_id);


--
-- Name: knowledge_bundle_entity_rank_score_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_bundle_entity_rank_score_index ON public.knowledge_bundle_entity USING btree (bundle_id, rank, score DESC, entity_id);


--
-- Name: knowledge_bundle_evidence_rank_score_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_bundle_evidence_rank_score_index ON public.knowledge_bundle_evidence USING btree (bundle_id, rank, score DESC, evidence_id);


--
-- Name: knowledge_bundle_relation_rank_score_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_bundle_relation_rank_score_index ON public.knowledge_bundle_relation USING btree (bundle_id, rank, score DESC, relation_id);


--
-- Name: knowledge_chunk_content_text_lower_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_content_text_lower_trgm ON public.knowledge_chunk USING gin (lower(content_text) public.gin_trgm_ops);


--
-- Name: knowledge_chunk_entity_mention_from_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_entity_mention_from_index ON public.knowledge_chunk_entity_mention USING btree (library_id, from_id, relation_type, support DESC);


--
-- Name: knowledge_chunk_entity_mention_to_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_entity_mention_to_index ON public.knowledge_chunk_entity_mention USING btree (library_id, to_id, relation_type, support DESC);


--
-- Name: knowledge_chunk_library_document_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_library_document_index ON public.knowledge_chunk USING btree (library_id, document_id, chunk_state, chunk_index);


--
-- Name: knowledge_chunk_library_lower_text_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_library_lower_text_trgm ON public.knowledge_chunk USING gin (library_id, (((((lower(normalized_text) || chr(1)) || lower(content_text)) || chr(1)) || lower(COALESCE(window_text, ''::text)))) public.gin_trgm_ops);


--
-- Name: knowledge_chunk_library_occurred_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_library_occurred_index ON public.knowledge_chunk USING btree (library_id, occurred_at);


--
-- Name: knowledge_chunk_normalized_text_lower_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_normalized_text_lower_trgm ON public.knowledge_chunk USING gin (lower(normalized_text) public.gin_trgm_ops);


--
-- Name: knowledge_chunk_normalized_text_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_normalized_text_trgm ON public.knowledge_chunk USING gin (normalized_text public.gin_trgm_ops);


--
-- Name: knowledge_chunk_revision_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_revision_index ON public.knowledge_chunk USING btree (revision_id, chunk_index, chunk_id);


--
-- Name: knowledge_chunk_search_tsv_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_search_tsv_gin ON public.knowledge_chunk USING gin (search_tsv) WITH (fastupdate=off);


--
-- Name: knowledge_chunk_support_block_ids_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_support_block_ids_gin ON public.knowledge_chunk USING gin (support_block_ids);


--
-- Name: knowledge_chunk_window_text_lower_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_chunk_window_text_lower_trgm ON public.knowledge_chunk USING gin (lower(COALESCE(window_text, ''::text)) public.gin_trgm_ops);


--
-- Name: knowledge_context_bundle_execution_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_context_bundle_execution_index ON public.knowledge_context_bundle USING btree (query_execution_id) WHERE (query_execution_id IS NOT NULL);


--
-- Name: knowledge_context_bundle_library_updated_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_context_bundle_library_updated_index ON public.knowledge_context_bundle USING btree (library_id, updated_at, bundle_id);


--
-- Name: knowledge_document_external_key_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX knowledge_document_external_key_unique ON public.knowledge_document USING btree (workspace_id, library_id, external_key);


--
-- Name: knowledge_document_file_name_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_document_file_name_trgm ON public.knowledge_document USING gin (file_name public.gin_trgm_ops);


--
-- Name: knowledge_document_library_updated_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_document_library_updated_index ON public.knowledge_document USING btree (library_id, workspace_id, updated_at, document_id);


--
-- Name: knowledge_document_title_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_document_title_trgm ON public.knowledge_document USING gin (title public.gin_trgm_ops);


--
-- Name: knowledge_entity_candidate_library_revision_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_entity_candidate_library_revision_index ON public.knowledge_entity_candidate USING btree (library_id, revision_id, candidate_state, candidate_id);


--
-- Name: knowledge_entity_library_support_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_entity_library_support_index ON public.knowledge_entity USING btree (library_id, support_count, updated_at, entity_id);


--
-- Name: knowledge_entity_search_tsv_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_entity_search_tsv_gin ON public.knowledge_entity USING gin (search_tsv) WITH (fastupdate=off);


--
-- Name: knowledge_evidence_entity_support_from_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_evidence_entity_support_from_index ON public.knowledge_evidence_entity_support USING btree (library_id, from_id, relation_type, support DESC);


--
-- Name: knowledge_evidence_entity_support_to_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_evidence_entity_support_to_index ON public.knowledge_evidence_entity_support USING btree (library_id, to_id, relation_type, support DESC);


--
-- Name: knowledge_evidence_fact_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_evidence_fact_id_index ON public.knowledge_evidence USING btree (fact_id, created_at);


--
-- Name: knowledge_evidence_relation_support_from_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_evidence_relation_support_from_index ON public.knowledge_evidence_relation_support USING btree (library_id, from_id, relation_type, support DESC);


--
-- Name: knowledge_evidence_relation_support_to_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_evidence_relation_support_to_index ON public.knowledge_evidence_relation_support USING btree (library_id, to_id, relation_type, support DESC);


--
-- Name: knowledge_evidence_search_tsv_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_evidence_search_tsv_gin ON public.knowledge_evidence USING gin (search_tsv) WITH (fastupdate=off);


--
-- Name: knowledge_evidence_source_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_evidence_source_index ON public.knowledge_evidence USING btree (library_id, document_id, revision_id, chunk_id);


--
-- Name: knowledge_relation_candidate_library_revision_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_relation_candidate_library_revision_index ON public.knowledge_relation_candidate USING btree (library_id, revision_id, candidate_state, candidate_id);


--
-- Name: knowledge_relation_library_support_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_relation_library_support_index ON public.knowledge_relation USING btree (library_id, support_count, updated_at, relation_id);


--
-- Name: knowledge_relation_search_tsv_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_relation_search_tsv_gin ON public.knowledge_relation USING gin (search_tsv) WITH (fastupdate=off);


--
-- Name: knowledge_relation_subject_object_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_relation_subject_object_index ON public.knowledge_relation USING btree (library_id, subject_entity_id, object_entity_id, predicate);


--
-- Name: knowledge_retrieval_trace_bundle_created_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_retrieval_trace_bundle_created_index ON public.knowledge_retrieval_trace USING btree (bundle_id, created_at, trace_id);


--
-- Name: knowledge_retrieval_trace_query_execution_created_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_retrieval_trace_query_execution_created_index ON public.knowledge_retrieval_trace USING btree (query_execution_id, created_at, trace_id) WHERE (query_execution_id IS NOT NULL);


--
-- Name: knowledge_revision_document_revision_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_revision_document_revision_index ON public.knowledge_revision USING btree (document_id, revision_number, revision_id);


--
-- Name: knowledge_revision_library_vector_state_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_revision_library_vector_state_index ON public.knowledge_revision USING btree (library_id, vector_state, revision_id);


--
-- Name: knowledge_revision_revision_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_revision_revision_id_index ON public.knowledge_revision USING btree (revision_id);


--
-- Name: knowledge_structured_block_block_id_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_structured_block_block_id_index ON public.knowledge_structured_block USING btree (block_id);


--
-- Name: knowledge_structured_block_revision_kind_occurred_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_structured_block_revision_kind_occurred_index ON public.knowledge_structured_block USING btree (revision_id, block_kind, occurred_at);


--
-- Name: knowledge_structured_block_revision_ordinal_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_structured_block_revision_ordinal_index ON public.knowledge_structured_block USING btree (revision_id, ordinal);


--
-- Name: knowledge_structured_block_search_tsv_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_structured_block_search_tsv_gin ON public.knowledge_structured_block USING gin (search_tsv) WITH (fastupdate=off);


--
-- Name: knowledge_structured_revision_revision_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_structured_revision_revision_index ON public.knowledge_structured_revision USING btree (revision_id, document_id);


--
-- Name: knowledge_technical_fact_literal_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_technical_fact_literal_index ON public.knowledge_technical_fact USING btree (canonical_value_exact, fact_kind);


--
-- Name: knowledge_technical_fact_revision_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_technical_fact_revision_index ON public.knowledge_technical_fact USING btree (revision_id, fact_kind);


--
-- Name: knowledge_technical_fact_search_tsv_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_technical_fact_search_tsv_gin ON public.knowledge_technical_fact USING gin (search_tsv) WITH (fastupdate=off);


--
-- Name: knowledge_technical_fact_support_chunk_ids_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_technical_fact_support_chunk_ids_gin ON public.knowledge_technical_fact USING gin (support_chunk_ids);


--
-- Name: knowledge_vector_manifest_exact_profile_dimension_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX knowledge_vector_manifest_exact_profile_dimension_key ON public.knowledge_vector_relation_manifest USING btree (library_id, vector_kind, embedding_model_key) WHERE ((embedding_model_key ~~ 'embedding-profile:v1:%'::text) OR (embedding_model_key ~~ 'embedding-rebuild:v1:%'::text));


--
-- Name: knowledge_vector_relation_manifest_default_index; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX knowledge_vector_relation_manifest_default_index ON public.knowledge_vector_relation_manifest USING btree (library_id, dim, is_default, promoted);


--
-- Name: maintenance_job_run_dead_letter_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX maintenance_job_run_dead_letter_idx ON public.maintenance_job_run USING btree (class, updated_at) WHERE (state = 'dead_letter'::public.maintenance_run_state);


--
-- Name: maintenance_job_run_due_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX maintenance_job_run_due_idx ON public.maintenance_job_run USING btree (class, next_due_at) WHERE (state = 'pending'::public.maintenance_run_state);


--
-- Name: maintenance_job_run_heartbeat_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX maintenance_job_run_heartbeat_idx ON public.maintenance_job_run USING btree (heartbeat_at) WHERE (state = 'leased'::public.maintenance_run_state);


--
-- Name: maintenance_job_run_scope_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX maintenance_job_run_scope_key ON public.maintenance_job_run USING btree (class, scope_kind, COALESCE(scope_id, '00000000-0000-0000-0000-000000000000'::uuid));


--
-- Name: runtime_execution_parent_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX runtime_execution_parent_idx ON public.runtime_execution USING btree (parent_execution_id) WHERE (parent_execution_id IS NOT NULL);


--
-- Name: runtime_graph_community_lib_ver_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX runtime_graph_community_lib_ver_idx ON public.runtime_graph_community USING btree (library_id, projection_version);


--
-- Name: runtime_graph_community_members_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX runtime_graph_community_members_idx ON public.runtime_graph_community USING gin (member_node_ids);


--
-- Name: content_document_head content_document_head_mutation_lineage; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER content_document_head_mutation_lineage AFTER INSERT OR UPDATE ON public.content_document_head DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_lineage();


--
-- Name: content_document content_document_scope_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER content_document_scope_immutable BEFORE UPDATE ON public.content_document FOR EACH ROW EXECUTE FUNCTION public.preserve_content_lineage_owner();


--
-- Name: content_mutation_item content_mutation_item_lineage; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER content_mutation_item_lineage AFTER INSERT OR UPDATE ON public.content_mutation_item DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_lineage();


--
-- Name: content_mutation_item content_mutation_item_owner_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER content_mutation_item_owner_immutable BEFORE UPDATE ON public.content_mutation_item FOR EACH ROW EXECUTE FUNCTION public.preserve_content_lineage_owner();


--
-- Name: content_mutation_predecessor_audit content_mutation_predecessor_audit_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER content_mutation_predecessor_audit_immutable BEFORE DELETE OR UPDATE ON public.content_mutation_predecessor_audit FOR EACH ROW EXECUTE FUNCTION public.preserve_content_mutation_reference();


--
-- Name: content_mutation_predecessor_audit content_mutation_predecessor_audit_representation; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER content_mutation_predecessor_audit_representation AFTER INSERT OR DELETE OR UPDATE ON public.content_mutation_predecessor_audit DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_representation();


--
-- Name: content_mutation_reference content_mutation_reference_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER content_mutation_reference_immutable BEFORE UPDATE ON public.content_mutation_reference FOR EACH ROW EXECUTE FUNCTION public.preserve_content_mutation_reference();


--
-- Name: content_mutation_reference content_mutation_reference_representation; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER content_mutation_reference_representation AFTER INSERT ON public.content_mutation_reference DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_representation();


--
-- Name: content_mutation content_mutation_representation; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER content_mutation_representation AFTER INSERT OR DELETE OR UPDATE ON public.content_mutation DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_representation();


--
-- Name: content_mutation content_mutation_request_identity_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER content_mutation_request_identity_immutable BEFORE INSERT OR UPDATE ON public.content_mutation FOR EACH ROW EXECUTE FUNCTION public.preserve_content_request_identity();


--
-- Name: ingest_job ingest_job_mutation_lineage; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER ingest_job_mutation_lineage AFTER INSERT OR UPDATE ON public.ingest_job DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_lineage();


--
-- Name: ai_account trg_ai_account_delete_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_account_delete_source_generations AFTER DELETE ON public.ai_account REFERENCING OLD TABLE AS old_accounts FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_account_delete_source_generations();


--
-- Name: ai_account trg_ai_account_generation_serializer; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_account_generation_serializer BEFORE INSERT OR DELETE OR UPDATE ON public.ai_account FOR EACH STATEMENT EXECUTE FUNCTION public.serialize_ai_config_generation_writes();


--
-- Name: ai_account trg_ai_account_scope_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_account_scope_immutable BEFORE UPDATE OF scope_kind, workspace_id, library_id ON public.ai_account FOR EACH ROW EXECUTE FUNCTION public.prevent_ai_account_scope_change();


--
-- Name: ai_account trg_ai_account_update_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_account_update_source_generations AFTER UPDATE ON public.ai_account REFERENCING OLD TABLE AS old_accounts NEW TABLE AS new_accounts FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_account_update_source_generations();


--
-- Name: ai_binding trg_ai_binding_account_scope; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER trg_ai_binding_account_scope AFTER INSERT OR UPDATE OF scope_kind, workspace_id, library_id, account_id ON public.ai_binding DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION public.enforce_ai_binding_account_scope();


--
-- Name: ai_binding trg_ai_binding_delete_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_binding_delete_source_generations AFTER DELETE ON public.ai_binding REFERENCING OLD TABLE AS old_bindings FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_binding_delete_source_generations();


--
-- Name: ai_binding trg_ai_binding_generation_serializer; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_binding_generation_serializer BEFORE INSERT OR DELETE OR UPDATE ON public.ai_binding FOR EACH STATEMENT EXECUTE FUNCTION public.serialize_ai_config_generation_writes();


--
-- Name: ai_binding trg_ai_binding_insert_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_binding_insert_source_generations AFTER INSERT ON public.ai_binding REFERENCING NEW TABLE AS new_bindings FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_binding_insert_source_generations();


--
-- Name: ai_binding trg_ai_binding_update_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_binding_update_source_generations AFTER UPDATE ON public.ai_binding REFERENCING OLD TABLE AS old_bindings NEW TABLE AS new_bindings FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_binding_update_source_generations();


--
-- Name: ai_model_catalog trg_ai_model_delete_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_model_delete_source_generations AFTER DELETE ON public.ai_model_catalog REFERENCING OLD TABLE AS old_models FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_model_delete_source_generations();


--
-- Name: ai_model_catalog trg_ai_model_generation_serializer; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_model_generation_serializer BEFORE INSERT OR DELETE OR UPDATE ON public.ai_model_catalog FOR EACH STATEMENT EXECUTE FUNCTION public.serialize_ai_config_generation_writes();


--
-- Name: ai_model_catalog trg_ai_model_update_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_model_update_source_generations AFTER UPDATE ON public.ai_model_catalog REFERENCING OLD TABLE AS old_models NEW TABLE AS new_models FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_model_update_source_generations();


--
-- Name: ai_provider_catalog trg_ai_provider_delete_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_provider_delete_source_generations AFTER DELETE ON public.ai_provider_catalog REFERENCING OLD TABLE AS old_providers FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_provider_delete_source_generations();


--
-- Name: ai_provider_catalog trg_ai_provider_generation_serializer; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_provider_generation_serializer BEFORE INSERT OR DELETE OR UPDATE ON public.ai_provider_catalog FOR EACH STATEMENT EXECUTE FUNCTION public.serialize_ai_config_generation_writes();


--
-- Name: ai_provider_catalog trg_ai_provider_update_source_generations; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ai_provider_update_source_generations AFTER UPDATE ON public.ai_provider_catalog REFERENCING OLD TABLE AS old_providers NEW TABLE AS new_providers FOR EACH STATEMENT EXECUTE FUNCTION public.invalidate_ai_provider_update_source_generations();


--
-- Name: catalog_library trg_catalog_library_ai_generation_serializer; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_catalog_library_ai_generation_serializer BEFORE DELETE ON public.catalog_library FOR EACH STATEMENT EXECUTE FUNCTION public.serialize_ai_config_generation_writes();


--
-- Name: catalog_library trg_catalog_library_carry_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_catalog_library_carry_readable_projection_version BEFORE UPDATE OF source_truth_version ON public.catalog_library FOR EACH ROW EXECUTE FUNCTION public.carry_readable_projection_version();


--
-- Name: catalog_library trg_catalog_library_initialize_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_catalog_library_initialize_readable_projection_version BEFORE INSERT ON public.catalog_library FOR EACH ROW EXECUTE FUNCTION public.initialize_readable_projection_version();


--
-- Name: catalog_library trg_catalog_library_provisioning_owner_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_catalog_library_provisioning_owner_immutable BEFORE UPDATE OF provisioned_by_principal_id ON public.catalog_library FOR EACH ROW EXECUTE FUNCTION public.prevent_catalog_provisioning_owner_change();


--
-- Name: catalog_workspace trg_catalog_workspace_ai_generation_serializer; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_catalog_workspace_ai_generation_serializer BEFORE DELETE ON public.catalog_workspace FOR EACH STATEMENT EXECUTE FUNCTION public.serialize_ai_config_generation_writes();


--
-- Name: catalog_workspace trg_catalog_workspace_provisioning_owner_immutable; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_catalog_workspace_provisioning_owner_immutable BEFORE UPDATE OF provisioned_by_principal_id ON public.catalog_workspace FOR EACH ROW EXECUTE FUNCTION public.prevent_catalog_provisioning_owner_change();


--
-- Name: content_document_head trg_content_document_head_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_content_document_head_readable_projection_version AFTER INSERT OR DELETE OR UPDATE ON public.content_document_head FOR EACH ROW EXECUTE FUNCTION public.maintain_projection_version_from_content_head();


--
-- Name: content_document trg_content_document_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_content_document_readable_projection_version AFTER INSERT OR DELETE OR UPDATE ON public.content_document FOR EACH ROW EXECUTE FUNCTION public.maintain_projection_version_from_content_document();


--
-- Name: content_revision trg_content_revision_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_content_revision_readable_projection_version AFTER INSERT OR DELETE OR UPDATE ON public.content_revision FOR EACH ROW EXECUTE FUNCTION public.maintain_projection_version_from_content_revision();


--
-- Name: ingest_job trg_ingest_job_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_ingest_job_readable_projection_version AFTER INSERT OR DELETE OR UPDATE ON public.ingest_job FOR EACH ROW EXECUTE FUNCTION public.maintain_projection_version_from_ingest_job();


--
-- Name: knowledge_document trg_knowledge_document_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_knowledge_document_readable_projection_version AFTER INSERT OR DELETE OR UPDATE ON public.knowledge_document FOR EACH ROW EXECUTE FUNCTION public.maintain_projection_version_from_knowledge_document();


--
-- Name: knowledge_revision trg_knowledge_revision_readable_projection_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_knowledge_revision_readable_projection_version AFTER INSERT OR DELETE OR UPDATE ON public.knowledge_revision FOR EACH ROW EXECUTE FUNCTION public.maintain_projection_version_from_knowledge_revision();


--
-- Name: webhook_delivery_attempt trg_redact_webhook_delivery_diagnostics; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_redact_webhook_delivery_diagnostics BEFORE INSERT OR UPDATE OF response_body_excerpt, error_code, error_message, delivery_state ON public.webhook_delivery_attempt FOR EACH ROW EXECUTE FUNCTION public.redact_webhook_delivery_diagnostics();


--
-- Name: webhook_delivery_attempt trg_webhook_delivery_attempt_subscription_scope; Type: TRIGGER; Schema: public; Owner: -
--

CREATE CONSTRAINT TRIGGER trg_webhook_delivery_attempt_subscription_scope AFTER INSERT OR UPDATE OF subscription_id, workspace_id, library_id, job_id ON public.webhook_delivery_attempt DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION public.enforce_webhook_delivery_attempt_subscription_scope();


--
-- Name: webhook_delivery_attempt trg_webhook_delivery_event_identity; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_webhook_delivery_event_identity AFTER INSERT ON public.webhook_delivery_attempt FOR EACH ROW EXECUTE FUNCTION public.enforce_webhook_delivery_event_identity();


--
-- Name: webhook_subscription trg_webhook_subscription_workspace_quota; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_webhook_subscription_workspace_quota AFTER INSERT OR UPDATE OF workspace_id, active ON public.webhook_subscription FOR EACH ROW EXECUTE FUNCTION public.enforce_webhook_subscription_workspace_quota();


--
-- Name: ai_binding ai_binding_account_provider_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding
    ADD CONSTRAINT ai_binding_account_provider_fkey FOREIGN KEY (account_id, provider_catalog_id) REFERENCES public.ai_account(id, provider_catalog_id) ON DELETE RESTRICT;


--
-- Name: ai_binding ai_binding_assignment_library_scope_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding
    ADD CONSTRAINT ai_binding_assignment_library_scope_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: ai_binding ai_binding_assignment_updated_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding
    ADD CONSTRAINT ai_binding_assignment_updated_by_principal_id_fkey FOREIGN KEY (updated_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: ai_binding ai_binding_model_provider_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding
    ADD CONSTRAINT ai_binding_model_provider_fkey FOREIGN KEY (model_catalog_id, provider_catalog_id) REFERENCES public.ai_model_catalog(id, provider_catalog_id) ON DELETE RESTRICT;


--
-- Name: ai_binding ai_binding_model_purpose_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding
    ADD CONSTRAINT ai_binding_model_purpose_fkey FOREIGN KEY (model_catalog_id, binding_purpose) REFERENCES public.ai_model_binding_purpose(model_catalog_id, binding_purpose) ON DELETE RESTRICT;


--
-- Name: ai_binding_validation ai_binding_validation_binding_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_binding_validation
    ADD CONSTRAINT ai_binding_validation_binding_id_fkey FOREIGN KEY (binding_id) REFERENCES public.ai_binding(id) ON DELETE CASCADE;


--
-- Name: ai_model_binding_purpose ai_model_binding_purpose_model_catalog_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_model_binding_purpose
    ADD CONSTRAINT ai_model_binding_purpose_model_catalog_id_fkey FOREIGN KEY (model_catalog_id) REFERENCES public.ai_model_catalog(id) ON DELETE CASCADE;


--
-- Name: ai_model_catalog ai_model_catalog_provider_catalog_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_model_catalog
    ADD CONSTRAINT ai_model_catalog_provider_catalog_id_fkey FOREIGN KEY (provider_catalog_id) REFERENCES public.ai_provider_catalog(id) ON DELETE RESTRICT;


--
-- Name: ai_account ai_provider_credential_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_account
    ADD CONSTRAINT ai_provider_credential_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: ai_account ai_provider_credential_library_scope_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_account
    ADD CONSTRAINT ai_provider_credential_library_scope_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: ai_account ai_provider_credential_provider_catalog_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_account
    ADD CONSTRAINT ai_provider_credential_provider_catalog_id_fkey FOREIGN KEY (provider_catalog_id) REFERENCES public.ai_provider_catalog(id) ON DELETE RESTRICT;


--
-- Name: ai_account ai_provider_credential_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ai_account
    ADD CONSTRAINT ai_provider_credential_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- Name: audit_event audit_event_actor_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_event
    ADD CONSTRAINT audit_event_actor_principal_id_fkey FOREIGN KEY (actor_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: audit_event_subject audit_event_subject_audit_event_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_event_subject
    ADD CONSTRAINT audit_event_subject_audit_event_id_fkey FOREIGN KEY (audit_event_id) REFERENCES public.audit_event(id) ON DELETE CASCADE;


--
-- Name: billing_charge billing_charge_usage_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_charge
    ADD CONSTRAINT billing_charge_usage_id_fkey FOREIGN KEY (usage_id) REFERENCES public.billing_usage(id) ON DELETE CASCADE;


--
-- Name: billing_execution_cost billing_execution_cost_library_workspace_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_execution_cost
    ADD CONSTRAINT billing_execution_cost_library_workspace_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: billing_execution_cost_rollup_state billing_execution_cost_rollup_state_library_scope_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_execution_cost_rollup_state
    ADD CONSTRAINT billing_execution_cost_rollup_state_library_scope_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: billing_provider_call billing_provider_call_binding_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_provider_call
    ADD CONSTRAINT billing_provider_call_binding_id_fkey FOREIGN KEY (binding_id) REFERENCES public.ai_binding(id) ON DELETE SET NULL;


--
-- Name: billing_provider_call billing_provider_call_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_provider_call
    ADD CONSTRAINT billing_provider_call_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: billing_provider_call billing_provider_call_model_catalog_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_provider_call
    ADD CONSTRAINT billing_provider_call_model_catalog_id_fkey FOREIGN KEY (model_catalog_id) REFERENCES public.ai_model_catalog(id) ON DELETE RESTRICT;


--
-- Name: billing_provider_call billing_provider_call_provider_catalog_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_provider_call
    ADD CONSTRAINT billing_provider_call_provider_catalog_id_fkey FOREIGN KEY (provider_catalog_id) REFERENCES public.ai_provider_catalog(id) ON DELETE RESTRICT;


--
-- Name: billing_provider_call billing_provider_call_runtime_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_provider_call
    ADD CONSTRAINT billing_provider_call_runtime_execution_id_fkey FOREIGN KEY (runtime_execution_id) REFERENCES public.runtime_execution(id) ON DELETE SET NULL;


--
-- Name: billing_usage billing_usage_provider_call_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.billing_usage
    ADD CONSTRAINT billing_usage_provider_call_id_fkey FOREIGN KEY (provider_call_id) REFERENCES public.billing_provider_call(id) ON DELETE CASCADE;


--
-- Name: catalog_library_connector catalog_library_connector_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library_connector
    ADD CONSTRAINT catalog_library_connector_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: catalog_library_connector catalog_library_connector_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library_connector
    ADD CONSTRAINT catalog_library_connector_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: catalog_library catalog_library_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library
    ADD CONSTRAINT catalog_library_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: catalog_library catalog_library_provisioned_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library
    ADD CONSTRAINT catalog_library_provisioned_by_principal_id_fkey FOREIGN KEY (provisioned_by_principal_id) REFERENCES public.iam_principal(id);


--
-- Name: catalog_library catalog_library_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_library
    ADD CONSTRAINT catalog_library_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- Name: catalog_workspace catalog_workspace_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_workspace
    ADD CONSTRAINT catalog_workspace_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: catalog_workspace catalog_workspace_provisioned_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.catalog_workspace
    ADD CONSTRAINT catalog_workspace_provisioned_by_principal_id_fkey FOREIGN KEY (provisioned_by_principal_id) REFERENCES public.iam_principal(id);


--
-- Name: content_chunk content_chunk_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_chunk
    ADD CONSTRAINT content_chunk_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.content_revision(id) ON DELETE CASCADE;


--
-- Name: content_document content_document_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document
    ADD CONSTRAINT content_document_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: content_document_head content_document_head_active_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document_head
    ADD CONSTRAINT content_document_head_active_revision_id_fkey FOREIGN KEY (active_revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: content_document_head content_document_head_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document_head
    ADD CONSTRAINT content_document_head_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.content_document(id) ON DELETE CASCADE;


--
-- Name: content_document_head content_document_head_latest_mutation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document_head
    ADD CONSTRAINT content_document_head_latest_mutation_id_fkey FOREIGN KEY (latest_mutation_id) REFERENCES public.content_mutation_reference(id) ON DELETE SET NULL;


--
-- Name: content_document_head content_document_head_latest_successful_attempt_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document_head
    ADD CONSTRAINT content_document_head_latest_successful_attempt_id_fkey FOREIGN KEY (latest_successful_attempt_id) REFERENCES public.ingest_attempt(id) ON DELETE SET NULL;


--
-- Name: content_document_head content_document_head_readable_lineage_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--



--
-- Name: content_document_head content_document_head_readable_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document_head
    ADD CONSTRAINT content_document_head_readable_revision_id_fkey FOREIGN KEY (readable_revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: content_document_head content_document_head_visibility_intent_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document_head
    ADD CONSTRAINT content_document_head_visibility_intent_fkey FOREIGN KEY (visibility_intent_id, document_id) REFERENCES public.content_visibility_intent(mutation_item_id, document_id) DEFERRABLE INITIALLY DEFERRED;


--
-- Name: content_document content_document_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_document
    ADD CONSTRAINT content_document_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: content_mutation_item content_mutation_item_base_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_base_revision_id_fkey FOREIGN KEY (base_revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: content_mutation_item content_mutation_item_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.content_document(id) ON DELETE RESTRICT;


--
-- Name: content_mutation_item content_mutation_item_mutation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_mutation_id_fkey FOREIGN KEY (mutation_id) REFERENCES public.content_mutation_reference(id) ON DELETE CASCADE;


--
-- Name: content_mutation_item content_mutation_item_result_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_result_revision_id_fkey FOREIGN KEY (result_revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: content_mutation_item content_mutation_item_visibility_intent_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_item
    ADD CONSTRAINT content_mutation_item_visibility_intent_fkey FOREIGN KEY (visibility_intent_id, document_id) REFERENCES public.content_visibility_intent(mutation_item_id, document_id) DEFERRABLE INITIALLY DEFERRED;


--
-- Name: content_mutation_predecessor_audit content_mutation_predecessor_audit_reference_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_predecessor_audit
    ADD CONSTRAINT content_mutation_predecessor_audit_reference_fkey FOREIGN KEY (id, workspace_id, library_id) REFERENCES public.content_mutation_reference(id, workspace_id, library_id) ON DELETE CASCADE;


--
-- Name: content_mutation content_mutation_reference_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation
    ADD CONSTRAINT content_mutation_reference_fkey FOREIGN KEY (id, workspace_id, library_id) REFERENCES public.content_mutation_reference(id, workspace_id, library_id) ON DELETE CASCADE;


--
-- Name: content_mutation_reference content_mutation_reference_library_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation_reference
    ADD CONSTRAINT content_mutation_reference_library_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: content_mutation content_mutation_requested_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_mutation
    ADD CONSTRAINT content_mutation_requested_by_principal_id_fkey FOREIGN KEY (requested_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: content_revision content_revision_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision
    ADD CONSTRAINT content_revision_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: content_revision content_revision_document_id_workspace_id_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision
    ADD CONSTRAINT content_revision_document_id_workspace_id_library_id_fkey FOREIGN KEY (document_id, workspace_id, library_id) REFERENCES public.content_document(id, workspace_id, library_id) ON DELETE CASCADE;


--
-- Name: content_revision_ingest_unit content_revision_ingest_unit_attempt_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision_ingest_unit
    ADD CONSTRAINT content_revision_ingest_unit_attempt_id_fkey FOREIGN KEY (attempt_id) REFERENCES public.ingest_attempt(id) ON DELETE SET NULL;


--
-- Name: content_revision_ingest_unit content_revision_ingest_unit_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision_ingest_unit
    ADD CONSTRAINT content_revision_ingest_unit_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.content_revision(id) ON DELETE CASCADE;


--
-- Name: content_revision content_revision_parent_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_revision
    ADD CONSTRAINT content_revision_parent_revision_id_fkey FOREIGN KEY (parent_revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: content_visibility_intent content_visibility_intent_document_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_visibility_intent
    ADD CONSTRAINT content_visibility_intent_document_fkey FOREIGN KEY (document_id) REFERENCES public.content_document(id) ON DELETE CASCADE;


--
-- Name: content_visibility_intent content_visibility_intent_item_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_visibility_intent
    ADD CONSTRAINT content_visibility_intent_item_fkey FOREIGN KEY (mutation_item_id, document_id) REFERENCES public.content_mutation_item(id, document_id) ON DELETE CASCADE;


--
-- Name: content_web_discovered_page content_web_discovered_page_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_discovered_page
    ADD CONSTRAINT content_web_discovered_page_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.content_document(id) ON DELETE SET NULL;


--
-- Name: content_web_discovered_page content_web_discovered_page_mutation_item_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_discovered_page
    ADD CONSTRAINT content_web_discovered_page_mutation_item_id_fkey FOREIGN KEY (mutation_item_id) REFERENCES public.content_mutation_item(id) ON DELETE SET NULL;


--
-- Name: content_web_discovered_page content_web_discovered_page_referrer_candidate_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_discovered_page
    ADD CONSTRAINT content_web_discovered_page_referrer_candidate_id_fkey FOREIGN KEY (referrer_candidate_id) REFERENCES public.content_web_discovered_page(id) ON DELETE SET NULL;


--
-- Name: content_web_discovered_page content_web_discovered_page_result_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_discovered_page
    ADD CONSTRAINT content_web_discovered_page_result_revision_id_fkey FOREIGN KEY (result_revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: content_web_discovered_page content_web_discovered_page_run_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_discovered_page
    ADD CONSTRAINT content_web_discovered_page_run_id_fkey FOREIGN KEY (run_id) REFERENCES public.content_web_ingest_run(id) ON DELETE CASCADE;


--
-- Name: content_web_ingest_run content_web_ingest_run_async_operation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_async_operation_id_fkey FOREIGN KEY (async_operation_id) REFERENCES public.ops_async_operation(id) ON DELETE SET NULL;


--
-- Name: content_web_ingest_run content_web_ingest_run_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: content_web_ingest_run content_web_ingest_run_mutation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_mutation_id_fkey FOREIGN KEY (mutation_id, workspace_id, library_id) REFERENCES public.content_mutation_reference(id, workspace_id, library_id) ON DELETE CASCADE;


--
-- Name: content_web_ingest_run content_web_ingest_run_requested_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_requested_by_principal_id_fkey FOREIGN KEY (requested_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: content_web_ingest_run content_web_ingest_run_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.content_web_ingest_run
    ADD CONSTRAINT content_web_ingest_run_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- Name: entity_dedup_verification_cache entity_dedup_verification_cache_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.entity_dedup_verification_cache
    ADD CONSTRAINT entity_dedup_verification_cache_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: extract_chunk_result extract_chunk_result_attempt_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_chunk_result
    ADD CONSTRAINT extract_chunk_result_attempt_id_fkey FOREIGN KEY (attempt_id) REFERENCES public.ingest_attempt(id) ON DELETE CASCADE;


--
-- Name: extract_chunk_result extract_chunk_result_chunk_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_chunk_result
    ADD CONSTRAINT extract_chunk_result_chunk_id_fkey FOREIGN KEY (chunk_id) REFERENCES public.content_chunk(id) ON DELETE CASCADE;


--
-- Name: extract_chunk_result extract_chunk_result_provider_call_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_chunk_result
    ADD CONSTRAINT extract_chunk_result_provider_call_id_fkey FOREIGN KEY (provider_call_id) REFERENCES public.billing_provider_call(id) ON DELETE SET NULL;


--
-- Name: extract_edge_candidate extract_edge_candidate_chunk_result_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_edge_candidate
    ADD CONSTRAINT extract_edge_candidate_chunk_result_id_fkey FOREIGN KEY (chunk_result_id) REFERENCES public.extract_chunk_result(id) ON DELETE CASCADE;


--
-- Name: extract_node_candidate extract_node_candidate_chunk_result_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_node_candidate
    ADD CONSTRAINT extract_node_candidate_chunk_result_id_fkey FOREIGN KEY (chunk_result_id) REFERENCES public.extract_chunk_result(id) ON DELETE CASCADE;


--
-- Name: extract_resume_cursor extract_resume_cursor_attempt_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.extract_resume_cursor
    ADD CONSTRAINT extract_resume_cursor_attempt_id_fkey FOREIGN KEY (attempt_id) REFERENCES public.ingest_attempt(id) ON DELETE CASCADE;


--
-- Name: iam_api_token iam_api_token_issued_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_api_token
    ADD CONSTRAINT iam_api_token_issued_by_principal_id_fkey FOREIGN KEY (issued_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: iam_api_token iam_api_token_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_api_token
    ADD CONSTRAINT iam_api_token_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_principal(id) ON DELETE CASCADE;


--
-- Name: iam_api_token_secret iam_api_token_secret_token_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_api_token_secret
    ADD CONSTRAINT iam_api_token_secret_token_principal_id_fkey FOREIGN KEY (token_principal_id) REFERENCES public.iam_api_token(principal_id) ON DELETE CASCADE;


--
-- Name: iam_api_token iam_api_token_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_api_token
    ADD CONSTRAINT iam_api_token_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- Name: iam_grant iam_grant_granted_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_grant
    ADD CONSTRAINT iam_grant_granted_by_principal_id_fkey FOREIGN KEY (granted_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: iam_grant iam_grant_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_grant
    ADD CONSTRAINT iam_grant_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_principal(id) ON DELETE CASCADE;


--
-- Name: iam_principal iam_principal_parent_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_principal
    ADD CONSTRAINT iam_principal_parent_principal_id_fkey FOREIGN KEY (parent_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: iam_service_identity iam_service_identity_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: iam_service_identity_operation iam_service_identity_operation_actor_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_actor_principal_id_fkey FOREIGN KEY (actor_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: iam_service_identity_operation iam_service_identity_operation_identity_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_identity_fkey FOREIGN KEY (service_identity_principal_id, owner_auth_provider_kind, owner_external_subject) REFERENCES public.iam_service_identity(principal_id, owner_auth_provider_kind, owner_external_subject) ON DELETE CASCADE;


--
-- Name: iam_service_identity_operation iam_service_identity_operation_secret_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_secret_fkey FOREIGN KEY (service_identity_principal_id, credential_secret_version) REFERENCES public.iam_api_token_secret(token_principal_id, secret_version) ON DELETE CASCADE;


--
-- Name: iam_service_identity iam_service_identity_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_api_token(principal_id) ON DELETE CASCADE;


--
-- Name: iam_session iam_session_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_session
    ADD CONSTRAINT iam_session_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_principal(id) ON DELETE CASCADE;


--
-- Name: iam_user iam_user_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_user
    ADD CONSTRAINT iam_user_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_principal(id) ON DELETE CASCADE;


--
-- Name: iam_workspace_membership iam_workspace_membership_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_workspace_membership
    ADD CONSTRAINT iam_workspace_membership_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_principal(id) ON DELETE CASCADE;


--
-- Name: iam_workspace_membership iam_workspace_membership_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.iam_workspace_membership
    ADD CONSTRAINT iam_workspace_membership_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- Name: ingest_attempt ingest_attempt_job_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_attempt
    ADD CONSTRAINT ingest_attempt_job_id_fkey FOREIGN KEY (job_id) REFERENCES public.ingest_job(id) ON DELETE CASCADE;


--
-- Name: ingest_attempt ingest_attempt_worker_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_attempt
    ADD CONSTRAINT ingest_attempt_worker_principal_id_fkey FOREIGN KEY (worker_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: ingest_job ingest_job_async_operation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_job
    ADD CONSTRAINT ingest_job_async_operation_id_fkey FOREIGN KEY (async_operation_id) REFERENCES public.ops_async_operation(id) ON DELETE SET NULL;


--
-- Name: ingest_job ingest_job_connector_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_job
    ADD CONSTRAINT ingest_job_connector_id_fkey FOREIGN KEY (connector_id) REFERENCES public.catalog_library_connector(id) ON DELETE SET NULL;


--
-- Name: ingest_job ingest_job_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_job
    ADD CONSTRAINT ingest_job_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: ingest_job ingest_job_mutation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_job
    ADD CONSTRAINT ingest_job_mutation_id_fkey FOREIGN KEY (mutation_id, workspace_id, library_id) REFERENCES public.content_mutation_reference(id, workspace_id, library_id) ON DELETE SET NULL (mutation_id);


--
-- Name: ingest_job ingest_job_mutation_item_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_job
    ADD CONSTRAINT ingest_job_mutation_item_id_fkey FOREIGN KEY (mutation_item_id) REFERENCES public.content_mutation_item(id) ON DELETE SET NULL NOT VALID;


--
-- Name: ingest_stage_event ingest_stage_event_attempt_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_stage_event
    ADD CONSTRAINT ingest_stage_event_attempt_id_fkey FOREIGN KEY (attempt_id) REFERENCES public.ingest_attempt(id) ON DELETE CASCADE;


--
-- Name: ingest_stage_provider_call ingest_stage_provider_call_stage_event_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ingest_stage_provider_call
    ADD CONSTRAINT ingest_stage_provider_call_stage_event_id_fkey FOREIGN KEY (stage_event_id) REFERENCES public.ingest_stage_event(id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_chunk knowledge_bundle_chunk_bundle_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_chunk
    ADD CONSTRAINT knowledge_bundle_chunk_bundle_id_fkey FOREIGN KEY (bundle_id) REFERENCES public.knowledge_context_bundle(bundle_id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_chunk knowledge_bundle_chunk_chunk_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_chunk
    ADD CONSTRAINT knowledge_bundle_chunk_chunk_id_fkey FOREIGN KEY (chunk_id) REFERENCES public.knowledge_chunk(chunk_id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_entity knowledge_bundle_entity_bundle_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_entity
    ADD CONSTRAINT knowledge_bundle_entity_bundle_id_fkey FOREIGN KEY (bundle_id) REFERENCES public.knowledge_context_bundle(bundle_id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_entity knowledge_bundle_entity_entity_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_entity
    ADD CONSTRAINT knowledge_bundle_entity_entity_id_fkey FOREIGN KEY (entity_id) REFERENCES public.runtime_graph_node(id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_evidence knowledge_bundle_evidence_bundle_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_evidence
    ADD CONSTRAINT knowledge_bundle_evidence_bundle_id_fkey FOREIGN KEY (bundle_id) REFERENCES public.knowledge_context_bundle(bundle_id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_evidence knowledge_bundle_evidence_evidence_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_evidence
    ADD CONSTRAINT knowledge_bundle_evidence_evidence_id_fkey FOREIGN KEY (evidence_id) REFERENCES public.runtime_graph_evidence(id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_relation knowledge_bundle_relation_bundle_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_relation
    ADD CONSTRAINT knowledge_bundle_relation_bundle_id_fkey FOREIGN KEY (bundle_id) REFERENCES public.knowledge_context_bundle(bundle_id) ON DELETE CASCADE;


--
-- Name: knowledge_bundle_relation knowledge_bundle_relation_relation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_bundle_relation
    ADD CONSTRAINT knowledge_bundle_relation_relation_id_fkey FOREIGN KEY (relation_id) REFERENCES public.runtime_graph_edge(id) ON DELETE CASCADE;


--
-- Name: knowledge_chunk knowledge_chunk_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_chunk
    ADD CONSTRAINT knowledge_chunk_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.knowledge_document(document_id) ON DELETE CASCADE;


--
-- Name: knowledge_chunk_entity_mention knowledge_chunk_entity_mention_from_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_chunk_entity_mention
    ADD CONSTRAINT knowledge_chunk_entity_mention_from_id_fkey FOREIGN KEY (from_id) REFERENCES public.knowledge_chunk(chunk_id) ON DELETE CASCADE;


--
-- Name: knowledge_chunk_entity_mention knowledge_chunk_entity_mention_to_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_chunk_entity_mention
    ADD CONSTRAINT knowledge_chunk_entity_mention_to_id_fkey FOREIGN KEY (to_id) REFERENCES public.knowledge_entity(entity_id) ON DELETE CASCADE;


--
-- Name: knowledge_chunk knowledge_chunk_primary_block_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_chunk
    ADD CONSTRAINT knowledge_chunk_primary_block_id_fkey FOREIGN KEY (primary_block_id) REFERENCES public.knowledge_structured_block(block_id) ON DELETE SET NULL;


--
-- Name: knowledge_chunk knowledge_chunk_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_chunk
    ADD CONSTRAINT knowledge_chunk_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE CASCADE;


--
-- Name: knowledge_context_bundle knowledge_context_bundle_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_context_bundle
    ADD CONSTRAINT knowledge_context_bundle_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: knowledge_document knowledge_document_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_document
    ADD CONSTRAINT knowledge_document_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: knowledge_entity_candidate knowledge_entity_candidate_chunk_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_entity_candidate
    ADD CONSTRAINT knowledge_entity_candidate_chunk_id_fkey FOREIGN KEY (chunk_id) REFERENCES public.knowledge_chunk(chunk_id) ON DELETE SET NULL;


--
-- Name: knowledge_entity_candidate knowledge_entity_candidate_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_entity_candidate
    ADD CONSTRAINT knowledge_entity_candidate_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE CASCADE;


--
-- Name: knowledge_entity knowledge_entity_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_entity
    ADD CONSTRAINT knowledge_entity_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: knowledge_evidence knowledge_evidence_block_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence
    ADD CONSTRAINT knowledge_evidence_block_id_fkey FOREIGN KEY (block_id) REFERENCES public.knowledge_structured_block(block_id) ON DELETE SET NULL;


--
-- Name: knowledge_evidence knowledge_evidence_chunk_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence
    ADD CONSTRAINT knowledge_evidence_chunk_id_fkey FOREIGN KEY (chunk_id) REFERENCES public.knowledge_chunk(chunk_id) ON DELETE SET NULL;


--
-- Name: knowledge_evidence knowledge_evidence_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence
    ADD CONSTRAINT knowledge_evidence_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.knowledge_document(document_id) ON DELETE CASCADE;


--
-- Name: knowledge_evidence_entity_support knowledge_evidence_entity_support_from_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence_entity_support
    ADD CONSTRAINT knowledge_evidence_entity_support_from_id_fkey FOREIGN KEY (from_id) REFERENCES public.knowledge_evidence(evidence_id) ON DELETE CASCADE;


--
-- Name: knowledge_evidence_entity_support knowledge_evidence_entity_support_to_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence_entity_support
    ADD CONSTRAINT knowledge_evidence_entity_support_to_id_fkey FOREIGN KEY (to_id) REFERENCES public.knowledge_entity(entity_id) ON DELETE CASCADE;


--
-- Name: knowledge_evidence knowledge_evidence_fact_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence
    ADD CONSTRAINT knowledge_evidence_fact_id_fkey FOREIGN KEY (fact_id) REFERENCES public.knowledge_technical_fact(fact_id) ON DELETE SET NULL;


--
-- Name: knowledge_evidence_relation_support knowledge_evidence_relation_support_from_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence_relation_support
    ADD CONSTRAINT knowledge_evidence_relation_support_from_id_fkey FOREIGN KEY (from_id) REFERENCES public.knowledge_evidence(evidence_id) ON DELETE CASCADE;


--
-- Name: knowledge_evidence_relation_support knowledge_evidence_relation_support_to_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence_relation_support
    ADD CONSTRAINT knowledge_evidence_relation_support_to_id_fkey FOREIGN KEY (to_id) REFERENCES public.knowledge_relation(relation_id) ON DELETE CASCADE;


--
-- Name: knowledge_evidence knowledge_evidence_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_evidence
    ADD CONSTRAINT knowledge_evidence_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE CASCADE;


--
-- Name: knowledge_relation_candidate knowledge_relation_candidate_chunk_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_relation_candidate
    ADD CONSTRAINT knowledge_relation_candidate_chunk_id_fkey FOREIGN KEY (chunk_id) REFERENCES public.knowledge_chunk(chunk_id) ON DELETE SET NULL;


--
-- Name: knowledge_relation_candidate knowledge_relation_candidate_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_relation_candidate
    ADD CONSTRAINT knowledge_relation_candidate_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE CASCADE;


--
-- Name: knowledge_relation knowledge_relation_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_relation
    ADD CONSTRAINT knowledge_relation_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: knowledge_relation knowledge_relation_object_entity_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_relation
    ADD CONSTRAINT knowledge_relation_object_entity_id_fkey FOREIGN KEY (object_entity_id) REFERENCES public.knowledge_entity(entity_id) ON DELETE SET NULL;


--
-- Name: knowledge_relation knowledge_relation_subject_entity_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_relation
    ADD CONSTRAINT knowledge_relation_subject_entity_id_fkey FOREIGN KEY (subject_entity_id) REFERENCES public.knowledge_entity(entity_id) ON DELETE SET NULL;


--
-- Name: knowledge_retrieval_trace knowledge_retrieval_trace_bundle_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_retrieval_trace
    ADD CONSTRAINT knowledge_retrieval_trace_bundle_id_fkey FOREIGN KEY (bundle_id) REFERENCES public.knowledge_context_bundle(bundle_id) ON DELETE CASCADE;


--
-- Name: knowledge_revision knowledge_revision_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_revision
    ADD CONSTRAINT knowledge_revision_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.knowledge_document(document_id) ON DELETE CASCADE;


--
-- Name: knowledge_revision knowledge_revision_superseded_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_revision
    ADD CONSTRAINT knowledge_revision_superseded_by_fkey FOREIGN KEY (superseded_by_revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE SET NULL;


--
-- Name: knowledge_structured_block knowledge_structured_block_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_structured_block
    ADD CONSTRAINT knowledge_structured_block_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.knowledge_document(document_id) ON DELETE CASCADE;


--
-- Name: knowledge_structured_block knowledge_structured_block_parent_block_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_structured_block
    ADD CONSTRAINT knowledge_structured_block_parent_block_id_fkey FOREIGN KEY (parent_block_id) REFERENCES public.knowledge_structured_block(block_id) ON DELETE SET NULL;


--
-- Name: knowledge_structured_block knowledge_structured_block_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_structured_block
    ADD CONSTRAINT knowledge_structured_block_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE CASCADE;


--
-- Name: knowledge_structured_revision knowledge_structured_revision_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_structured_revision
    ADD CONSTRAINT knowledge_structured_revision_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.knowledge_document(document_id) ON DELETE CASCADE;


--
-- Name: knowledge_structured_revision knowledge_structured_revision_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_structured_revision
    ADD CONSTRAINT knowledge_structured_revision_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE CASCADE;


--
-- Name: knowledge_technical_fact knowledge_technical_fact_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_technical_fact
    ADD CONSTRAINT knowledge_technical_fact_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.knowledge_document(document_id) ON DELETE CASCADE;


--
-- Name: knowledge_technical_fact knowledge_technical_fact_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_technical_fact
    ADD CONSTRAINT knowledge_technical_fact_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.knowledge_revision(revision_id) ON DELETE CASCADE;


--
-- Name: knowledge_vector_relation_manifest knowledge_vector_relation_manifest_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.knowledge_vector_relation_manifest
    ADD CONSTRAINT knowledge_vector_relation_manifest_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: ops_async_operation ops_async_operation_library_workspace_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_async_operation
    ADD CONSTRAINT ops_async_operation_library_workspace_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE SET NULL (library_id);


--
-- Name: ops_async_operation ops_async_operation_parent_async_operation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_async_operation
    ADD CONSTRAINT ops_async_operation_parent_async_operation_id_fkey FOREIGN KEY (parent_async_operation_id) REFERENCES public.ops_async_operation(id) ON DELETE SET NULL;


--
-- Name: ops_async_operation ops_async_operation_requested_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_async_operation
    ADD CONSTRAINT ops_async_operation_requested_by_principal_id_fkey FOREIGN KEY (requested_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: ops_library_state ops_library_state_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_library_state
    ADD CONSTRAINT ops_library_state_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: ops_library_warning ops_library_warning_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_library_warning
    ADD CONSTRAINT ops_library_warning_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: ops_library_warning ops_library_warning_source_operation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ops_library_warning
    ADD CONSTRAINT ops_library_warning_source_operation_id_fkey FOREIGN KEY (source_operation_id) REFERENCES public.ops_async_operation(id) ON DELETE SET NULL;


--
-- Name: query_chunk_reference query_chunk_reference_chunk_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_chunk_reference
    ADD CONSTRAINT query_chunk_reference_chunk_id_fkey FOREIGN KEY (chunk_id) REFERENCES public.content_chunk(id) ON DELETE CASCADE;


--
-- Name: query_chunk_reference query_chunk_reference_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_chunk_reference
    ADD CONSTRAINT query_chunk_reference_execution_id_fkey FOREIGN KEY (execution_id) REFERENCES public.query_execution(id) ON DELETE CASCADE;


--
-- Name: query_conversation query_conversation_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_conversation
    ADD CONSTRAINT query_conversation_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: query_conversation query_conversation_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_conversation
    ADD CONSTRAINT query_conversation_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: query_execution query_execution_binding_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution
    ADD CONSTRAINT query_execution_binding_id_fkey FOREIGN KEY (binding_id) REFERENCES public.ai_binding(id) ON DELETE SET NULL;


--
-- Name: query_execution query_execution_conversation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution
    ADD CONSTRAINT query_execution_conversation_id_fkey FOREIGN KEY (conversation_id) REFERENCES public.query_conversation(id) ON DELETE CASCADE;


--
-- Name: query_execution query_execution_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution
    ADD CONSTRAINT query_execution_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: query_execution_replay query_execution_replay_conversation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution_replay
    ADD CONSTRAINT query_execution_replay_conversation_id_fkey FOREIGN KEY (conversation_id) REFERENCES public.query_conversation(id) ON DELETE CASCADE;


--
-- Name: query_execution_replay query_execution_replay_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution_replay
    ADD CONSTRAINT query_execution_replay_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: query_execution_replay query_execution_replay_request_turn_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution_replay
    ADD CONSTRAINT query_execution_replay_request_turn_id_fkey FOREIGN KEY (request_turn_id) REFERENCES public.query_turn(id) ON DELETE CASCADE;


--
-- Name: query_execution_replay query_execution_replay_response_turn_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution_replay
    ADD CONSTRAINT query_execution_replay_response_turn_id_fkey FOREIGN KEY (response_turn_id) REFERENCES public.query_turn(id) ON DELETE CASCADE;


--
-- Name: query_execution_replay query_execution_replay_source_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution_replay
    ADD CONSTRAINT query_execution_replay_source_execution_id_fkey FOREIGN KEY (source_execution_id) REFERENCES public.query_execution(id) DEFERRABLE INITIALLY DEFERRED;


--
-- Name: query_execution query_execution_request_turn_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution
    ADD CONSTRAINT query_execution_request_turn_id_fkey FOREIGN KEY (request_turn_id) REFERENCES public.query_turn(id) ON DELETE SET NULL;


--
-- Name: query_execution query_execution_response_turn_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution
    ADD CONSTRAINT query_execution_response_turn_id_fkey FOREIGN KEY (response_turn_id) REFERENCES public.query_turn(id) ON DELETE SET NULL;


--
-- Name: query_execution query_execution_runtime_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_execution
    ADD CONSTRAINT query_execution_runtime_execution_id_fkey FOREIGN KEY (runtime_execution_id) REFERENCES public.runtime_execution(id) ON DELETE RESTRICT;


--
-- Name: query_ir_cache query_ir_cache_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_ir_cache
    ADD CONSTRAINT query_ir_cache_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: query_llm_context_snapshot query_llm_context_snapshot_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_llm_context_snapshot
    ADD CONSTRAINT query_llm_context_snapshot_execution_id_fkey FOREIGN KEY (execution_id) REFERENCES public.query_execution(id) ON DELETE CASCADE;


--
-- Name: query_result_cache query_result_cache_library_id_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_result_cache
    ADD CONSTRAINT query_result_cache_library_id_workspace_id_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: query_result_cache query_result_cache_source_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_result_cache
    ADD CONSTRAINT query_result_cache_source_execution_id_fkey FOREIGN KEY (source_execution_id) REFERENCES public.query_execution(id) ON DELETE CASCADE;


--
-- Name: query_turn query_turn_author_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_turn
    ADD CONSTRAINT query_turn_author_principal_id_fkey FOREIGN KEY (author_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: query_turn query_turn_conversation_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_turn
    ADD CONSTRAINT query_turn_conversation_id_fkey FOREIGN KEY (conversation_id) REFERENCES public.query_conversation(id) ON DELETE CASCADE;


--
-- Name: query_turn query_turn_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.query_turn
    ADD CONSTRAINT query_turn_execution_id_fkey FOREIGN KEY (execution_id) REFERENCES public.query_execution(id) ON DELETE SET NULL;


--
-- Name: runtime_action_record runtime_action_record_provider_binding_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_action_record
    ADD CONSTRAINT runtime_action_record_provider_binding_id_fkey FOREIGN KEY (provider_binding_id) REFERENCES public.ai_binding(id) ON DELETE SET NULL;


--
-- Name: runtime_action_record runtime_action_record_runtime_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_action_record
    ADD CONSTRAINT runtime_action_record_runtime_execution_id_fkey FOREIGN KEY (runtime_execution_id) REFERENCES public.runtime_execution(id) ON DELETE CASCADE;


--
-- Name: runtime_action_record runtime_action_record_stage_record_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_action_record
    ADD CONSTRAINT runtime_action_record_stage_record_id_fkey FOREIGN KEY (stage_record_id) REFERENCES public.runtime_stage_record(id) ON DELETE CASCADE;


--
-- Name: runtime_execution runtime_execution_parent_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_execution
    ADD CONSTRAINT runtime_execution_parent_execution_id_fkey FOREIGN KEY (parent_execution_id) REFERENCES public.runtime_execution(id) ON DELETE SET NULL;


--
-- Name: runtime_graph_canonical_summary runtime_graph_canonical_summary_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_canonical_summary
    ADD CONSTRAINT runtime_graph_canonical_summary_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_canonical_summary runtime_graph_canonical_summary_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_canonical_summary
    ADD CONSTRAINT runtime_graph_canonical_summary_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_community runtime_graph_community_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_community
    ADD CONSTRAINT runtime_graph_community_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_edge runtime_graph_edge_from_node_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_edge
    ADD CONSTRAINT runtime_graph_edge_from_node_id_fkey FOREIGN KEY (from_node_id) REFERENCES public.runtime_graph_node(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_edge runtime_graph_edge_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_edge
    ADD CONSTRAINT runtime_graph_edge_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_edge runtime_graph_edge_to_node_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_edge
    ADD CONSTRAINT runtime_graph_edge_to_node_id_fkey FOREIGN KEY (to_node_id) REFERENCES public.runtime_graph_node(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_evidence runtime_graph_evidence_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_evidence
    ADD CONSTRAINT runtime_graph_evidence_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.content_document(id) ON DELETE SET NULL;


--
-- Name: runtime_graph_evidence runtime_graph_evidence_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_evidence
    ADD CONSTRAINT runtime_graph_evidence_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_evidence runtime_graph_evidence_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_evidence
    ADD CONSTRAINT runtime_graph_evidence_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: runtime_graph_extraction runtime_graph_extraction_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_extraction
    ADD CONSTRAINT runtime_graph_extraction_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.content_document(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_extraction runtime_graph_extraction_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_extraction
    ADD CONSTRAINT runtime_graph_extraction_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_extraction runtime_graph_extraction_runtime_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_extraction
    ADD CONSTRAINT runtime_graph_extraction_runtime_execution_id_fkey FOREIGN KEY (runtime_execution_id) REFERENCES public.runtime_execution(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_filtered_artifact runtime_graph_filtered_artifact_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_filtered_artifact
    ADD CONSTRAINT runtime_graph_filtered_artifact_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_filtered_artifact runtime_graph_filtered_artifact_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_filtered_artifact
    ADD CONSTRAINT runtime_graph_filtered_artifact_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.content_revision(id) ON DELETE SET NULL;


--
-- Name: runtime_graph_node runtime_graph_node_document_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_node
    ADD CONSTRAINT runtime_graph_node_document_id_fkey FOREIGN KEY (document_id) REFERENCES public.content_document(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_node runtime_graph_node_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_node
    ADD CONSTRAINT runtime_graph_node_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_graph_snapshot runtime_graph_snapshot_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_graph_snapshot
    ADD CONSTRAINT runtime_graph_snapshot_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_policy_decision runtime_policy_decision_action_record_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_policy_decision
    ADD CONSTRAINT runtime_policy_decision_action_record_id_fkey FOREIGN KEY (action_record_id) REFERENCES public.runtime_action_record(id) ON DELETE CASCADE;


--
-- Name: runtime_policy_decision runtime_policy_decision_runtime_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_policy_decision
    ADD CONSTRAINT runtime_policy_decision_runtime_execution_id_fkey FOREIGN KEY (runtime_execution_id) REFERENCES public.runtime_execution(id) ON DELETE CASCADE;


--
-- Name: runtime_policy_decision runtime_policy_decision_stage_record_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_policy_decision
    ADD CONSTRAINT runtime_policy_decision_stage_record_id_fkey FOREIGN KEY (stage_record_id) REFERENCES public.runtime_stage_record(id) ON DELETE CASCADE;


--
-- Name: runtime_provider_profile runtime_provider_profile_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_provider_profile
    ADD CONSTRAINT runtime_provider_profile_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: runtime_provider_validation_log runtime_provider_validation_log_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_provider_validation_log
    ADD CONSTRAINT runtime_provider_validation_log_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE SET NULL;


--
-- Name: runtime_stage_record runtime_stage_record_runtime_execution_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.runtime_stage_record
    ADD CONSTRAINT runtime_stage_record_runtime_execution_id_fkey FOREIGN KEY (runtime_execution_id) REFERENCES public.runtime_execution(id) ON DELETE CASCADE;


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_job_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_job_id_fkey FOREIGN KEY (job_id) REFERENCES public.ingest_job(id) ON DELETE SET NULL;


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_job_scope_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_job_scope_fkey FOREIGN KEY (job_id, workspace_id, library_id) REFERENCES public.ingest_job(id, workspace_id, library_id) NOT VALID;


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_library_workspace_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_library_workspace_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE NOT VALID;


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_subscription_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_subscription_id_fkey FOREIGN KEY (subscription_id) REFERENCES public.webhook_subscription(id) ON DELETE CASCADE;


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_subscription_workspace_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_subscription_workspace_fkey FOREIGN KEY (subscription_id, workspace_id) REFERENCES public.webhook_subscription(id, workspace_id) ON DELETE CASCADE NOT VALID;


--
-- Name: webhook_delivery_attempt webhook_delivery_attempt_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_delivery_attempt
    ADD CONSTRAINT webhook_delivery_attempt_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- Name: webhook_lifecycle_outbox webhook_lifecycle_outbox_library_scope_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_lifecycle_outbox
    ADD CONSTRAINT webhook_lifecycle_outbox_library_scope_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;


--
-- Name: webhook_lifecycle_outbox_recipient webhook_lifecycle_outbox_recipient_outbox_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_lifecycle_outbox_recipient
    ADD CONSTRAINT webhook_lifecycle_outbox_recipient_outbox_fkey FOREIGN KEY (outbox_id) REFERENCES public.webhook_lifecycle_outbox(id) ON DELETE CASCADE;


--
-- Name: webhook_subscription webhook_subscription_created_by_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_subscription
    ADD CONSTRAINT webhook_subscription_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;


--
-- Name: webhook_subscription webhook_subscription_library_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_subscription
    ADD CONSTRAINT webhook_subscription_library_id_fkey FOREIGN KEY (library_id) REFERENCES public.catalog_library(id) ON DELETE CASCADE;


--
-- Name: webhook_subscription webhook_subscription_library_workspace_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_subscription
    ADD CONSTRAINT webhook_subscription_library_workspace_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE NOT VALID;


--
-- Name: webhook_subscription webhook_subscription_workspace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_subscription
    ADD CONSTRAINT webhook_subscription_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES public.catalog_workspace(id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--

-- Restore the SQLx session namespace after pg_dump's isolated DDL namespace.
SELECT pg_catalog.set_config('search_path', 'public', false);
