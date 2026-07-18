-- OPERATOR NOTE: this transactional migration adds ordinary UNIQUE/partial
-- indexes, scans legacy webhook rows for repair/redaction, and briefly takes
-- table locks. It is not a zero-downtime contract migration. Run it in a
-- measured maintenance window after checking table sizes/lock waiters; keep
-- statement/lock timeouts, free disk, and a rollback window appropriate for
-- ingest_job, webhook_delivery_attempt, billing_provider_call,
-- billing_execution_cost_rollup_state, query_result_cache, content_mutation_item,
-- knowledge_context_bundle, and query_execution. Restrictive CHECK validation
-- is intentionally deferred to a later post-drain contract migration.
-- Drain old graph writers before the first v2 graph claim. After that claim,
-- an old binary fails closed with SQLSTATE 40001 for the affected library;
-- rollback requires the explicit drained protocol-reset procedure documented
-- beside `writer_protocol_version` below, never a blind binary rollback.
-- Freeze AI catalog/binding changes until old query pods are drained: their
-- legacy cache identity does not include the generation invalidation added by
-- this release.

-- Generic web ingestion must not assume a particular source product. Migrate
-- only the exact 0001 built-in Confluence policy fingerprint. Policies with
-- any content change remain untouched; an operator who intentionally re-saved
-- that byte-equivalent built-in value is indistinguishable without historical
-- provenance and will receive the neutral default. The structural predicates
-- make the fingerprint intent reviewable instead of relying on the digest alone.
update catalog_library
set web_ingest_policy = '{"crawlFilter":{"allowPatterns":[],"blockPatterns":[]},"materializationFilter":{"allowPatterns":[],"blockPatterns":[]}}'::jsonb
where md5(web_ingest_policy::text) = 'e221d153b511e80f8c8d6c8dd7e877c9'
  and case
      when jsonb_typeof(web_ingest_policy #> '{crawlFilter,blockPatterns}') = 'array'
      then jsonb_array_length(web_ingest_policy #> '{crawlFilter,blockPatterns}') = 37
      else false
  end
  and web_ingest_policy @> '{"crawlFilter":{"blockPatterns":[{"kind":"path_prefix","value":"/login.action"},{"kind":"glob","value":"*sortBy=*"}]}}'::jsonb;

alter table catalog_library
    alter column web_ingest_policy
    set default '{"crawlFilter":{"allowPatterns":[],"blockPatterns":[]},"materializationFilter":{"allowPatterns":[],"blockPatterns":[]}}'::jsonb;

-- The semantic reranker uses the ordinary structured-chat gateway. Make the
-- optional `rerank` binding selectable for existing generative chat models
-- without widening embedding or vision-only profiles. Preserve every other
-- metadata field and role; the membership predicate makes replays idempotent.
update ai_model_catalog
set metadata_json = jsonb_set(
    metadata_json,
    '{defaultRoles}',
    (metadata_json -> 'defaultRoles') || '["rerank"]'::jsonb,
    false
)
where capability_kind = 'chat'::ai_model_capability_kind
  and jsonb_typeof(metadata_json -> 'defaultRoles') = 'array'
  and not ((metadata_json -> 'defaultRoles') ? 'rerank')
  and (metadata_json -> 'defaultRoles') ?| array[
      'extract_text',
      'extract_graph',
      'query_compile',
      'query_answer',
      'agent',
      'utility'
  ];

-- Keep the recurring stale-reservation sweep proportional to the small set of
-- in-flight calls instead of the lifetime billing history. This release-pending
-- migration is transactional, so CREATE INDEX (rather than CONCURRENTLY) can
-- briefly block writes on an exceptionally large existing billing table;
-- operators in that situation should schedule the migration in a quiet window.
create index if not exists idx_billing_provider_call_started_at_pending
    on billing_provider_call (started_at, id)
    where call_state = 'started'::billing_call_state;

-- Fence current-protocol webhook completion by an opaque ownership token.
-- Legacy in-flight rows remain tokenless and are not automatically reclaimed:
-- their old owner may still resume during overlap. A later contract migration
-- can install the lease-shape CHECK after old writers are drained and the audit
-- below is empty. Historical transport details are deliberately replaced
-- because reqwest errors may contain a full target URL (including path/query
-- data) and must not remain in operator-visible persistence.
alter table webhook_delivery_attempt
    add column if not exists delivery_lease_token uuid,
    add column if not exists error_code text,
    add column if not exists occurred_at timestamp with time zone;

alter table webhook_subscription
    add column if not exists delete_requested_at timestamp with time zone;

-- Older attempts did not persist producer occurrence time separately from
-- their payload. `created_at` is the closest immutable server-side timestamp
-- and is safer than trusting an arbitrary legacy JSON field when rebuilding
-- the canonical signed envelope.
update webhook_delivery_attempt
set occurred_at = created_at
where occurred_at is null;

alter table webhook_delivery_attempt
    alter column occurred_at set default now(),
    alter column occurred_at set not null;

-- Bind delivery history to the same tenant/library as its subscription. The
-- composite FKs are NOT VALID so a historical corrupt row does not make the
-- rollout unavailable; PostgreSQL still enforces them for every new write.
-- The redacted audit view below is the explicit inventory for quarantining or
-- repairing legacy rows before an operator validates the constraints.
do $$
begin
    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_subscription_id_workspace_id_key'
          and conrelid = 'webhook_subscription'::regclass
    ) then
        alter table webhook_subscription
            add constraint webhook_subscription_id_workspace_id_key
            unique (id, workspace_id);
    end if;

    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_delivery_attempt_subscription_workspace_fkey'
          and conrelid = 'webhook_delivery_attempt'::regclass
    ) then
        alter table webhook_delivery_attempt
            add constraint webhook_delivery_attempt_subscription_workspace_fkey
            foreign key (subscription_id, workspace_id)
            references webhook_subscription (id, workspace_id)
            on delete cascade
            not valid;
    end if;

    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_delivery_attempt_library_workspace_fkey'
          and conrelid = 'webhook_delivery_attempt'::regclass
    ) then
        alter table webhook_delivery_attempt
            add constraint webhook_delivery_attempt_library_workspace_fkey
            foreign key (library_id, workspace_id)
            references catalog_library (id, workspace_id)
            on delete cascade
            not valid;
    end if;

    if not exists (
        select 1 from pg_constraint
        where conname = 'ingest_job_id_workspace_id_library_id_key'
          and conrelid = 'ingest_job'::regclass
    ) then
        alter table ingest_job
            add constraint ingest_job_id_workspace_id_library_id_key
            unique (id, workspace_id, library_id);
    end if;

    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_delivery_attempt_job_scope_fkey'
          and conrelid = 'webhook_delivery_attempt'::regclass
    ) then
        alter table webhook_delivery_attempt
            add constraint webhook_delivery_attempt_job_scope_fkey
            foreign key (job_id, workspace_id, library_id)
            references ingest_job (id, workspace_id, library_id)
            not valid;
    end if;
