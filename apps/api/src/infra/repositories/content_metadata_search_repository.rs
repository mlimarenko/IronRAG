use std::collections::BTreeSet;

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::shared::versioning::dotted_version_terms;

#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentMetadataSearchRow {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub readable_revision_id: Option<Uuid>,
    pub revision_title: Option<String>,
    pub metadata_score: f64,
    pub matched_text: String,
}
pub async fn search_document_metadata_rows(
    postgres: &PgPool,
    library_id: Uuid,
    query: &str,
    limit: u32,
) -> Result<Vec<ContentDocumentMetadataSearchRow>, sqlx::Error> {
    let search_terms = metadata_search_terms(query);
    if search_terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, ContentDocumentMetadataSearchRow>(concat!(
        r"with candidate as (
         select
            d.id as document_id,
            d.workspace_id,
            d.library_id,
            d.external_key,
             current_revision.revision_id as readable_revision_id,
            r.title as revision_title,
            case
                when lower(coalesce(r.title, '')) = any($2) then 1400::double precision
                when lower(coalesce(current_revision.source_file_name, '')) = any($2) then 1390::double precision
                when lower(d.external_key) = any($2) then 1380::double precision
                when cardinality($4::text[]) > 0
                    and exists (
                        select 1 from unnest($4::text[]) as term(value)
                        where strpos(lower(coalesce(r.title, '')), term.value) > 0
                    ) then 1320::double precision
                when cardinality($4::text[]) > 0
                    and exists (
                        select 1 from unnest($4::text[]) as term(value)
                        where strpos(lower(coalesce(current_revision.source_file_name, '')), term.value) > 0
                    ) then 1300::double precision
                when cardinality($4::text[]) > 0
                    and exists (
                        select 1 from unnest($4::text[]) as term(value)
                        where strpos(lower(d.external_key), term.value) > 0
                    ) then 1280::double precision
                -- Partial metadata matches preserve a stable ordering inside
                -- the metadata lane. Cross-lane fusion uses dense document
                -- ranks, so these values are deliberately comparable only to
                -- other metadata candidates.
                when exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(coalesce(r.title, '')), term.value) > 0
                ) then
                    70::double precision + 80::double precision * least(
                        4,
                        (
                            select count(*)::integer
                            from unnest($3::text[]) as term(value)
                            where strpos(lower(coalesce(r.title, '')), term.value) > 0
                        )
                    )
                when exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(coalesce(current_revision.source_file_name, '')), term.value) > 0
                ) then
                    68::double precision + 78::double precision * least(
                        4,
                        (
                            select count(*)::integer
                            from unnest($3::text[]) as term(value)
                            where strpos(lower(coalesce(current_revision.source_file_name, '')), term.value) > 0
                        )
                    )
                when exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(d.external_key), term.value) > 0
                ) then
                    65::double precision + 75::double precision * least(
                        4,
                        (
                            select count(*)::integer
                            from unnest($3::text[]) as term(value)
                            where strpos(lower(d.external_key), term.value) > 0
                        )
                    )
                else 70::double precision
            end as metadata_score,
            case
                when lower(coalesce(r.title, '')) = any($2) then coalesce(r.title, d.external_key)
                when lower(coalesce(current_revision.source_file_name, '')) = any($2) then current_revision.source_file_name
                when lower(d.external_key) = any($2) then d.external_key
                when cardinality($4::text[]) > 0
                    and exists (
                        select 1 from unnest($4::text[]) as term(value)
                        where strpos(lower(coalesce(r.title, '')), term.value) > 0
                    ) then coalesce(r.title, d.external_key)
                when cardinality($4::text[]) > 0
                    and exists (
                        select 1 from unnest($4::text[]) as term(value)
                        where strpos(lower(coalesce(current_revision.source_file_name, '')), term.value) > 0
                    ) then current_revision.source_file_name
                when cardinality($4::text[]) > 0
                    and exists (
                        select 1 from unnest($4::text[]) as term(value)
                        where strpos(lower(d.external_key), term.value) > 0
                    ) then d.external_key
                when exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(coalesce(r.title, '')), term.value) > 0
                ) then coalesce(r.title, d.external_key)
                when exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(coalesce(current_revision.source_file_name, '')), term.value) > 0
                ) then current_revision.source_file_name
                when exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(d.external_key), term.value) > 0
                ) then d.external_key
                else coalesce(r.title, d.external_key)
            end as matched_text
          from content_document d
          left join content_document_head h on h.document_id = d.id
          left join content_current_revision current_revision
            on current_revision.document_id = d.id
           and current_revision.workspace_id = d.workspace_id
           and current_revision.library_id = d.library_id
          left join content_revision r on r.id = current_revision.revision_id
           and r.document_id = d.id
           and r.workspace_id = d.workspace_id
           and r.library_id = d.library_id
         -- Readability and readiness are derived from the knowledge-plane row,
         -- so a candidate admitted on the content-plane state alone can carry no
         -- readiness at all. Every other retrieval lane already filters on the
         -- knowledge plane; this one is joined to it for the same reason.
         join knowledge_document k
           on k.document_id = d.id
          and k.workspace_id = d.workspace_id
          and k.library_id = d.library_id
          and k.document_state = 'active'
          and k.deleted_at is null
         where d.library_id = $1
           and d.document_state = 'active'
            and d.deleted_at is null
            and not (",
        super::content_document_head_repository::unresolved_head_suppression!(),
        r")
           and (current_revision.revision_id is null or r.id is not null)
           and (
                lower(d.external_key) = any($2)
                or lower(coalesce(r.title, '')) = any($2)
                or lower(coalesce(current_revision.source_file_name, '')) = any($2)
                or exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(d.external_key), term.value) > 0
                )
                or exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(coalesce(r.title, '')), term.value) > 0
                )
                or exists (
                    select 1 from unnest($3::text[]) as term(value)
                    where strpos(lower(coalesce(current_revision.source_file_name, '')), term.value) > 0
                )
                or (
                    cardinality($4::text[]) > 0
                    and (
                        exists (
                            select 1 from unnest($4::text[]) as term(value)
                            where strpos(lower(d.external_key), term.value) > 0
                        )
                        or exists (
                            select 1 from unnest($4::text[]) as term(value)
                            where strpos(lower(coalesce(r.title, '')), term.value) > 0
                        )
                        or exists (
                            select 1 from unnest($4::text[]) as term(value)
                            where strpos(lower(coalesce(current_revision.source_file_name, '')), term.value) > 0
                        )
                    )
                )
           )
        ), ranked_candidate as (
         select candidate.*,
            regexp_match(
                candidate.matched_text,
                '([0-9]+)\.([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?'
            ) as version_parts
         from candidate
        )
         select
            document_id,
            workspace_id,
            library_id,
            external_key,
            readable_revision_id,
            revision_title,
            metadata_score,
            matched_text
         from ranked_candidate
         order by
            metadata_score desc,
            coalesce((version_parts[1])::integer, -1) desc,
            coalesce((version_parts[2])::integer, -1) desc,
            coalesce((version_parts[3])::integer, -1) desc,
            coalesce((version_parts[4])::integer, -1) desc,
            document_id desc
         limit $5",
    ))
    .bind(library_id)
    .bind(&search_terms.generic)
    .bind(&search_terms.generic)
    .bind(&search_terms.version)
    .bind(i64::from(limit))
    .fetch_all(postgres)
    .await
}

