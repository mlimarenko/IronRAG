use super::*;

const LEGACY_EXISTS_FIRST_HEAD_SQL: &str = "SELECT chunk_id, library_id, document_id
             FROM knowledge_chunk c
             WHERE c.revision_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision r
                   WHERE r.revision_id = c.revision_id
                     AND r.workspace_id = c.workspace_id
                     AND r.library_id = c.library_id
                     AND r.document_id = c.document_id
                     AND r.preparation_state = 'prepared'
               )
             ORDER BY chunk_index ASC, chunk_id ASC
             LIMIT $2";

const LEGACY_EXISTS_FIRST_TAIL_SQL: &str = "SELECT chunk_id
             FROM (
               SELECT *
               FROM knowledge_chunk
               WHERE revision_id = $1
                 AND chunk_state = 'ready'
                 AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision r
                   WHERE r.revision_id = knowledge_chunk.revision_id
                     AND r.workspace_id = knowledge_chunk.workspace_id
                     AND r.library_id = knowledge_chunk.library_id
                     AND r.document_id = knowledge_chunk.document_id
                     AND r.preparation_state = 'prepared'
                 )
               ORDER BY chunk_index DESC, chunk_id DESC
               LIMIT $2
             ) c
             ORDER BY chunk_index ASC, chunk_id ASC";

const LEGACY_EXISTS_FIRST_WINDOWS_SQL: &str = "with windowed as materialized (
               select c.chunk_id, c.workspace_id, c.library_id, c.document_id,
                      c.revision_id, c.chunk_index
               from knowledge_chunk c
               where c.revision_id = $1
                 and exists (
                   select 1
                   from knowledge_structured_revision r
                   where r.revision_id = c.revision_id
                     and r.workspace_id = c.workspace_id
                     and r.library_id = c.library_id
                     and r.document_id = c.document_id
                     and r.preparation_state = 'prepared'
                 )
                 and exists (
                   select 1
                   from unnest($2::int4[], $3::int4[]) as w(min_index, max_index)
                   where c.chunk_index between w.min_index and w.max_index
                 )
               order by c.chunk_index asc, c.chunk_id asc
               limit $4
             )
             select *
             from windowed c
             order by c.chunk_index asc, c.chunk_id asc";

fn compact_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

fn cte_where_clause(cte: &str) -> Option<String> {
    let compact = compact_sql(cte);
    let where_at = compact.find(" where ")?;
    let order_at = compact[where_at..].find(" order by ").map(|relative| where_at + relative)?;
    Some(compact[where_at..order_at].to_string())
}

fn prepared_revision_identity_gate(sql: &str) -> bool {
    let compact = compact_sql(sql);
    [
        "r.revision_id = c.revision_id",
        "r.workspace_id = c.workspace_id",
        "r.library_id = c.library_id",
        "r.document_id = c.document_id",
        "r.preparation_state = 'prepared'",
    ]
    .iter()
    .all(|predicate| compact.contains(predicate))
}

fn revision_index_first_bounded_prefix(sql: &str, cte_name: &str) -> bool {
    let compact = compact_sql(sql);
    let marker = format!("{cte_name} as materialized (");
    let lowered = sql.to_ascii_lowercase();
    let Some(marker_at) = lowered.find(&marker) else {
        return false;
    };
    if compact.contains("knowledge_chunk_library_document_index") {
        return false;
    }
    let cte = materialized_cte_body(sql, cte_name);
    let cte_compact = compact_sql(&cte);
    let Some(where_clause) = cte_where_clause(&cte) else {
        return false;
    };
    let after_cte = compact_sql(&sql[marker_at + marker.len() + cte.len() + 1..]);
    where_clause.contains("c.revision_id = $1")
        && !where_clause.contains("library_id")
        && !where_clause.contains("document_id")
        && !where_clause.contains("exists")
        && cte_compact.contains("limit $2")
        && !cte_compact.contains("exists (")
        && after_cte.contains("exists (")
        && prepared_revision_identity_gate(&after_cte)
        && after_cte.contains("order by c.chunk_index asc, c.chunk_id asc")
}

