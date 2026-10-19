use anyhow::Context;

use super::{
    AppState, BTreeMap, BTreeSet, HashMap, HashSet, KnowledgeChunkRow, KnowledgeDocumentRow,
    LATEST_VERSION_SEMANTIC_CHUNK_CAP,
    LATEST_VERSION_SEMANTIC_UNSCOPED_IDENTITY_DOCUMENT_CAP_MULTIPLIER,
    LATEST_VERSION_SEMANTIC_UNSCOPED_ROWS_PER_DOCUMENT, LATEST_VERSION_STRUCTURAL_DENSITY_MIN_ROWS,
    LATEST_VERSION_STRUCTURAL_DENSITY_REGEX, LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS,
    ReleaseSourceIdentity, Uuid, chunk_answer_source_text, compare_version_desc,
    extract_config_assignment_literals, extract_explicit_path_literals, extract_parameter_literals,
    extract_release_context_version, extract_semver_like_version, is_source_profile_chunk_row,
    strip_leading_procedure_order_marker, text_has_release_version_marker,
};

#[derive(Clone)]
pub(super) struct LatestVersionSemanticRow {
    pub(super) version: Vec<u32>,
    pub(super) document_rank: usize,
    pub(super) structural_density_score: usize,
    pub(super) from_structural_inventory: bool,
    pub(super) row: KnowledgeChunkRow,
}

pub(super) fn order_latest_version_semantic_rows(
    mut rows: Vec<LatestVersionSemanticRow>,
    source_tail_inventory: bool,
    requested_count: usize,
) -> Vec<LatestVersionSemanticRow> {
    if !source_tail_inventory {
        rows.sort_by(|left, right| {
            compare_version_desc(&left.version, &right.version)
                .then_with(|| right.from_structural_inventory.cmp(&left.from_structural_inventory))
                .then_with(|| right.structural_density_score.cmp(&left.structural_density_score))
                .then_with(|| left.document_rank.cmp(&right.document_rank))
                .then_with(|| left.row.chunk_index.cmp(&right.row.chunk_index))
                .then_with(|| left.row.chunk_id.cmp(&right.row.chunk_id))
        });
        return rows;
    }

    let identity_prefix_cap = requested_count
        .saturating_mul(LATEST_VERSION_SEMANTIC_UNSCOPED_IDENTITY_DOCUMENT_CAP_MULTIPLIER)
        .clamp(requested_count.max(1), LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    let mut identity_candidates =
        rows.iter().filter(|row| !row.from_structural_inventory).cloned().collect::<Vec<_>>();
    identity_candidates.sort_by(|left, right| {
        compare_version_desc(&left.version, &right.version)
            .then_with(|| left.document_rank.cmp(&right.document_rank))
            .then_with(|| right.row.chunk_index.cmp(&left.row.chunk_index))
            .then_with(|| left.row.chunk_id.cmp(&right.row.chunk_id))
    });
    let mut identity_prefix = Vec::new();
    let mut identity_documents = HashSet::<Uuid>::new();
    for row in identity_candidates {
        if identity_prefix.len() >= identity_prefix_cap {
            break;
        }
        if identity_documents.insert(row.row.document_id) {
            identity_prefix.push(row);
        }
    }

    let mut per_document = BTreeMap::<Uuid, Vec<LatestVersionSemanticRow>>::new();
    for row in rows {
        per_document.entry(row.row.document_id).or_default().push(row);
    }
    let mut documents = per_document
        .iter()
        .map(|(document_id, rows)| {
            let from_structural_inventory = rows.iter().any(|row| row.from_structural_inventory);
            let structural_density_score =
                rows.iter().map(|row| row.structural_density_score).max().unwrap_or_default();
            let document_rank = rows.iter().map(|row| row.document_rank).min().unwrap_or_default();
            (*document_id, from_structural_inventory, structural_density_score, document_rank)
        })
        .collect::<Vec<_>>();
    documents.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.cmp(&right.0))
    });

    for rows in per_document.values_mut() {
        rows.sort_by(|left, right| {
            compare_version_desc(&left.version, &right.version)
                .then_with(|| right.row.chunk_index.cmp(&left.row.chunk_index))
                .then_with(|| left.row.chunk_id.cmp(&right.row.chunk_id))
        });
    }

    let rows_per_document = LATEST_VERSION_SEMANTIC_UNSCOPED_ROWS_PER_DOCUMENT
        .clamp(1, LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    let document_pool_limit =
        requested_count.clamp(4, LATEST_VERSION_SEMANTIC_CHUNK_CAP).min(documents.len());
    let (primary_documents, secondary_documents) = documents.split_at(document_pool_limit);
    let mut ordered = Vec::new();
    for row_offset in 0..rows_per_document {
        for (document_id, _, _, _) in primary_documents {
            let Some(rows) = per_document.get(document_id) else {
                continue;
            };
            let Some(row) = rows.get(row_offset) else {
                continue;
            };
            ordered.push(row.clone());
        }
    }
    ordered.extend(identity_prefix);
    for (document_id, _, _, _) in primary_documents {
        let Some(rows) = per_document.get(document_id) else {
            continue;
        };
        ordered.extend(rows.iter().skip(rows_per_document).cloned());
    }
    for (document_id, _, _, _) in secondary_documents {
        let Some(rows) = per_document.get(document_id) else {
            continue;
        };
        ordered.extend(rows.iter().cloned());
    }
    ordered
}