end
$$;

create or replace function enforce_webhook_delivery_attempt_subscription_scope()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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

drop trigger if exists trg_webhook_delivery_attempt_subscription_scope
    on webhook_delivery_attempt;
create constraint trigger trg_webhook_delivery_attempt_subscription_scope
after insert or update of subscription_id, workspace_id, library_id, job_id
on webhook_delivery_attempt
deferrable initially immediate
for each row
execute function enforce_webhook_delivery_attempt_subscription_scope();

create or replace view webhook_delivery_attempt_tenant_integrity_audit as
select
    delivery.id as delivery_attempt_id,
    case
        when subscription.id is null then 'subscription_missing'
        when subscription.workspace_id <> delivery.workspace_id then 'subscription_workspace_mismatch'
        when library.id is null then 'library_scope_missing'
        when subscription.library_id is not null
             and subscription.library_id <> delivery.library_id
            then 'subscription_library_mismatch'
        when delivery.job_id is null then 'job_missing'
        when job.id is null then 'job_scope_mismatch'
        when job.job_kind <> 'webhook_delivery'::ingest_job_kind then 'job_kind_mismatch'
        when exists (
            select 1
            from webhook_delivery_attempt other_delivery
            where other_delivery.job_id = delivery.job_id
              and other_delivery.id <> delivery.id
        ) then 'duplicate_job_link'
        else 'unknown_scope_mismatch'
    end as issue_code
from webhook_delivery_attempt delivery
left join webhook_subscription subscription
  on subscription.id = delivery.subscription_id
left join catalog_library library
  on library.id = delivery.library_id
 and library.workspace_id = delivery.workspace_id
left join ingest_job job
  on job.id = delivery.job_id
 and job.workspace_id = delivery.workspace_id
 and job.library_id = delivery.library_id
where subscription.id is null
   or subscription.workspace_id <> delivery.workspace_id
   or library.id is null
   or (
       subscription.library_id is not null
       and subscription.library_id <> delivery.library_id
   )
   or delivery.job_id is null
   or job.id is null
   or job.job_kind <> 'webhook_delivery'::ingest_job_kind
   or exists (
       select 1
       from webhook_delivery_attempt other_delivery
       where other_delivery.job_id = delivery.job_id
         and other_delivery.id <> delivery.id
   );

-- Contract phase (after the audit is empty) installs:
--   create unique index ... on webhook_delivery_attempt(job_id)
--   where job_id is not null;
-- The expansion-phase trigger above enforces the same rule for every new link
-- without making legacy duplicate rows abort this rollout.

-- Serialize the logical `(subscription,event)` identity for rolling old/new
-- publishers. A conventional UNIQUE constraint cannot be installed until any
-- unsafe historical duplicates have been audited, but this trigger gives all
-- new writes the same invariant immediately.
create or replace function enforce_webhook_delivery_event_identity()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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

drop trigger if exists trg_webhook_delivery_event_identity
    on webhook_delivery_attempt;
create trigger trg_webhook_delivery_event_identity
after insert on webhook_delivery_attempt
for each row
execute function enforce_webhook_delivery_event_identity();

create or replace view webhook_delivery_event_identity_audit as
select subscription_id,
       count(*)::bigint as duplicate_count,
       'duplicate_subscription_event'::text as issue_code
from webhook_delivery_attempt
group by subscription_id, event_id
having count(*) > 1;

-- Rolling-upgrade bridge for the legacy two-transaction publisher. The
-- deferred trigger observes the final row state: current code links its job
-- before commit and is a no-op, while an older pod that commits the attempt
-- first receives a correctly scoped queue job in that same commit. This avoids
-- an eternal pending orphan if the old pod's subsequent nil-library job insert
-- fails the newer tenant FK.
create or replace function repair_unlinked_webhook_delivery_on_commit()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
declare
    repaired_job_id uuid;
begin
    insert into ingest_job (
        id, workspace_id, library_id, job_kind, queue_state,
        priority, dedupe_key, queued_at, available_at
    )
    select uuidv7(), delivery.workspace_id, delivery.library_id,
           'webhook_delivery'::ingest_job_kind,
           'queued'::ingest_queue_state,
           5,
           'wh-delivery-' || delivery.subscription_id::text || '-' || delivery.event_id,
           now(), now()
    from webhook_delivery_attempt delivery
    where delivery.id = new.id
      and delivery.job_id is null
      and delivery.delivery_state = 'pending'::webhook_delivery_state
    on conflict (library_id, dedupe_key) where dedupe_key is not null
    do update set dedupe_key = excluded.dedupe_key
    returning id into repaired_job_id;

    if repaired_job_id is not null then
        update webhook_delivery_attempt
        set job_id = repaired_job_id,
            updated_at = now()
        where id = new.id
          and job_id is null
          and delivery_state = 'pending'::webhook_delivery_state;
    end if;
    return null;
end
$$;

drop trigger if exists trg_repair_unlinked_webhook_delivery_on_commit
    on webhook_delivery_attempt;