fn revision_index_first_windowed_fetch(
    sql: &str,
    revision_predicate: &str,
    window_predicate: &str,
    outer_order: &str,
) -> bool {
    let lowered = sql.to_ascii_lowercase();
    let marker = "windowed as materialized (";
    let Some(marker_at) = lowered.find(marker) else {
        return false;
    };
    let cte = materialized_cte_body(sql, "windowed");
    let cte_compact = compact_sql(&cte);
    let Some(predicates) = cte_where_clause(&cte) else {
        return false;
    };
    let after_cte = compact_sql(&sql[marker_at + marker.len() + cte.len() + 1..]);

    cte_compact.contains("from knowledge_chunk c")
        && predicates.contains(revision_predicate)
        && predicates.contains(window_predicate)
        && cte_compact.contains("limit $4")
        && !cte_compact.contains("knowledge_structured_revision")
        && !cte_compact.contains("preparation_state")
        && !predicates.contains("c.library_id")
        && !predicates.contains("c.document_id")
        && !predicates.contains("chunk_state")
        && after_cte.contains("from windowed c")
        && after_cte.contains("knowledge_structured_revision r")
        && prepared_revision_identity_gate(&after_cte)
        && after_cte.contains(outer_order)
}

fn direct_single_window_ranges(sql: &str) -> bool {
    let compact = compact_sql(sql);
    let windowed = compact_sql(&materialized_cte_body(sql, "windowed"));
    compact.contains("from requested_windows w cross join lateral")
        && compact.contains(
            "where c.revision_id = $1 and c.chunk_index between w.min_index and w.max_index",
        )
        && windowed.contains("order by c.chunk_index asc, c.chunk_id asc limit $4")
        && windowed.contains("order by picked.chunk_index asc, picked.chunk_id asc limit $4")
        && !compact.contains("where c.revision_id = $1 and exists")
}

fn direct_multi_window_ranges(sql: &str) -> bool {
    let compact = compact_sql(sql);
    compact.contains(
        "range_agg( int8range(min_index::bigint, max_index::bigint, '[]') ) as windows",
    )
        && compact.contains("cross join lateral")
        && compact.contains(
            "where c.revision_id = merged_windows.revision_id and c.chunk_index >= lower(merged_windows.chunk_range) and c.chunk_index < upper(merged_windows.chunk_range)",
        )
        && !compact.contains("and exists ( select 1 from requested_windows")
}

#[test]
fn revision_head_sql_starts_from_revision_id_then_prepared_gate() {
    let sql = list_head_chunks_by_revision_sql();
    let compact = compact_sql(&sql);
    let cte = compact_sql(&materialized_cte_body(&sql, "head"));

    assert!(revision_index_first_bounded_prefix(&sql, "head"));
    assert!(
        cte_where_clause(&materialized_cte_body(&sql, "head"))
            .is_some_and(|where_clause| !where_clause.contains("chunk_state"))
    );
    assert!(cte.contains("order by c.chunk_index asc, c.chunk_id asc"));
    assert!(cte.contains("limit $2"));
    assert!(!cte.contains("exists ("));
    assert!(compact.contains("from head c"));
    assert!(!compact.contains("plan_cache_mode"));
    assert!(!compact.contains("to_tsquery"));
    assert!(!compact.contains("search_tsv"));
}

#[test]
fn revision_tail_sql_is_the_desc_twin_of_head() {
    let sql = list_tail_chunks_by_revision_sql();
    let compact = compact_sql(&sql);
    let cte = compact_sql(&materialized_cte_body(&sql, "tail"));
    let where_clause = cte_where_clause(&materialized_cte_body(&sql, "tail"));

    assert!(revision_index_first_bounded_prefix(&sql, "tail"));
    assert!(
        where_clause.is_some_and(|where_clause| where_clause.contains("c.chunk_state = 'ready'"))
    );
    assert!(cte.contains("order by c.chunk_index desc, c.chunk_id desc"));
    assert!(cte.contains("limit $2"));
    assert!(compact.contains("from tail c"));
    assert!(compact.contains("order by c.chunk_index asc, c.chunk_id asc"));
    assert!(!compact.contains("plan_cache_mode"));
}

#[test]
fn planted_exists_first_head_sql_fails_even_with_limit_400() {
    assert!(
        !revision_index_first_bounded_prefix(LEGACY_EXISTS_FIRST_HEAD_SQL, "head"),
        "EXISTS-first plus LIMIT 400 is the ~7.5s Nested Loop through knowledge_chunk_library_document_index"
    );
}

#[test]
fn planted_library_id_in_inner_where_fails_the_revision_index_gate() {
    let planted = list_head_chunks_by_revision_sql().replace(
        "where c.revision_id = $1",
        "where c.revision_id = $1 and c.library_id = $3 and c.document_id = $4",
    );
    assert!(
        !revision_index_first_bounded_prefix(&planted, "head"),
        "library_id/document_id in the inner WHERE restores the library-document btree"
    );
}