pub(super) fn latest_version_semantic_rows_from_chunk_rows(
    chunk_rows: Vec<KnowledgeChunkRow>,
    document_rank: usize,
    from_structural_inventory: bool,
) -> (Vec<LatestVersionSemanticRow>, BTreeSet<Vec<u32>>, usize) {
    let mut rows = Vec::new();
    let mut distinct_versions = BTreeSet::<Vec<u32>>::new();
    let mut structural_score = 0usize;
    for row in chunk_rows.into_iter().filter(|row| !is_source_profile_chunk_row(row)) {
        let source_text = chunk_answer_source_text(&row);
        let mut versions = latest_version_context_versions(&source_text);
        if versions.is_empty() {
            continue;
        }
        versions.sort_by(|left, right| compare_version_desc(left, right));
        distinct_versions.extend(versions.iter().cloned());
        structural_score = structural_score
            .saturating_add(latest_version_structural_inventory_text_score(&source_text));
        rows.push(LatestVersionSemanticRow {
            version: versions[0].clone(),
            document_rank,
            structural_density_score: 0,
            from_structural_inventory,
            row,
        });
    }
    (rows, distinct_versions, structural_score)
}

#[derive(Clone)]
pub(super) struct LatestVersionSemanticDocument {
    pub(super) document_id: Uuid,
    pub(super) revision_id: Uuid,
    pub(super) semantic_score: usize,
    pub(super) identity_text: String,
    pub(super) requires_structural_release_density: bool,
}

pub(super) fn latest_version_semantic_candidate_documents(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    limit: usize,
) -> Vec<LatestVersionSemanticDocument> {
    if limit == 0 {
        return Vec::new();
    }
    let rows = document_index
        .values()
        .filter(|document| latest_version_semantic_document_is_content_candidate(document))
        .filter_map(|document| {
            let identity_text = latest_version_document_identity_text(document);
            let semantic_score =
                latest_version_identity_structural_score(&identity_text, scope_terms);
            if semantic_score == 0 {
                return None;
            }
            let revision_id = document.readable_revision_id?;
            Some(LatestVersionSemanticDocument {
                document_id: document.document_id,
                revision_id,
                semantic_score,
                identity_text,
                requires_structural_release_density: false,
            })
        })
        .collect::<Vec<_>>();
    let scoped_rows = if scope_terms.is_empty() {
        rows
    } else {
        let scoped = rows
            .iter()
            .filter(|candidate| {
                scope_terms.iter().any(|term| candidate.identity_text.contains(term))
            })
            .cloned()
            .collect::<Vec<_>>();
        if scoped.is_empty() { rows } else { scoped }
    };
    let mut rows = scoped_rows;
    rows.sort_by(|left, right| {
        right
            .semantic_score
            .cmp(&left.semantic_score)
            .then_with(|| left.identity_text.cmp(&right.identity_text))
            .then_with(|| left.document_id.cmp(&right.document_id))
    });
    rows.truncate(limit);
    rows
}