create constraint trigger trg_repair_unlinked_webhook_delivery_on_commit
after insert on webhook_delivery_attempt
deferrable initially deferred
for each row
execute function repair_unlinked_webhook_delivery_on_commit();

-- Safe legacy replay cleanup: remove only pristine, unlinked duplicates when
-- another row for the same logical event is already linked. No attempted or
-- terminal delivery history is discarded.
delete from webhook_delivery_attempt duplicate
using webhook_delivery_attempt canonical
where duplicate.id <> canonical.id
  and duplicate.subscription_id = canonical.subscription_id
  and duplicate.event_id = canonical.event_id
  and duplicate.job_id is null
  and duplicate.delivery_state = 'pending'::webhook_delivery_state
  and duplicate.attempt_number = 0
  and (
      canonical.job_id is not null
      or (
          canonical.id < duplicate.id
          and canonical.job_id is null
          and canonical.delivery_state = 'pending'::webhook_delivery_state
          and canonical.attempt_number = 0
      )
  );

-- One-time repair for pending orphans that predate trigger installation.
insert into ingest_job (
    id, workspace_id, library_id, job_kind, queue_state,
    priority, dedupe_key, queued_at, available_at
)
select uuidv7(), delivery.workspace_id, delivery.library_id,
       'webhook_delivery'::ingest_job_kind,
       'queued'::ingest_queue_state,
       5,
       'wh-delivery-' || delivery.subscription_id::text || '-' || delivery.event_id,
       now(), now()
from webhook_delivery_attempt delivery
join webhook_subscription subscription
  on subscription.id = delivery.subscription_id
 and subscription.workspace_id = delivery.workspace_id
 and (subscription.library_id is null or subscription.library_id = delivery.library_id)
join catalog_library library
  on library.id = delivery.library_id
 and library.workspace_id = delivery.workspace_id
where delivery.job_id is null
  and delivery.delivery_state = 'pending'::webhook_delivery_state
  and octet_length(delivery.event_id) <= 512
on conflict (library_id, dedupe_key) where dedupe_key is not null do nothing;

update webhook_delivery_attempt delivery
set job_id = job.id,
    updated_at = now()
from ingest_job job
where delivery.job_id is null
  and delivery.delivery_state = 'pending'::webhook_delivery_state
  and octet_length(delivery.event_id) <= 512
  and job.workspace_id = delivery.workspace_id
  and job.library_id = delivery.library_id
  and job.job_kind = 'webhook_delivery'::ingest_job_kind
  and job.dedupe_key =
      'wh-delivery-' || delivery.subscription_id::text || '-' || delivery.event_id
  and exists (
      select 1
      from webhook_subscription subscription
      join catalog_library library
        on library.id = delivery.library_id
       and library.workspace_id = delivery.workspace_id
      where subscription.id = delivery.subscription_id
        and subscription.workspace_id = delivery.workspace_id
        and (
            subscription.library_id is null
            or subscription.library_id = delivery.library_id
        )
  );

-- Legacy in-flight rows are deliberately not reclaimed or rewritten here.
-- Their owners may still resume during rolling overlap. A current worker only
-- reclaims leases that already carry a current-protocol ownership token; old
-- tokenless owners must finish normally or be explicitly abandoned by an
-- operator who acknowledges the possible external duplicate-delivery risk.

update webhook_delivery_attempt
set error_code = 'legacy_failure_redacted',
    error_message = 'Legacy webhook delivery failure detail was redacted during upgrade'
where error_message is not null
  and error_code is null;

-- Response bodies are controlled by the remote endpoint and can contain
-- credentials or echoed request material. Delivery diagnostics retain the
-- status and typed failure summary instead of persisting that content.
update webhook_delivery_attempt
set response_body_excerpt = null
where response_body_excerpt is not null;

-- Keep privacy invariant during mixed-version overlap. Older writers may try
-- to persist remote-controlled response bodies or transport error strings.
-- Normalize every such write to a bounded static summary before it reaches
-- disk; current writers already supply the same canonical codes/summaries.
create or replace function redact_webhook_delivery_diagnostics()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
begin
    new.response_body_excerpt := null;
    if new.delivery_state = 'delivered'::webhook_delivery_state then
        new.error_code := null;
        new.error_message := null;
    elsif new.error_message is not null then
        new.error_code := coalesce(new.error_code, 'legacy_failure_redacted');
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
            else 'Legacy webhook delivery failure detail was redacted during upgrade'
        end;
    end if;
    return new;
end
$$;

drop trigger if exists trg_redact_webhook_delivery_diagnostics
    on webhook_delivery_attempt;
create trigger trg_redact_webhook_delivery_diagnostics
before insert or update of response_body_excerpt, error_code, error_message, delivery_state
on webhook_delivery_attempt
for each row
execute function redact_webhook_delivery_diagnostics();

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conname = 'webhook_delivery_attempt_error_code_format'
          and conrelid = 'webhook_delivery_attempt'::regclass
    ) then
        alter table webhook_delivery_attempt
            add constraint webhook_delivery_attempt_error_code_format check (
                error_code is null
                or error_code ~ '^[a-z][a-z0-9_]{0,63}$'
            );
    end if;

    if not exists (
        select 1
        from pg_constraint
        where conname = 'webhook_delivery_attempt_error_message_length'
          and conrelid = 'webhook_delivery_attempt'::regclass
    ) then
        alter table webhook_delivery_attempt
            add constraint webhook_delivery_attempt_error_message_length check (
                error_message is null or char_length(error_message) <= 512
            );
    end if;
end
$$;

-- The delivery-token shape is intentionally expand-only in this migration.
-- A CHECK constraint would also apply to writes from an older pod during a
-- rolling deployment, and those pods do not know how to populate the token.
-- Operators must drain old delivery producers, confirm this redacted audit is
-- empty, then install/validate the CHECK in a later contract migration.
create or replace view webhook_delivery_lease_shape_audit as
select id as delivery_attempt_id,
       'delivery_lease_shape_mismatch'::text as issue_code