#[test]
fn planted_exists_inside_materialized_head_fails_the_revision_index_gate() {
    let planted = list_head_chunks_by_revision_sql().replace(
        "where c.revision_id = $1",
        "where c.revision_id = $1 and exists ( select 1 from knowledge_structured_revision r where r.library_id = c.library_id )",
    );
    assert!(
        !revision_index_first_bounded_prefix(&planted, "head"),
        "EXISTS inside the prefix CTE is the current helper's planner leak"
    );
}

#[test]
fn planted_head_without_outer_order_by_fails_the_revision_index_gate() {
    let sql = list_head_chunks_by_revision_sql();
    let needle = "order by c.chunk_index asc, c.chunk_id asc";
    assert!(sql.contains(needle), "outer order by must exist before planting");
    let last = sql.rfind(needle).unwrap_or_default();
    let planted = format!("{}{}", &sql[..last], &sql[last + needle.len()..]);
    assert!(
        !revision_index_first_bounded_prefix(&planted, "head"),
        "Hash Semi Join on EXISTS is not reading order; outer ORDER BY is the contract"
    );
}

#[test]
fn planted_ready_filter_on_head_is_visible_to_the_parity_gate() {
    let planted = list_head_chunks_by_revision_sql().replace(
        "where c.revision_id = $1",
        "where c.revision_id = $1 and c.chunk_state = 'ready'",
    );
    assert!(
        cte_where_clause(&materialized_cte_body(&planted, "head"))
            .is_some_and(|where_clause| where_clause.contains("chunk_state")),
        "the plant must actually add chunk_state"
    );
    assert!(
        cte_where_clause(&materialized_cte_body(&list_head_chunks_by_revision_sql(), "head"))
            .is_some_and(|where_clause| !where_clause.contains("chunk_state")),
        "head must keep unlimited-list parity: no chunk_state filter"
    );
}

#[test]
fn planted_exists_first_tail_sql_fails_the_revision_index_gate() {
    assert!(
        !revision_index_first_bounded_prefix(LEGACY_EXISTS_FIRST_TAIL_SQL, "tail"),
        "DESC LIMIT with correlated library/document EXISTS is the same leak as head"
    );
}

#[test]
fn bounded_revision_fetches_materialize_revision_windows_before_prepared_gate() {
    let range_sql = list_chunks_by_revision_range_sql();
    assert!(revision_index_first_windowed_fetch(
        &range_sql,
        "c.revision_id = $1",
        "c.chunk_index between $2 and $3",
        "order by c.chunk_index asc, c.chunk_id asc",
    ));

    let windows_sql = list_chunks_by_revision_windows_sql();
    assert!(revision_index_first_windowed_fetch(
        &windows_sql,
        "c.revision_id = $1",
        "c.chunk_index between w.min_index and w.max_index",
        "order by c.chunk_index asc, c.chunk_id asc",
    ));
    assert!(compact_sql(&windows_sql).contains("unnest($2::int4[], $3::int4[])"));

    let multi_sql = list_chunks_by_revisions_windows_sql();
    assert!(revision_index_first_windowed_fetch(
        &multi_sql,
        "c.revision_id = merged_windows.revision_id",
        "c.chunk_index >= lower(merged_windows.chunk_range)",
        "order by c.ordinal asc, c.chunk_index asc, c.chunk_id asc",
    ));
    let multi_compact = compact_sql(&multi_sql);
    assert!(multi_compact.contains("cross join lateral"));
    assert!(multi_compact.contains("min(ordinal) as ordinal"));
    assert!(multi_compact.contains("row_number() over ( partition by picked.revision_id"));
    assert!(multi_compact.contains("where c.revision_rank <= $4"));
}

#[test]
fn bounded_window_fetches_use_direct_parameterized_chunk_ranges() {
    assert!(
        direct_single_window_ranges(&list_chunks_by_revision_windows_sql()),
        "single-revision windows must parameterize one revision-index range scan per window"
    );
    assert!(
        direct_multi_window_ranges(&list_chunks_by_revisions_windows_sql()),
        "multi-revision overlaps must be merged into parameterized index ranges before the cap"
    );
}

