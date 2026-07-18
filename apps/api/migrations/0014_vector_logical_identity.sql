-- One logical object/profile/generation owns one physical vector row. Existing
-- duplicates are never discarded implicitly: migration stops with an explicit
-- repair requirement so an operator can inspect provenance first.
do $migration$
declare
    lane record;
    has_duplicates boolean;
    logical_index_name text;
begin
    for lane in
        select relation.relname as relation_name,
               left(relation.relname, char_length('knowledge_chunk_vector_d'))
                   = 'knowledge_chunk_vector_d' as is_chunk
        from pg_catalog.pg_class relation
        join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
        where namespace.nspname = current_schema()
          and relation.relkind = 'r'
          and (
              relation.relname ~ '^knowledge_chunk_vector_d[0-9]+$'
              or relation.relname ~ '^knowledge_entity_vector_d[0-9]+$'
          )
        order by relation.relname
    loop
        if lane.is_chunk then
            execute format(
                'select exists (
                     select 1 from %I.%I
                     group by library_id, chunk_id, revision_id,
                              embedding_model_key, vector_kind, freshness_generation
                     having count(*) > 1
                     limit 1
                 )',
                current_schema(),
                lane.relation_name
            ) into has_duplicates;
        else
            execute format(
                'select exists (
                     select 1 from %I.%I
                     group by library_id, entity_id,
                              embedding_model_key, vector_kind, freshness_generation
                     having count(*) > 1
                     limit 1
                 )',
                current_schema(),
                lane.relation_name
            ) into has_duplicates;
        end if;

        if has_duplicates then
            raise exception
                'cannot install logical vector uniqueness on %: duplicate object/profile/generation rows require explicit repair',
                lane.relation_name;
        end if;

        logical_index_name := lane.relation_name || '_logical_key';
        if lane.is_chunk then
            execute format(
                'create unique index if not exists %I
                 on %I.%I (
                     library_id, chunk_id, revision_id,
                     embedding_model_key, vector_kind, freshness_generation
                 )',
                logical_index_name,
                current_schema(),
                lane.relation_name
            );
        else
            execute format(
                'create unique index if not exists %I
                 on %I.%I (
                     library_id, entity_id,
                     embedding_model_key, vector_kind, freshness_generation
                 )',
                logical_index_name,
                current_schema(),
                lane.relation_name
            );
        end if;
    end loop;
end
$migration$;