pub(super) fn latest_version_identity_structural_score(
    identity_text: &str,
    scope_terms: &[String],
) -> usize {
    let mut score = 0usize;
    if extract_semver_like_version(identity_text).is_some() {
        score = score.saturating_add(512);
    }
    let scope_overlap =
        scope_terms.iter().filter(|term| identity_text.contains(term.as_str())).count();
    score
        .saturating_add(scope_overlap.saturating_mul(1024))
        .saturating_add(usize::from(scope_overlap > 0).saturating_mul(128))
}

pub(super) fn latest_version_structural_probe_candidate_documents(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    excluded_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> Vec<LatestVersionSemanticDocument> {
    if limit == 0 {
        return Vec::new();
    }
    let mut rows = document_index
        .values()
        .filter(|document| latest_version_semantic_document_is_content_candidate(document))
        .filter(|document| !excluded_document_ids.contains(&document.document_id))
        .filter_map(|document| {
            let revision_id = document.readable_revision_id?;
            let identity_text = latest_version_document_identity_text(document);
            let scope_overlap =
                scope_terms.iter().filter(|term| identity_text.contains(term.as_str())).count();
            if !scope_terms.is_empty() && scope_overlap == 0 {
                return None;
            }
            let semantic_score = scope_overlap
                .saturating_mul(1024)
                .saturating_add(document.latest_revision_no.unwrap_or_default() as usize);
            Some((
                document.updated_at,
                document.latest_revision_no.unwrap_or_default(),
                LatestVersionSemanticDocument {
                    document_id: document.document_id,
                    revision_id,
                    semantic_score,
                    identity_text,
                    requires_structural_release_density: true,
                },
            ))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| right.2.semantic_score.cmp(&left.2.semantic_score))
            .then_with(|| left.2.identity_text.cmp(&right.2.identity_text))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    rows.truncate(limit);
    rows.into_iter().map(|(_, _, document)| document).collect()
}

#[derive(sqlx::FromRow)]
pub(super) struct LatestVersionStructuralDensityCandidateRow {
    pub(super) document_id: Uuid,
    pub(super) revision_id: Uuid,
    pub(super) version_row_count: i64,
    pub(super) max_version_chunk_index: Option<i32>,
}

pub(super) const LATEST_VERSION_STRUCTURAL_DENSITY_CANDIDATES_SQL: &str =
    "with candidate_revisions as (
       select distinct document_id, revision_id
       from unnest($2::uuid[], $6::uuid[]) as candidate(document_id, revision_id)
     ),
     candidate_documents as (
       select d.document_id, d.workspace_id, d.library_id,
              d.readable_revision_id as revision_id
       from candidate_revisions candidate
       join knowledge_document d
         on d.document_id = candidate.document_id
        and d.readable_revision_id = candidate.revision_id
       join knowledge_structured_revision sr
         on sr.revision_id = candidate.revision_id
        and sr.document_id = d.document_id
        and sr.workspace_id = d.workspace_id
        and sr.library_id = d.library_id
        and sr.preparation_state = 'prepared'
       where d.library_id = $1
         and d.document_state = 'active'
         and d.readable_revision_id is not null
         and d.deleted_at is null
     ),
     chunk_signals as (
       select
         c.document_id,
         c.revision_id,
         c.chunk_index,
         ((coalesce(c.content_text, '') || E'\n' || coalesce(c.window_text, '')) ~ $3) as has_version_marker
       from knowledge_chunk c
       join candidate_documents candidate
         on candidate.document_id = c.document_id
        and candidate.workspace_id = c.workspace_id
        and candidate.library_id = c.library_id
        and candidate.revision_id = c.revision_id
       where c.library_id = $1
         and c.chunk_state = 'ready'
     ),
     scored as (
       select
         document_id,
         revision_id,
         count(*) filter (where has_version_marker)::bigint as version_row_count,
         max(chunk_index) filter (where has_version_marker) as max_version_chunk_index
       from chunk_signals
       group by document_id, revision_id
     )
     select document_id, revision_id, version_row_count, max_version_chunk_index
     from scored
     where version_row_count >= $4
     order by version_row_count desc, max_version_chunk_index desc nulls last, document_id asc
     limit $5";

pub(super) async fn latest_version_structural_density_candidate_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    excluded_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> anyhow::Result<Vec<LatestVersionSemanticDocument>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let Some(library_id) = document_index.values().next().map(|document| document.library_id)
    else {
        return Ok(Vec::new());
    };
    let mut candidate_revisions = document_index
        .values()
        .filter(|document| latest_version_semantic_document_is_content_candidate(document))
        .filter(|document| !excluded_document_ids.contains(&document.document_id))
        .filter(|document| {
            if scope_terms.is_empty() {
                return true;
            }
            let identity_text = latest_version_document_identity_text(document);
            scope_terms.iter().any(|term| identity_text.contains(term))
        })
        .filter_map(|document| {
            document.readable_revision_id.map(|revision_id| (document.document_id, revision_id))
        })
        .collect::<Vec<_>>();
    candidate_revisions.sort_unstable();
    if candidate_revisions.is_empty() {
        return Ok(Vec::new());
    }
    let (candidate_document_ids, candidate_revision_ids): (Vec<_>, Vec<_>) =
        candidate_revisions.into_iter().unzip();

    let rows = sqlx::query_as::<_, LatestVersionStructuralDensityCandidateRow>(
        LATEST_VERSION_STRUCTURAL_DENSITY_CANDIDATES_SQL,
    )
    .bind(library_id)
    .bind(&candidate_document_ids)
    .bind(LATEST_VERSION_STRUCTURAL_DENSITY_REGEX)
    .bind(LATEST_VERSION_STRUCTURAL_DENSITY_MIN_ROWS)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .bind(&candidate_revision_ids)
    .fetch_all(&state.persistence.postgres)
    .await
    .context("failed to load latest-version structural-density candidates")?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let document = document_index.get(&row.document_id)?;
            (document.readable_revision_id == Some(row.revision_id)).then_some(())?;
            let revision_id = row.revision_id;
            let identity_text = latest_version_document_identity_text(document);
            let version_row_score = usize::try_from(row.version_row_count).unwrap_or(usize::MAX);
            let max_index_score = row
                .max_version_chunk_index
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or_default();
            Some(LatestVersionSemanticDocument {
                document_id: document.document_id,
                revision_id,
                semantic_score: version_row_score
                    .saturating_mul(1024)
                    .saturating_add(max_index_score.min(1023)),
                identity_text,
                requires_structural_release_density: true,
            })
        })
        .collect())
}

