use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// One multi-word entity node eligible for acronym-alias backfill.
#[derive(Debug, Clone, FromRow)]
pub struct AcronymBackfillNodeRow {
    pub id: Uuid,
    pub label: String,
}

/// Lists non-document entity nodes of the active projection whose label is a
/// multi-word phrase, keyset-paginated by `id`. Only multi-word labels can
/// carry a per-word-initials acronym, so single-token labels are filtered out
/// in SQL to keep the backfill scan bounded on large libraries.
///
/// The multi-word predicate is structural (an alphanumeric run, a non-alnum
/// gap, then another alphanumeric run) and script-agnostic.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn list_multiword_runtime_graph_nodes_for_acronym_backfill(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    after_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<AcronymBackfillNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, AcronymBackfillNodeRow>(
        "select n.id, n.label
         from runtime_graph_node n
         where n.library_id = $1
           and n.projection_version = $2
           and n.node_type <> 'document'
           and n.label ~ '[[:alnum:]][^[:alnum:]]+[[:alnum:]]'
           and ($3::uuid is null or n.id > $3)
         order by n.id asc
         limit $4",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Lists non-document entity nodes of the active projection whose label is a
/// single token (no internal non-alphanumeric gap) and is not pure lowercase
/// prose, keyset-paginated by `id`. These are the candidate short-form acronym
/// nodes for the reverse corpus-gloss backfill (a node labelled `AS` gaining the
/// full-form phrase `Alpha Suite` from a `<phrase> ( <short> )` gloss in its own
/// evidence).
///
/// Both structural predicates are script-agnostic. The single-token predicate is
/// the complement of the multi-word predicate used by
/// [`list_multiword_runtime_graph_nodes_for_acronym_backfill`]. A label that is
/// purely lowercase letters can never be identifier-shaped (no separator, digit,
/// or uppercase), so excluding those in SQL is recall-complete while keeping the
/// scan bounded; the precise identifier-shape gate runs caller-side before any
/// evidence is loaded.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn list_short_token_runtime_graph_nodes_for_acronym_backfill(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    after_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<AcronymBackfillNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, AcronymBackfillNodeRow>(
        "select n.id, n.label
         from runtime_graph_node n
         where n.library_id = $1
           and n.projection_version = $2
           and n.node_type <> 'document'
           and n.label !~ '[[:alnum:]][^[:alnum:]]+[[:alnum:]]'
           and n.label !~ '^[[:space:]]*[[:lower:]]+[[:space:]]*$'
           and char_length(btrim(n.label)) >= 2
           and ($3::uuid is null or n.id > $3)
         order by n.id asc
         limit $4",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// One evidence chunk text attached to a graph node.
#[derive(Debug, Clone, FromRow)]
pub struct NodeEvidenceChunkTextRow {
    pub node_id: Uuid,
    pub chunk_text: String,
}

/// Loads up to `per_node_limit` evidence chunk texts for each node in
/// `node_ids`, joining `runtime_graph_evidence` to `content_chunk`. Used by the
/// acronym backfill to re-run the structural detectors over existing data
/// without re-ingesting documents.
///
/// Only chunks that actually contain the node's label as a substring are
/// returned. Both structural detectors require the full-form label to appear in
/// the chunk (the parenthetical gloss is `<label> ( <short> )`; the standalone
/// detector requires the label phrase to be present), so this filter is
/// recall-complete for them while bounding the text volume to the chunks that
/// can possibly carry a gloss — no arbitrary per-node cap that could drop a
/// gloss chunk on a high-evidence node.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn list_runtime_graph_node_evidence_chunk_texts(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<NodeEvidenceChunkTextRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, NodeEvidenceChunkTextRow>(
        "select distinct e.target_id as node_id, c.normalized_text as chunk_text
         from runtime_graph_evidence e
         join runtime_graph_node n
           on n.id = e.target_id
          and n.library_id = $1
          and n.projection_version = $3
         join content_chunk c on c.id = e.chunk_id
         where e.library_id = $1
           and e.target_kind = 'node'
           and e.target_id = any($2::uuid[])
           and e.chunk_id is not null
           and position(lower(n.label) in lower(c.normalized_text)) > 0",
    )
    .bind(library_id)
    .bind(node_ids)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Idempotently unions `new_aliases` into a node's `aliases_json`.