from webhook_delivery_attempt
where (delivery_state = 'delivering'::webhook_delivery_state)
      <> (delivery_lease_token is not null);

create index if not exists idx_webhook_delivery_stale_lease
    on webhook_delivery_attempt (updated_at, id)
    where delivery_state = 'delivering'::webhook_delivery_state;

create index if not exists idx_webhook_delivery_catalog_blockers
    on webhook_delivery_attempt (library_id, id)
    where delivery_state in ('pending', 'delivering')
       or (delivery_state = 'failed' and next_attempt_at is not null);

create index if not exists idx_webhook_delivery_library_linked_job
    on webhook_delivery_attempt (library_id, job_id)
    where job_id is not null;

-- Public management APIs use immutable `(created_at, id)` keysets. Keep both
-- the all-state and state-filtered paths index-backed so delivery history can
-- never degrade into an unbounded sort as a tenant grows.
create index if not exists idx_webhook_delivery_subscription_keyset
    on webhook_delivery_attempt (subscription_id, created_at desc, id desc);

create index if not exists idx_webhook_delivery_subscription_state_keyset
    on webhook_delivery_attempt (
        subscription_id,
        delivery_state,
        created_at desc,
        id desc
    );

-- Lifecycle webhooks use a transactional outbox so a committed document
-- delete/readiness transition cannot be lost between the content transaction
-- and queue fanout. Dispatch leases are recoverable and fanout remains
-- idempotent through the existing per-subscription ingest-job dedupe key.
create table if not exists webhook_lifecycle_outbox (
    id uuid default uuidv7() not null,
    event_id text not null,
    event_type text not null,
    occurred_at timestamp with time zone not null,
    workspace_id uuid not null,
    library_id uuid not null,
    payload_json jsonb not null,
    dispatch_state text default 'pending' not null,
    dispatch_attempts integer default 0 not null,
    available_at timestamp with time zone default now() not null,
    lease_owner text,
    lease_token uuid,
    leased_at timestamp with time zone,
    lease_expires_at timestamp with time zone,
    last_error_code text,
    last_error text,
    dispatched_at timestamp with time zone,
    resolution_reason_code text,
    resolved_at timestamp with time zone,
    created_at timestamp with time zone default now() not null,
    updated_at timestamp with time zone default now() not null,
    constraint webhook_lifecycle_outbox_pkey primary key (id),
    constraint webhook_lifecycle_outbox_event_id_key unique (event_id),
    constraint webhook_lifecycle_outbox_event_id_length
        check (char_length(event_id) between 1 and 512),
    constraint webhook_lifecycle_outbox_event_type
        check (event_type in ('revision.ready', 'document.deleted')),
    constraint webhook_lifecycle_outbox_payload_object
        check (jsonb_typeof(payload_json) = 'object'),
    constraint webhook_lifecycle_outbox_dispatch_state
        check (dispatch_state in (
            'pending', 'dispatching', 'dispatched', 'dead_letter', 'resolved'
        )),
    constraint webhook_lifecycle_outbox_attempts_nonnegative
        check (dispatch_attempts >= 0),
    constraint webhook_lifecycle_outbox_lease_owner_length
        check (lease_owner is null or char_length(lease_owner) between 1 and 255),
    constraint webhook_lifecycle_outbox_last_error_length
        check (last_error is null or char_length(last_error) <= 2000),
    constraint webhook_lifecycle_outbox_last_error_code_format
        check (last_error_code is null or last_error_code ~ '^[a-z][a-z0-9_]{0,63}$'),
    constraint webhook_lifecycle_outbox_dead_letter_error_code
        check (dispatch_state <> 'dead_letter' or last_error_code is not null),
    constraint webhook_lifecycle_outbox_resolution_reason_code_format
        check (
            resolution_reason_code is null
            or resolution_reason_code ~ '^[a-z][a-z0-9_]{0,63}$'
        ),
    constraint webhook_lifecycle_outbox_lease_shape check (
        (
            dispatch_state = 'dispatching'
            and lease_owner is not null
            and lease_token is not null
            and leased_at is not null
            and lease_expires_at is not null
        )
        or (
            dispatch_state <> 'dispatching'
            and lease_owner is null
            and lease_token is null
            and leased_at is null
            and lease_expires_at is null
        )
    ),
    constraint webhook_lifecycle_outbox_dispatched_shape
        check ((dispatch_state = 'dispatched') = (dispatched_at is not null)),
    constraint webhook_lifecycle_outbox_resolved_shape check (
        (
            dispatch_state = 'resolved'
            and resolution_reason_code is not null
            and resolved_at is not null
        )
        or (
            dispatch_state <> 'resolved'
            and resolution_reason_code is null
            and resolved_at is null
        )
    ),
    constraint webhook_lifecycle_outbox_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library (id, workspace_id)
        on delete cascade
);

-- Keep a partially provisioned pre-release database recoverable. Normal sqlx
-- migrations are atomic, but these statements also make the release-pending
-- migration safe for operator-created preview schemas.
alter table webhook_lifecycle_outbox
    add column if not exists occurred_at timestamp with time zone,
    add column if not exists last_error_code text,
    add column if not exists resolution_reason_code text,
    add column if not exists resolved_at timestamp with time zone;

update webhook_lifecycle_outbox
set occurred_at = created_at
where occurred_at is null;

update webhook_lifecycle_outbox
set last_error_code = 'legacy_fanout_failed',
    last_error = case
        when last_error is null then null
        else 'Legacy webhook fanout failure detail was redacted during upgrade'
    end
where last_error_code is null
  and (last_error is not null or dispatch_state = 'dead_letter');

alter table webhook_lifecycle_outbox
    alter column occurred_at set not null;

-- Expand the terminal state contract without making old relay binaries write
-- new columns. Old workers continue to produce the four legacy states; new
-- workers alone can perform the explicit dead-letter resolution transition.
alter table webhook_lifecycle_outbox
    drop constraint if exists webhook_lifecycle_outbox_dispatch_state;