pub(super) fn latest_version_context_versions(text: &str) -> Vec<Vec<u32>> {
    let mut versions = BTreeSet::<Vec<u32>>::new();
    for line in text.lines() {
        if let Some(version) = extract_release_context_version(line) {
            versions.insert(version);
        }
    }
    if versions.is_empty()
        && let Some(version) = extract_release_context_version(text)
    {
        versions.insert(version);
    }
    versions.into_iter().collect()
}

pub(super) fn latest_version_structural_inventory_text_score(text: &str) -> usize {
    let version_score = latest_version_context_versions(text).len().min(8).saturating_mul(4);
    let row_marker_score = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            strip_leading_procedure_order_marker(trimmed) != trimmed
                || trimmed.starts_with(['-', '*', '•'])
                || trimmed.matches('|').count() >= 2
                || trimmed.matches('\t').count() >= 2
        })
        .count()
        .min(8);
    let literal_score = extract_explicit_path_literals(text, 4)
        .len()
        .saturating_add(extract_parameter_literals(text, 4).len())
        .saturating_add(extract_config_assignment_literals(text, 4).len())
        .min(4);
    version_score.saturating_add(row_marker_score).saturating_add(literal_score)
}

pub(super) fn latest_version_structural_inventory_candidate_has_density(
    distinct_version_count: usize,
    version_row_count: usize,
    structural_score: usize,
) -> bool {
    distinct_version_count >= LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS
        && (version_row_count >= LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS
            || structural_score >= LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS * 4)
}