///
/// The update is a no-op (zero rows affected) when every alias in
/// `new_aliases` is already present, so re-running the backfill never
/// duplicates an alias and never churns `updated_at`. Existing aliases are
/// preserved; the merged set is deduplicated and sorted.
///
/// Returns the number of rows updated (0 or 1).
///
/// # Errors
/// Returns any `SQLx` error raised during the update.
pub async fn add_runtime_graph_node_aliases(
    pool: &PgPool,
    node_id: Uuid,
    projection_version: i64,
    new_aliases: &[String],
) -> Result<u64, sqlx::Error> {
    let trimmed: Vec<String> = new_aliases
        .iter()
        .map(|alias| alias.trim().to_string())
        .filter(|alias| !alias.is_empty())
        .collect();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let incoming_json = serde_json::Value::Array(
        trimmed.iter().map(|alias| serde_json::Value::String(alias.clone())).collect(),
    );
    let result = sqlx::query(
        "update runtime_graph_node n
         set aliases_json = (
                 select coalesce(jsonb_agg(distinct value order by value), '[]'::jsonb)
                 from (
                     select btrim(value) as value
                     from jsonb_array_elements_text(
                         case when jsonb_typeof(n.aliases_json) = 'array'
                              then n.aliases_json else '[]'::jsonb end
                     ) as existing(value)
                     where btrim(value) <> ''
                     union
                     select btrim(value) as value
                     from unnest($3::text[]) as incoming(value)
                     where btrim(value) <> ''
                 ) merged
             ),
             updated_at = now()
         where n.id = $1
           and n.projection_version = $2
           and not (coalesce(n.aliases_json, '[]'::jsonb) @> $4::jsonb)",
    )
    .bind(node_id)
    .bind(projection_version)
    .bind(&trimmed)
    .bind(incoming_json)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Recomputes the structural (parenthetical-gloss) acronym alias subset for a
/// node.
///
/// The operation is a **replace-within-slot**, not a plain union:
/// - Any existing alias that is identifier-shaped AND equals `label_initials`
///   (i.e. was written by a previous structural-alias pass) is removed from the
///   stored set if it is absent from `justified_aliases`.
/// - Every alias in `justified_aliases` is added.
/// - All other existing aliases (LLM-extracted, manually set) are preserved.
///
/// This deterministically reverts wrongly attached initials-aliases from
/// earlier passes while keeping aliases from other sources intact. Re-running
/// with the same `justified_aliases` is a no-op.
///
/// `label_initials` must be the uppercased per-word initials of the node label
/// (e.g. `"AS"` for `"Alpha Suite"`). It is used as the discriminating key: an
/// existing alias is only eligible for removal when it matches this value
/// case-insensitively.
///
/// Returns the number of rows updated (0 or 1).
///
/// # Errors
/// Returns any `SQLx` error raised during the update.
pub async fn recompute_runtime_graph_node_structural_aliases(
    pool: &PgPool,
    node_id: Uuid,
    projection_version: i64,
    label_initials: &str,
    justified_aliases: &[String],
) -> Result<u64, sqlx::Error> {
    // Build the expected final set: existing aliases that are NOT the stale
    // initials-slot, plus the newly justified ones.
    // We do this in SQL so it is a single round-trip and consistent even under
    // concurrent writes.
    let upper_initials = label_initials.to_uppercase();
    let justified_trimmed: Vec<String> =
        justified_aliases.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    let justified_json = serde_json::Value::Array(
        justified_trimmed.iter().map(|a| serde_json::Value::String(a.clone())).collect(),
    );
    let result = sqlx::query(
        // $1 = node_id, $2 = projection_version,
        // $3 = upper_initials (the stale-slot discriminator),
        // $4 = justified_trimmed (incoming set as text[]),
        // $5 = justified_json (for the @> no-op guard on the incoming side)
        "update runtime_graph_node n
         set aliases_json = (
                 select coalesce(jsonb_agg(distinct value order by value), '[]'::jsonb)
                 from (
                     -- keep existing aliases that are NOT the initials slot
                     select btrim(value) as value
                     from jsonb_array_elements_text(
                         case when jsonb_typeof(n.aliases_json) = 'array'
                              then n.aliases_json else '[]'::jsonb end
                     ) as existing(value)
                     where btrim(value) <> ''
                       and upper(btrim(value)) <> $3
                     union
                     -- add the freshly justified aliases
                     select btrim(value) as value
                     from unnest($4::text[]) as incoming(value)
                     where btrim(value) <> ''
                 ) merged
             ),
             updated_at = now()
         where n.id = $1
           and n.projection_version = $2
           and (
               -- there is a stale initials-alias that needs removal
               exists (
                   select 1
                   from jsonb_array_elements_text(
                       case when jsonb_typeof(n.aliases_json) = 'array'
                            then n.aliases_json else '[]'::jsonb end
                   ) as existing(value)
                   where upper(btrim(value)) = $3
                     and not ($5::jsonb @> jsonb_build_array(btrim(value)))
               )
               or
               -- there is a justified alias not yet in the stored set
               not (coalesce(n.aliases_json, '[]'::jsonb) @> $5::jsonb)
           )",
    )
    .bind(node_id)
    .bind(projection_version)
    .bind(&upper_initials)
    .bind(&justified_trimmed)
    .bind(justified_json)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Recomputes the structural (parenthetical-gloss) **full-form** alias subset
/// for a short-token node — the reverse of
/// [`recompute_runtime_graph_node_structural_aliases`].
///
/// The forward pass attaches the short acronym to a multi-word node and keys the
/// stale slot by *literal equality* with the acronym. This reverse pass attaches
/// the full-form phrase to a node whose label *is* the short acronym and keys the
/// stale slot by *per-word initials*: an existing alias is part of the recomputed
/// slot iff its uppercased per-word initials equal `short_label_upper`.
///
/// - Any existing alias whose initials equal `short_label_upper` is removed if it
///   is absent from `justified_aliases` (the gloss that justified it is gone).
/// - Every alias in `justified_aliases` is added.
/// - All other existing aliases (single-token, foreign-initials, LLM-extracted,
///   manually set) are preserved — `is distinct from` keeps NULL-initials values.
///
/// `short_label_upper` must be the uppercased node label (e.g. `"AS"`). The two
/// passes never run on the same node: the forward pass scans multi-word labels,
/// this one scans single-token labels. Re-running with the same justified set is
/// a no-op.
///
/// Returns the number of rows updated (0 or 1).
///
/// # Errors
/// Returns any `SQLx` error raised during the update.
pub async fn recompute_runtime_graph_node_fullform_aliases(
    pool: &PgPool,
    node_id: Uuid,
    projection_version: i64,
    justified_aliases: &[String],
) -> Result<u64, sqlx::Error> {
    let justified_trimmed: Vec<String> =
        justified_aliases.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    if justified_trimmed.is_empty() {
        return Ok(0);
    }
    let justified_json = serde_json::Value::Array(
        justified_trimmed.iter().map(|a| serde_json::Value::String(a.clone())).collect(),
    );
    // ADD-ONLY by design: the reverse direction unions gloss-justified
    // full-form aliases into the node's alias set and never removes anything.
    // Unlike the forward slot (the exact short string, unambiguously owned by
    // the gloss capture), a "full-form" slot has no safe ownership marker — a
    // removal pass keyed on per-word initials was observed stripping
    // legitimate extraction-derived aliases of OTHER senses on polysemous
    // short-labeled nodes. A stale gloss-derived full form lingering after a
    // gloss disappears is the lesser harm and is corrected by re-ingest.
    let result = sqlx::query(
        // $1 = node_id, $2 = projection_version,
        // $3 = justified aliases as text[], $4 = same as jsonb (no-op guard).
        "update runtime_graph_node n
         set aliases_json = (
                 select coalesce(jsonb_agg(distinct value order by value), '[]'::jsonb)
                 from (
                     select btrim(value) as value
                     from jsonb_array_elements_text(
                         case when jsonb_typeof(n.aliases_json) = 'array'
                              then n.aliases_json else '[]'::jsonb end
                     ) as existing(value)
                     where btrim(value) <> ''
                     union
                     select btrim(value) as value
                     from unnest($3::text[]) as incoming(value)
                     where btrim(value) <> ''
                 ) merged
             ),
             updated_at = now()
         where n.id = $1
           and n.projection_version = $2
           and not (coalesce(n.aliases_json, '[]'::jsonb) @> $4::jsonb)",
    )
    .bind(node_id)
    .bind(projection_version)
    .bind(&justified_trimmed)
    .bind(justified_json)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