alter table webhook_lifecycle_outbox
    add constraint webhook_lifecycle_outbox_dispatch_state check (
        dispatch_state in (
            'pending', 'dispatching', 'dispatched', 'dead_letter', 'resolved'
        )
    );

do $$
begin
    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_lifecycle_outbox_last_error_code_format'
          and conrelid = 'webhook_lifecycle_outbox'::regclass
    ) then
        alter table webhook_lifecycle_outbox
            add constraint webhook_lifecycle_outbox_last_error_code_format check (
                last_error_code is null
                or last_error_code ~ '^[a-z][a-z0-9_]{0,63}$'
            );
    end if;

    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_lifecycle_outbox_dead_letter_error_code'
          and conrelid = 'webhook_lifecycle_outbox'::regclass
    ) then
        alter table webhook_lifecycle_outbox
            add constraint webhook_lifecycle_outbox_dead_letter_error_code check (
                dispatch_state <> 'dead_letter'
                or last_error_code is not null
            );
    end if;

    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_lifecycle_outbox_resolution_reason_code_format'
          and conrelid = 'webhook_lifecycle_outbox'::regclass
    ) then
        alter table webhook_lifecycle_outbox
            add constraint webhook_lifecycle_outbox_resolution_reason_code_format check (
                resolution_reason_code is null
                or resolution_reason_code ~ '^[a-z][a-z0-9_]{0,63}$'
            );
    end if;

    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_lifecycle_outbox_resolved_shape'
          and conrelid = 'webhook_lifecycle_outbox'::regclass
    ) then
        alter table webhook_lifecycle_outbox
            add constraint webhook_lifecycle_outbox_resolved_shape check (
                (
                    dispatch_state = 'resolved'
                    and resolution_reason_code is not null
                    and resolved_at is not null
                )
                or (
                    dispatch_state <> 'resolved'
                    and resolution_reason_code is null
                    and resolved_at is null
                )
            );
    end if;
end
$$;

create table if not exists webhook_lifecycle_outbox_recipient (
    outbox_id uuid not null,
    subscription_id uuid not null,
    created_at timestamp with time zone default now() not null,
    constraint webhook_lifecycle_outbox_recipient_pkey
        primary key (outbox_id, subscription_id),
    constraint webhook_lifecycle_outbox_recipient_outbox_fkey
        foreign key (outbox_id)
        references webhook_lifecycle_outbox (id)
        on delete cascade
);

-- `subscription_id` is an event-time UUID snapshot, not a live ownership FK.
-- A concurrent subscription delete must never make canonical content commit
-- fail during recipient capture. Dispatch joins the live subscription and
-- terminal-skips a deleted/inactive recipient; the outbox FK owns cleanup.
alter table webhook_lifecycle_outbox_recipient
    drop constraint if exists webhook_lifecycle_outbox_recipient_subscription_fkey;

create index if not exists idx_webhook_lifecycle_outbox_due
    on webhook_lifecycle_outbox (available_at, created_at, id)
    where dispatch_state = 'pending';

create index if not exists idx_webhook_lifecycle_outbox_expired_lease
    on webhook_lifecycle_outbox (lease_expires_at, created_at, id)
    where dispatch_state = 'dispatching';

create index if not exists idx_webhook_lifecycle_outbox_library_created
    on webhook_lifecycle_outbox (library_id, created_at, id);

create index if not exists idx_webhook_lifecycle_outbox_library_blockers
    on webhook_lifecycle_outbox (library_id, id)
    where dispatch_state <> 'dispatched';

-- Exact post-resolution blocker index. The older superset index is retained
-- for rolling-upgrade compatibility and can still serve old binaries safely.
create index if not exists idx_webhook_lifecycle_outbox_library_unresolved_blockers
    on webhook_lifecycle_outbox (library_id, id)
    where dispatch_state not in ('dispatched', 'resolved');

create index if not exists idx_webhook_lifecycle_outbox_dispatched_retention
    on webhook_lifecycle_outbox (dispatched_at, id)
    where dispatch_state = 'dispatched';

create index if not exists idx_webhook_lifecycle_outbox_dead_letter
    on webhook_lifecycle_outbox (updated_at, id)
    where dispatch_state = 'dead_letter';

create index if not exists idx_webhook_lifecycle_outbox_library_dead_letter
    on webhook_lifecycle_outbox (library_id, updated_at desc, id desc)
    where dispatch_state = 'dead_letter';

create index if not exists idx_webhook_lifecycle_outbox_recipient_subscription
    on webhook_lifecycle_outbox_recipient (subscription_id, outbox_id);

-- New subscriptions must not cross tenant ownership, even through direct
-- repository/SQL callers. NOT VALID preserves upgrade availability when a
-- historical mismatched row exists; PostgreSQL still enforces the constraint
-- for every new or updated row, and the maintenance audit can identify legacy
-- rows for explicit operator repair.
do $$
begin
    if not exists (
        select 1 from pg_constraint
        where conname = 'webhook_subscription_library_workspace_fkey'
          and conrelid = 'webhook_subscription'::regclass
    ) then
        alter table webhook_subscription
            add constraint webhook_subscription_library_workspace_fkey
            foreign key (library_id, workspace_id)
            references catalog_library (id, workspace_id)
            on delete cascade
            not valid;
    end if;

end
$$;

-- Restrictive field/event CHECKs belong to the post-drain contract phase:
-- NOT VALID would still reject writes from an older pod during overlap. Keep
-- this expansion migration observable without narrowing the legacy write API.
create or replace view webhook_subscription_contract_audit as
select id as subscription_id,
       case
           when char_length(btrim(display_name)) not between 1 and 128
             or octet_length(target_url) not between 1 and 2048
             or octet_length(secret) not between 1 and 8192
             or octet_length(custom_headers_json::text) > 9000
               then 'bounded_field_violation'
           when cardinality(event_types) not between 1 and 2
             or not (event_types <@ array['revision.ready', 'document.deleted']::text[])
             or cardinality(event_types) <>
                (case when 'revision.ready' = any(event_types) then 1 else 0 end)
                + (case when 'document.deleted' = any(event_types) then 1 else 0 end)
               then 'event_catalog_violation'
           when jsonb_typeof(custom_headers_json) not in ('object', 'string', 'null')
               then 'custom_headers_shape_violation'
           else 'unknown_contract_violation'
       end as issue_code