pub(super) fn latest_version_semantic_document_is_content_candidate(
    document: &KnowledgeDocumentRow,
) -> bool {
    if document.document_state != "active"
        || document.document_role != crate::domains::content::DOCUMENT_ROLE_PRIMARY
    {
        return false;
    }
    ![
        document.title.as_deref(),
        document.file_name.as_deref(),
        document.source_uri.as_deref(),
        Some(document.external_key.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(latest_version_identity_value_is_image_like)
}

pub(super) fn latest_version_identity_value_is_image_like(value: &str) -> bool {
    let path = value.split(['?', '#']).next().unwrap_or(value).trim();
    matches!(
        std::path::Path::new(path)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref()
            .unwrap_or_default(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg"
    )
}

pub(super) fn latest_version_document_identity_text(document: &KnowledgeDocumentRow) -> String {
    [
        document.title.as_deref(),
        document.file_name.as_deref(),
        document.source_uri.as_deref(),
        document.document_hint.as_deref(),
        Some(document.external_key.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LatestVersionDocument {
    pub(crate) document_id: Uuid,
    pub(crate) revision_id: Uuid,
    pub(crate) version: Vec<u32>,
    pub(crate) title: String,
}

pub(crate) fn latest_version_documents(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    count: usize,
    scope_terms: &[String],
) -> Vec<LatestVersionDocument> {
    let rows = document_index
        .values()
        .filter_map(|document| {
            let latest_document = latest_version_document_from_index_row(document)?;
            let identity_text = format!(
                "{} {}",
                latest_document.title.to_lowercase(),
                document.external_key.to_lowercase()
            );
            Some((latest_document, identity_text))
        })
        .collect::<Vec<_>>();
    let scoped_rows = if scope_terms.is_empty() {
        rows
    } else {
        let scoped = rows
            .iter()
            .filter(|(_, identity_text)| {
                scope_terms.iter().any(|term| identity_text.contains(term))
            })
            .cloned()
            .collect::<Vec<_>>();
        if scoped.is_empty() { rows } else { scoped }
    };
    let mut rows = scoped_rows.into_iter().map(|(document, _)| document).collect::<Vec<_>>();
    rows.sort_by(compare_latest_version_documents);
    rows = dedupe_latest_version_documents(rows);
    rows.sort_by(compare_latest_version_documents);
    rows.truncate(count);
    rows
}

pub(super) fn latest_version_document_from_index_row(
    document: &KnowledgeDocumentRow,
) -> Option<LatestVersionDocument> {
    if !latest_version_semantic_document_is_content_candidate(document) {
        return None;
    }
    let primary_title = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(document.file_name.as_deref())?;
    if !text_has_release_version_marker(primary_title) {
        return None;
    }
    // Titles can carry one release across multiple parallel version channels.
    // A generic semver scanner returns the first literal and can therefore rank
    // the newest release as ancient.
    // Reuse the release-context parser used for chunk bodies so the version
    // adjacent to the release marker, including a direct compound group, is
    // interpreted consistently on both retrieval paths.
    let version = extract_release_context_version(primary_title)?;
    let revision_id = document.readable_revision_id?;
    Some(LatestVersionDocument {
        document_id: document.document_id,
        revision_id,
        version,
        title: primary_title.to_string(),
    })
}

pub(super) fn compare_latest_version_documents(
    left: &LatestVersionDocument,
    right: &LatestVersionDocument,
) -> std::cmp::Ordering {
    compare_version_desc(&left.version, &right.version)
        .then_with(|| left.title.cmp(&right.title))
        .then_with(|| left.document_id.cmp(&right.document_id))
}

pub(super) fn dedupe_latest_version_documents(
    rows: Vec<LatestVersionDocument>,
) -> Vec<LatestVersionDocument> {
    let mut seen_sources = HashSet::<ReleaseSourceIdentity>::with_capacity(rows.len());
    rows.into_iter()
        .filter(|document| {
            seen_sources
                .insert(ReleaseSourceIdentity::new(document.document_id, document.revision_id))
        })
        .collect()
}