#[derive(Debug, Default, PartialEq, Eq)]
struct MetadataSearchTerms {
    generic: Vec<String>,
    version: Vec<String>,
}

impl MetadataSearchTerms {
    const fn is_empty(&self) -> bool {
        self.generic.is_empty() && self.version.is_empty()
    }
}

fn metadata_search_terms(query: &str) -> MetadataSearchTerms {
    let normalized_query = query.trim().to_lowercase();
    if normalized_query.is_empty() {
        return MetadataSearchTerms::default();
    }

    let mut seen = BTreeSet::new();
    let mut generic = Vec::new();
    if seen.insert(normalized_query.clone()) {
        generic.push(normalized_query.clone());
    }
    for token in normalized_query.split_whitespace() {
        let normalized_token = token
            .trim_matches(|character: char| {
                !character.is_alphanumeric() && !matches!(character, '.' | '_' | '-' | '/' | '\\')
            })
            .trim();
        if normalized_token.chars().count() >= 2 {
            push_metadata_search_term(&mut generic, &mut seen, normalized_token.to_string());
        }
        if generic.len() >= 8 {
            break;
        }
    }

    MetadataSearchTerms { generic, version: metadata_version_terms(&normalized_query) }
}

fn push_metadata_search_term(terms: &mut Vec<String>, seen: &mut BTreeSet<String>, term: String) {
    if seen.insert(term.clone()) {
        terms.push(term);
    }
}

fn metadata_version_terms(normalized_query: &str) -> Vec<String> {
    let has_word_context = normalized_query
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-' && character != '/'
        })
        .any(|token| token.chars().count() >= 2 && token.chars().any(char::is_alphabetic));

    dotted_version_terms(normalized_query)
        .into_iter()
        .filter(|term| term.matches('.').count() >= 2 || has_word_context)
        .collect()
}

#[cfg(test)]
#[path = "content_metadata_search_repository_tests.rs"]
mod tests;