from webhook_subscription
where char_length(btrim(display_name)) not between 1 and 128
   or octet_length(target_url) not between 1 and 2048
   or octet_length(secret) not between 1 and 8192
   or octet_length(custom_headers_json::text) > 9000
   or cardinality(event_types) not between 1 and 2
   or not (event_types <@ array['revision.ready', 'document.deleted']::text[])
   or cardinality(event_types) <>
      (case when 'revision.ready' = any(event_types) then 1 else 0 end)
      + (case when 'document.deleted' = any(event_types) then 1 else 0 end)
   or jsonb_typeof(custom_headers_json) not in ('object', 'string', 'null');

-- Enforce fanout quota in the database so an older API pod cannot bypass the
-- sole workspace-scoped serializer during a rolling deployment. Existing
-- over-quota tenants remain visible through the redacted audit view and must
-- be drained explicitly; the migration never deactivates customer endpoints.
create or replace function enforce_webhook_subscription_workspace_quota()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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

drop trigger if exists trg_webhook_subscription_workspace_quota
    on webhook_subscription;
create trigger trg_webhook_subscription_workspace_quota
after insert or update of workspace_id, active
on webhook_subscription
for each row
execute function enforce_webhook_subscription_workspace_quota();

create or replace view webhook_subscription_quota_audit as
select workspace_id,
       count(*)::bigint as active_subscription_count,
       'active_subscription_quota_exceeded'::text as issue_code
from webhook_subscription
where active
group by workspace_id
having count(*) > 100;

create or replace view webhook_subscription_total_quota_audit as
select workspace_id,
       count(*)::bigint as total_subscription_count,
       'total_subscription_quota_exceeded'::text as issue_code
from webhook_subscription
group by workspace_id
having count(*) > 1000;

create index if not exists idx_webhook_subscription_workspace_keyset
    on webhook_subscription (workspace_id, created_at asc, id asc);

create or replace view webhook_subscription_tenant_integrity_audit as
select subscription.id as subscription_id,
       'library_workspace_mismatch'::text as issue_code
from webhook_subscription subscription
left join catalog_library library
  on library.id = subscription.library_id
 and library.workspace_id = subscription.workspace_id
where subscription.library_id is not null
  and library.id is null;

-- Derived execution-cost rows are rebuilt from canonical provider calls,
-- usage, and charges. Keep a generation-fenced durable repair cursor so a
-- process crash after canonical completion cannot permanently strand a stale
-- aggregate. The row is retained after repair: applied_generation catches up
-- to dirty_generation, which also closes the "missing marker" race between a
-- concurrent completion and an aggregate refresh.
create table if not exists billing_execution_cost_rollup_state (
    owning_execution_kind billing_owning_execution_kind not null,
    owning_execution_id uuid not null,
    workspace_id uuid not null,
    library_id uuid not null,
    dirty_generation bigint default 1 not null,
    applied_generation bigint default 0 not null,
    dirty_at timestamp with time zone default now() not null,
    applied_at timestamp with time zone,
    repair_attempts integer default 0 not null,
    next_attempt_at timestamp with time zone default now() not null,
    last_error text,
    terminal_error_code text,
    constraint billing_execution_cost_rollup_state_pkey
        primary key (owning_execution_kind, owning_execution_id),
    constraint billing_execution_cost_rollup_state_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library (id, workspace_id)
        on delete cascade,
    constraint billing_execution_cost_rollup_state_generations
        check (
            dirty_generation >= 1
            and applied_generation >= 0
            and applied_generation <= dirty_generation
        ),
    constraint billing_execution_cost_rollup_state_attempts_nonnegative
        check (repair_attempts >= 0),
    constraint billing_execution_cost_rollup_state_last_error_length
        check (last_error is null or char_length(last_error) <= 512),
    constraint billing_execution_cost_rollup_state_terminal_error
        check (
            terminal_error_code is null
            or (
                terminal_error_code = 'mixed_currency'
                and applied_generation = dirty_generation
                and applied_at is not null
            )
        )
);

create index if not exists idx_billing_execution_cost_rollup_state_due
    on billing_execution_cost_rollup_state (next_attempt_at, dirty_at, owning_execution_id)
    where applied_generation < dirty_generation;

create index if not exists idx_billing_execution_cost_rollup_state_library_dirty
    on billing_execution_cost_rollup_state (library_id, dirty_at, owning_execution_id)
    where applied_generation < dirty_generation;

create index if not exists idx_billing_execution_cost_rollup_state_workspace_dirty
    on billing_execution_cost_rollup_state (workspace_id, dirty_at, owning_execution_id)
    where applied_generation < dirty_generation;

create index if not exists idx_billing_execution_cost_rollup_state_library_terminal
    on billing_execution_cost_rollup_state (library_id, owning_execution_id)
    where terminal_error_code is not null;

create index if not exists idx_billing_execution_cost_rollup_state_workspace_terminal
    on billing_execution_cost_rollup_state (workspace_id, owning_execution_id)
    where terminal_error_code is not null;

-- Existing aggregates cannot prove which canonical generation they include.
-- Mark every historical execution dirty once; the bounded worker repair loop
-- reconciles them without trusting timestamps that can race with completion.
insert into billing_execution_cost_rollup_state (
    owning_execution_kind,
    owning_execution_id,
    workspace_id,
    library_id,
    dirty_generation,
    applied_generation,
    dirty_at,
    next_attempt_at
)
select distinct on (
    provider_call.owning_execution_kind,
    provider_call.owning_execution_id
)
    provider_call.owning_execution_kind,
    provider_call.owning_execution_id,
    provider_call.workspace_id,
    provider_call.library_id,
    1,
    0,
    now(),
    now()