#[test]
fn planted_correlated_window_exists_fails_the_direct_range_gate() {
    let sql = list_chunks_by_revision_windows_sql();
    let needle = "and c.chunk_index between w.min_index and w.max_index";
    assert_eq!(sql.matches(needle).count(), 1, "plant target must be unique");
    let planted = sql.replace(
        needle,
        "and exists ( select 1 from requested_windows candidate_window where c.chunk_index between candidate_window.min_index and candidate_window.max_index )",
    );
    assert!(planted.contains("and exists ( select 1 from requested_windows candidate_window"));
    assert!(
        !direct_single_window_ranges(&planted),
        "a correlated window EXISTS must not pass as a bounded revision-index range scan"
    );
}

#[test]
fn planted_uncorrelated_prepared_exists_fails_the_identity_gate() {
    let sql = list_chunks_by_revision_range_sql();
    assert_eq!(
        sql.matches(PREPARED_REVISION_CHUNK_GATE_SQL).count(),
        1,
        "plant target must be the shared prepared identity gate"
    );
    let planted = sql.replace(
        PREPARED_REVISION_CHUNK_GATE_SQL,
        "exists ( select 1 from knowledge_structured_revision r where r.preparation_state = 'prepared' )",
    );
    assert!(planted.contains("knowledge_structured_revision r"));
    assert!(
        !revision_index_first_windowed_fetch(
            &planted,
            "c.revision_id = $1",
            "c.chunk_index between $2 and $3",
            "order by c.chunk_index asc, c.chunk_id asc",
        ),
        "an unrelated prepared revision must not authorize bounded chunks"
    );
}

#[test]
fn planted_single_windows_without_global_cap_fails_the_bounded_gate() {
    let sql = list_chunks_by_revision_windows_sql();
    let needle = "order by picked.chunk_index asc, picked.chunk_id asc\n           limit $4";
    assert_eq!(sql.matches(needle).count(), 1, "plant target must be the global cap");
    let planted = sql.replace(needle, "order by picked.chunk_index asc, picked.chunk_id asc");
    assert_eq!(planted.matches("limit $4").count(), 1, "the lateral cap must remain");
    assert!(
        !direct_single_window_ranges(&planted),
        "per-window limits do not replace the global 2000-row prefix cap"
    );
}

#[test]
fn planted_exists_first_windows_sql_fails_even_with_limit() {
    assert!(LEGACY_EXISTS_FIRST_WINDOWS_SQL.contains("limit $4"), "plant includes the limit");
    assert!(
        !revision_index_first_windowed_fetch(
            LEGACY_EXISTS_FIRST_WINDOWS_SQL,
            "c.revision_id = $1",
            "c.chunk_index between w.min_index and w.max_index",
            "order by c.chunk_index asc, c.chunk_id asc",
        ),
        "LIMIT cannot rescue a materialized CTE whose prepared gate exposes library/document"
    );
}

#[test]
fn planted_library_document_predicates_inside_windowed_fail_the_revision_index_gate() {
    let sql = list_chunks_by_revision_range_sql();
    let needle = "where c.revision_id = $1";
    assert_eq!(sql.matches(needle).count(), 1, "plant target must be unique");
    let planted = sql
        .replace(needle, "where c.revision_id = $1 and c.library_id = $5 and c.document_id = $6");
    assert!(planted.contains("c.library_id = $5"), "plant must land in the predicate");
    assert!(
        !revision_index_first_windowed_fetch(
            &planted,
            "c.revision_id = $1",
            "c.chunk_index between $2 and $3",
            "order by c.chunk_index asc, c.chunk_id asc",
        ),
        "library/document predicates inside the CTE restore the wrong access path"
    );
}

#[test]
fn planted_bounded_fetch_without_outer_order_fails_the_contract_gate() {
    let sql = list_chunks_by_revision_windows_sql();
    let needle = "order by c.chunk_index asc, c.chunk_id asc";
    assert_eq!(sql.matches(needle).count(), 2, "lateral and outer order must both exist");
    assert!(sql.contains(needle), "outer order by must exist before planting");
    let last = sql.rfind(needle).unwrap_or_default();
    let planted = format!("{}{}", &sql[..last], &sql[last + needle.len()..]);
    assert_eq!(planted.matches(needle).count(), 1, "only the outer order is removed");
    assert!(
        !revision_index_first_windowed_fetch(
            &planted,
            "c.revision_id = $1",
            "c.chunk_index between w.min_index and w.max_index",
            needle,
        ),
        "the prepared semi-join cannot inherit the CTE's reading order"
    );
}
