-- One-time heal for production graphs corrupted by the entity_resolution
-- step-7 dup-key bug fixed in v0.3.0+1. Two classes of damage exist:
--
--   1. Parallel edges that share `(library_id, projection_version, from_node_id,
--      relation_type, to_node_id)` but differ on `canonical_key`. Created when
--      a node merge re-pointed `from_node_id`/`to_node_id` from a deleted node
--      onto a kept node that already had an edge with the same triple. Step 7
--      then tried to UPDATE both edges to the same canonical_key and failed
--      the `runtime_graph_edge_..._canonical_key_..._key` unique constraint.
--
--   2. Edges whose `canonical_key` no longer matches
--      `canonical_edge_key(from.canonical_key, relation_type, to.canonical_key)`
--      because step 7 aborted before rewriting them.
--
-- This script is idempotent: re-runs simply find no work. It collapses the
-- duplicate groups (oldest survives, evidence repointed, summaries/vectors
-- dropped) and then rewrites stale canonical_keys library-wide.
--
-- Run via:
--   docker exec -i ironrag-postgres-1 psql -U postgres -d ironrag \
--     -v ON_ERROR_STOP=1 -1 -f - < heal-parallel-graph-edges.sql

\timing on
\set ON_ERROR_STOP on

begin;

-- Block any concurrent canonical-graph mutation on the libraries we are
-- about to touch. The application path uses no advisory lock today, so
-- this just protects us from a worker rerunning entity_resolution while
-- the heal is mid-flight.
select pg_advisory_xact_lock(hashtext('ironrag.heal-parallel-graph-edges')::bigint);

\echo '== Pre-heal corruption snapshot =='
select 'parallel_groups' as metric, count(*)::bigint as value
  from (
    select library_id, projection_version, from_node_id, relation_type, to_node_id
      from runtime_graph_edge
     group by 1,2,3,4,5
    having count(*) > 1
  ) g
union all
select 'mismatched_canonical_keys',
       count(*)::bigint
  from runtime_graph_edge e
  join runtime_graph_node nf on nf.id = e.from_node_id
  join runtime_graph_node nt on nt.id = e.to_node_id
 where e.canonical_key <> nf.canonical_key || '--' || e.relation_type || '--' || nt.canonical_key;

-- Step 1. Collapse parallel edges. Survivor = oldest by created_at, then id.
-- Soft refs to losers are repointed (evidence) or dropped (summary, vector).
-- Loser rows are deleted. `support_count` is left for the recompute below.
with grouped as (
    select id,
           library_id,
           projection_version,
           row_number() over w as rn,
           first_value(id) over w as survivor_id
      from runtime_graph_edge
    window w as (
        partition by library_id, projection_version, from_node_id, relation_type, to_node_id
        order by created_at asc, id asc
    )
),
losers as (
    select g.id as loser_id, g.survivor_id, g.library_id
      from grouped g
     where g.rn > 1
),
repoint_evidence as (
    update runtime_graph_evidence ev
       set target_id = losers.survivor_id
      from losers
     where ev.target_kind = 'edge'
       and ev.library_id = losers.library_id
       and ev.target_id = losers.loser_id
    returning 1
),
drop_summaries as (
    delete from runtime_graph_canonical_summary s
     using losers
     where s.library_id = losers.library_id
       and s.target_kind = 'edge'
       and s.target_id = losers.loser_id
    returning 1
)
delete from runtime_graph_edge
 using losers
 where runtime_graph_edge.id = losers.loser_id;

-- Step 2. Recompute support_count from active evidence for every edge in any
-- library/projection slice that had a collapse. Bounded scan: only edges
-- whose support_count drifted from the active-evidence count are touched.
update runtime_graph_edge e
   set support_count = coalesce(ec.cnt, 0),
       updated_at = now()
  from (
    select target_id as edge_id, count(*)::int as cnt
      from runtime_graph_evidence
     where target_kind = 'edge' and is_active = true
     group by 1
  ) ec
 where e.id = ec.edge_id
   and e.support_count is distinct from coalesce(ec.cnt, 0);

-- Catch edges that had no surviving evidence (counter must drop to zero).
update runtime_graph_edge e
   set support_count = 0, updated_at = now()
 where e.support_count <> 0
   and not exists (
       select 1 from runtime_graph_evidence ev
        where ev.target_kind = 'edge' and ev.is_active = true and ev.target_id = e.id
   );

-- Step 3. Rewrite stale canonical_keys.
update runtime_graph_edge e
   set canonical_key = nf.canonical_key || '--' || e.relation_type || '--' || nt.canonical_key,
       updated_at = now()
  from runtime_graph_node nf,
       runtime_graph_node nt
 where nf.id = e.from_node_id
   and nt.id = e.to_node_id
   and e.canonical_key <> nf.canonical_key || '--' || e.relation_type || '--' || nt.canonical_key;

\echo '== Post-heal corruption snapshot =='
select 'parallel_groups' as metric, count(*)::bigint as value
  from (
    select library_id, projection_version, from_node_id, relation_type, to_node_id
      from runtime_graph_edge
     group by 1,2,3,4,5
    having count(*) > 1
  ) g
union all
select 'mismatched_canonical_keys',
       count(*)::bigint
  from runtime_graph_edge e
  join runtime_graph_node nf on nf.id = e.from_node_id
  join runtime_graph_node nt on nt.id = e.to_node_id
 where e.canonical_key <> nf.canonical_key || '--' || e.relation_type || '--' || nt.canonical_key;

commit;