from billing_provider_call provider_call
order by
    provider_call.owning_execution_kind,
    provider_call.owning_execution_id,
    provider_call.started_at desc,
    provider_call.id desc
on conflict (owning_execution_kind, owning_execution_id) do nothing;

-- The existing library-scoped DESC index cannot support a global TTL sweep:
-- PostgreSQL would have to visit every library prefix. A global ascending
-- `(updated_at, cache_key)` index gives the bounded oldest-first GC query a
-- stable range/order path and deterministic tie-breaker. `updated_at` is NOT
-- NULL for every cache row, so a partial `IS NOT NULL` predicate would only
-- add planner complexity without reducing the index. This release-pending
-- migration is transactional; CREATE INDEX can briefly wait for cache writers
-- on a very large table, so operators should use the measured maintenance
-- window and lock/statement timeout guidance at the top of this file.
create index if not exists idx_query_result_cache_gc_updated
    on query_result_cache (updated_at, cache_key);

-- Runtime graph / AI configuration generation fencing.
--
-- A build owner is durable rather than process-local. This lets the terminal
-- graph publish compare-and-set its claim after a slow external graph-store
-- write, while projection_version remains independently monotonic.
alter table runtime_graph_snapshot
    add column if not exists build_epoch uuid,
    add column if not exists writer_protocol_version smallint default 1 not null;

update runtime_graph_snapshot
set build_epoch = uuidv7()
where build_epoch is null;

alter table runtime_graph_snapshot
    alter column build_epoch set default uuidv7(),
    alter column build_epoch set not null;

-- Rolling old binaries do not include build_epoch in their ON CONFLICT SET
-- list. Once a v2 writer claims a row, an old write would otherwise preserve
-- the v2 epoch while replacing the payload, making a later v2 terminal CAS
-- falsely appear owned. New writers set a transaction-local epoch marker;
-- reject marker-less writes only after the row has crossed the v2 boundary.
-- Rollout/rollback contract: old and new binaries may overlap until a library's
-- first v2 claim; old graph writers then fail closed with SQLSTATE 40001 for
-- that library. Do not roll graph workers back in place after v2 claims. An
-- emergency rollback requires a maintenance drain of every graph writer,
-- verification that no snapshot is `building`, and an explicit operator-only
-- reset of writer_protocol_version to 1 before old workers restart. Prefer a
-- forward fix; never reset the protocol while a v2 build can still publish.
create or replace function fence_runtime_graph_snapshot_writer_protocol()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
declare
    claimed_epoch text := current_setting(
        'ironrag.runtime_graph_snapshot_build_epoch',
        true
    );
begin
    if new.writer_protocol_version >= 2
       and claimed_epoch is distinct from new.build_epoch::text then
        raise exception 'runtime graph snapshot writer lost the v2 build epoch fence'
            using errcode = '40001';
    end if;
    return new;
end
$$;

drop trigger if exists trg_runtime_graph_snapshot_writer_protocol
    on runtime_graph_snapshot;
create trigger trg_runtime_graph_snapshot_writer_protocol
before insert or update on runtime_graph_snapshot
for each row
execute function fence_runtime_graph_snapshot_writer_protocol();

-- Existing query-result cache writes and replays already take a SHARE lock on
-- catalog_library and compare source_truth_version. Advance that same durable
-- generation in the AI-config transaction so no read-compute-insert window can
-- publish or replay a result made with an obsolete effective binding.
create or replace function bump_ai_config_source_generations(
    affected_library_ids uuid[]
)
returns void
language plpgsql
set search_path = pg_catalog, public
as $$
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

-- AI config triggers run after the changed child rows exist so they can derive
-- the effective scope. Take one transaction-scoped serializer in a BEFORE
-- STATEMENT trigger, before any child row lock, and take the same serializer
-- before catalog deletes. This closes the child -> library versus
-- library -> cascading-child deadlock cycle; configuration writes are rare
-- control-plane operations, so serializing them is an acceptable trade-off.
create or replace function serialize_ai_config_generation_writes()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
begin
    perform pg_advisory_xact_lock(
        hashtextextended('ironrag:ai-config-generation', 0)
    );
    return null;
end
$$;

drop trigger if exists trg_ai_binding_generation_serializer on ai_binding;
create trigger trg_ai_binding_generation_serializer
before insert or update or delete on ai_binding
for each statement
execute function serialize_ai_config_generation_writes();

drop trigger if exists trg_ai_account_generation_serializer on ai_account;
create trigger trg_ai_account_generation_serializer
before insert or update or delete on ai_account
for each statement
execute function serialize_ai_config_generation_writes();

drop trigger if exists trg_ai_model_generation_serializer on ai_model_catalog;
create trigger trg_ai_model_generation_serializer
before insert or update or delete on ai_model_catalog
for each statement
execute function serialize_ai_config_generation_writes();

drop trigger if exists trg_ai_provider_generation_serializer on ai_provider_catalog;
create trigger trg_ai_provider_generation_serializer
before insert or update or delete on ai_provider_catalog
for each statement
execute function serialize_ai_config_generation_writes();

drop trigger if exists trg_catalog_library_ai_generation_serializer on catalog_library;
create trigger trg_catalog_library_ai_generation_serializer
before delete on catalog_library
for each statement
execute function serialize_ai_config_generation_writes();

drop trigger if exists trg_catalog_workspace_ai_generation_serializer on catalog_workspace;
create trigger trg_catalog_workspace_ai_generation_serializer
before delete on catalog_workspace
for each statement
execute function serialize_ai_config_generation_writes();

create or replace function invalidate_ai_binding_insert_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

create or replace function invalidate_ai_binding_update_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

create or replace function invalidate_ai_binding_delete_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

