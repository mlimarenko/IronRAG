-- Collapse historical projection spellings before enforcing one runtime state
-- vocabulary. This is a forward-only data migration: application code reads
-- and writes only the canonical values installed below.

lock table knowledge_revision in share row exclusive mode;

do $$
begin
    if exists (
        select 1
        from knowledge_revision
        where text_state not in (
            'accepted', 'extracting_text', 'text_readable', 'failed',
            'readable', 'ready'
        )
           or vector_state not in (
               'accepted', 'processing', 'ready', 'failed', 'vector_ready'
           )
           or graph_state not in (
               'accepted', 'processing', 'ready', 'graph_degraded', 'failed',
               'graph_ready'
           )
    ) then
        raise exception using
            errcode = 'check_violation',
            message = 'knowledge_revision contains an unknown projection state',
            hint = 'repair unknown states explicitly before applying migration 0016';
    end if;
end
$$;

update knowledge_revision
set text_state = 'text_readable'
where text_state in ('readable', 'ready');

update knowledge_revision
set vector_state = 'ready'
where vector_state = 'vector_ready';

update knowledge_revision
set graph_state = 'ready'
where graph_state = 'graph_ready';

alter table knowledge_revision
    add constraint knowledge_revision_text_state_canonical_check
    check (text_state in ('accepted', 'extracting_text', 'text_readable', 'failed'))
    not valid;

alter table knowledge_revision
    add constraint knowledge_revision_vector_state_canonical_check
    check (vector_state in ('accepted', 'processing', 'ready', 'failed'))
    not valid;

alter table knowledge_revision
    add constraint knowledge_revision_graph_state_canonical_check
    check (graph_state in ('accepted', 'processing', 'ready', 'graph_degraded', 'failed'))
    not valid;

alter table knowledge_revision
    validate constraint knowledge_revision_text_state_canonical_check;

alter table knowledge_revision
    validate constraint knowledge_revision_vector_state_canonical_check;

alter table knowledge_revision
    validate constraint knowledge_revision_graph_state_canonical_check;