drop trigger if exists trg_ai_binding_source_generation on ai_binding;
drop trigger if exists trg_ai_binding_insert_source_generations on ai_binding;
drop trigger if exists trg_ai_binding_update_source_generations on ai_binding;
drop trigger if exists trg_ai_binding_delete_source_generations on ai_binding;
create trigger trg_ai_binding_insert_source_generations
after insert on ai_binding
referencing new table as new_bindings
for each statement
execute function invalidate_ai_binding_insert_source_generations();
create trigger trg_ai_binding_update_source_generations
after update on ai_binding
referencing old table as old_bindings new table as new_bindings
for each statement
execute function invalidate_ai_binding_update_source_generations();
create trigger trg_ai_binding_delete_source_generations
after delete on ai_binding
referencing old table as old_bindings
for each statement
execute function invalidate_ai_binding_delete_source_generations();

create or replace function invalidate_ai_account_update_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

create or replace function invalidate_ai_account_delete_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

drop trigger if exists trg_ai_account_source_generation on ai_account;
drop trigger if exists trg_ai_account_update_source_generations on ai_account;
drop trigger if exists trg_ai_account_delete_source_generations on ai_account;
create trigger trg_ai_account_update_source_generations
after update on ai_account
referencing old table as old_accounts new table as new_accounts
for each statement
execute function invalidate_ai_account_update_source_generations();
create trigger trg_ai_account_delete_source_generations
after delete on ai_account
referencing old table as old_accounts
for each statement
execute function invalidate_ai_account_delete_source_generations();

create or replace function invalidate_ai_model_update_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

create or replace function invalidate_ai_model_delete_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

drop trigger if exists trg_ai_model_source_generation on ai_model_catalog;
drop trigger if exists trg_ai_model_update_source_generations on ai_model_catalog;
drop trigger if exists trg_ai_model_delete_source_generations on ai_model_catalog;
create trigger trg_ai_model_update_source_generations
after update on ai_model_catalog
referencing old table as old_models new table as new_models
for each statement
execute function invalidate_ai_model_update_source_generations();
create trigger trg_ai_model_delete_source_generations
after delete on ai_model_catalog
referencing old table as old_models
for each statement
execute function invalidate_ai_model_delete_source_generations();

create or replace function invalidate_ai_provider_update_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

create or replace function invalidate_ai_provider_delete_source_generations()
returns trigger
language plpgsql
set search_path = pg_catalog, public
as $$
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
    perform public.bump_ai_config_source_generations(affected_library_ids);
    return null;
end
$$;

drop trigger if exists trg_ai_provider_source_generation on ai_provider_catalog;
drop trigger if exists trg_ai_provider_update_source_generations on ai_provider_catalog;
drop trigger if exists trg_ai_provider_delete_source_generations on ai_provider_catalog;
create trigger trg_ai_provider_update_source_generations
after update on ai_provider_catalog
referencing old table as old_providers new table as new_providers
for each statement
execute function invalidate_ai_provider_update_source_generations();
create trigger trg_ai_provider_delete_source_generations
after delete on ai_provider_catalog
referencing old table as old_providers
for each statement
execute function invalidate_ai_provider_delete_source_generations();

create index if not exists idx_ai_binding_active_account_generation
    on ai_binding (account_id)
    where binding_state = 'active';

create index if not exists idx_ai_binding_active_model_generation
    on ai_binding (model_catalog_id)
    where binding_state = 'active';

create index if not exists idx_ai_account_provider_generation
    on ai_account (provider_catalog_id);

-- Document-head parentage validation narrows every mutation check by the
-- owning mutation before inspecting its document/revision anchors. PostgreSQL
-- does not create an index for the content_mutation_item FK automatically;
-- without this covering path a head promotion would scan the whole item table.
create index if not exists idx_content_mutation_item_head_parentage
    on content_mutation_item (mutation_id, document_id)
    include (base_revision_id, result_revision_id);

-- Query replay provenance ownership and exact context-bundle identity.
--
-- A target conversation stores an answer copied from a source execution. The
-- source execution and its verified evidence must outlive every external
-- replay audit that references it; otherwise source-session eviction leaves a
-- plausible answer with no durable provenance. The target conversation still
-- owns the replay row and releases the source when it is deleted.
alter table query_execution_replay
    drop constraint if exists query_execution_replay_source_execution_id_fkey;
alter table query_execution_replay
    add constraint query_execution_replay_source_execution_id_fkey
    foreign key (source_execution_id)
    references query_execution(id)
    on delete no action
    deferrable initially deferred;

-- Legacy builds allowed more than one bundle to claim a query execution and
-- read it with an unordered LIMIT 1. Keep only the execution's canonical
-- context_bundle_id linked; detached diagnostic bundles remain available but
-- can no longer shadow answer verification.
update knowledge_context_bundle bundle
set query_execution_id = null,
    updated_at = clock_timestamp()
where bundle.query_execution_id is not null
  and not exists (
      select 1
      from query_execution execution
      where execution.id = bundle.query_execution_id
        and execution.context_bundle_id = bundle.bundle_id
        and execution.workspace_id = bundle.workspace_id
        and execution.library_id = bundle.library_id
  );

create unique index if not exists idx_knowledge_context_bundle_query_execution_unique
    on knowledge_context_bundle (query_execution_id)
    where query_execution_id is not null;

-- Conversation-cap eviction now checks both unfinished executions and source
-- executions retained by external replays. Keep both anti-joins index-backed.
create index if not exists idx_query_execution_conversation
    on query_execution (conversation_id, id);

-- Broad procedure synthesis samples a bounded revision head and a separate
-- schema-owned structured lane ordered by source ordinal. The ordinary
-- revision/ordinal index serves the head lane; this partial covering order lets
-- PostgreSQL stop the typed lane at its LIMIT without scanning and sorting every
-- structured block in a large revision.
create index if not exists idx_knowledge_structured_block_setup_order
    on knowledge_structured_block (revision_id, ordinal, block_id)
    where block_kind in ('table', 'table_row', 'code_block', 'source_unit');
