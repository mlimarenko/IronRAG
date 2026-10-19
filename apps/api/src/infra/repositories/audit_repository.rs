use chrono::{DateTime, Utc};
use sqlx::FromRow;
use sqlx::PgPool;
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AuditEventRow {
    pub id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub result_kind: String,
    pub created_at: DateTime<Utc>,
    pub redacted_message: Option<String>,
    pub internal_message: Option<String>,
    /// The displayed actor label; `NULL` for a system event. Only the list
    /// query selects it, so the insert default keeps the row type shared.
    #[sqlx(default)]
    pub actor_label: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AuditEventSubjectRow {
    pub audit_event_id: Uuid,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewAuditEvent {
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub result_kind: String,
    pub redacted_message: Option<String>,
    pub internal_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewAuditEventSubject {
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct AuditEventSubjectFilter {
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

/// The displayed actor label, server-side: the principal's display label, else
/// the user's display name, else the login, else the principal id; `NULL` when
/// the event has no actor principal. One definition, shared by the sort key,
/// the keyset cursor and the actor facet, so the three cannot drift.
pub const AUDIT_ACTOR_LABEL_SQL: &str = "(select coalesce(
        nullif(trim(p.display_label), ''),
        nullif(trim(u.display_name), ''),
        nullif(trim(u.login), ''),
        ae.actor_principal_id::text
    )
    from iam_principal p
    left join iam_user u on u.principal_id = p.id
    where p.id = ae.actor_principal_id
    limit 1)";

/// The attribute the audit list is ordered by. Every key names a stable,
/// non-ambiguous expression and the exact column id the grid writes, so a
/// `sort` parameter maps to a server order with no lookup table that could
/// drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditSortKey {
    CreatedAt,
    Actor,
    Action,
    Surface,
    Result,
}

impl AuditSortKey {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "created_at" => Some(Self::CreatedAt),
            "actor" => Some(Self::Actor),
            "action" => Some(Self::Action),
            "surface" => Some(Self::Surface),
            "result" => Some(Self::Result),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::Actor => "actor",
            Self::Action => "action",
            Self::Surface => "surface",
            Self::Result => "result",
        }
    }

    /// The SQL expression this key orders and cursor-compares on. `Actor`
    /// orders on the displayed label, not the principal id: the id is not
    /// equivalent to what the reader sees, so ordering by it would contradict
    /// the rows. The label is nullable (a system event), hence the null-aware
    /// keyset predicate below.
    #[must_use]
    pub const fn expr(self) -> &'static str {
        match self {
            Self::CreatedAt => "ae.created_at",
            Self::Actor => AUDIT_ACTOR_LABEL_SQL,
            Self::Action => "ae.action_kind",
            Self::Surface => "ae.surface_kind::text",
            Self::Result => "ae.result_kind::text",
        }
    }

    /// The value a keyset cursor carries for this key is a timestamp for the
    /// time column and text for every other column. `Actor` may be a real
    /// null (a system event), which is why the keyset predicate is
    /// null-aware.
    #[must_use]
    pub const fn is_time(self) -> bool {
        matches!(self, Self::CreatedAt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditSortOrder {
    Asc,
    Desc,
}

impl AuditSortOrder {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "asc" => Some(Self::Asc),
            "desc" => Some(Self::Desc),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }

    /// The keyset comparison for this direction. The id tie-breaker uses the
    /// same comparison so `(value, id)` moves strictly forward.
    const fn cmp(self) -> &'static str {
        match self {
            Self::Asc => ">",
            Self::Desc => "<",
        }
    }

    const fn sql(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AuditSort {
    pub key: AuditSortKey,
    pub order: AuditSortOrder,
}

impl Default for AuditSort {
    fn default() -> Self {
        Self { key: AuditSortKey::CreatedAt, order: AuditSortOrder::Desc }
    }
}

/// The typed sort value a keyset cursor carries. A timestamp for the time
/// column, the raw column text for the rest; `None` on a cursor row means the
/// ordered column was SQL NULL.
#[derive(Debug, Clone)]
pub enum AuditCursorValue {
    Time(DateTime<Utc>),
    Text(String),
}

impl AuditCursorValue {
    #[must_use]
    pub fn as_text(&self) -> String {
        match self {
            Self::Time(value) => value.to_rfc3339(),
            Self::Text(value) => value.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuditListCursorValue {
    pub value: Option<AuditCursorValue>,
    pub id: Uuid,
}

/// The multi-value column filters of the audit grid. Each list is OR within
/// its column; the columns are ANDed with each other. An empty list
/// constrains nothing.
#[derive(Debug, Clone, Default)]
pub struct AuditEventMultiFilters {
    /// Actor principal ids as text; the null bucket is a separate flag because
    /// a list element can never equal SQL NULL.
    pub actor_principal_ids: Vec<String>,
    pub actor_include_null: bool,
    pub action_kinds: Vec<String>,
    pub surface_kinds: Vec<String>,
    pub result_kinds: Vec<String>,
    /// Exact `created_at` renderings, compared as text so no precision is lost
    /// on the round trip through the facet value.
    pub created_at_texts: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ListAuditEventsQuery {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub subject_filter: AuditEventSubjectFilter,
    pub filters: AuditEventMultiFilters,
    pub search: Option<String>,
    /// Which metadata the whole-table search may match for this caller.
    pub internal: bool,
    /// Derived enum/interval hints whose displayed spelling matched the term.
    pub search_hints: AuditSearchHints,
    pub sort: AuditSort,
    pub limit: i64,
    /// Keyset cursor of the last row on the previous page. `None` starts from
    /// the top of the requested ordering.
    pub cursor: Option<AuditListCursorValue>,
}

#[derive(Debug, Clone)]
pub struct AuditEventPage {
    pub items: Vec<AuditEventRow>,
    pub total: i64,
    /// The last returned row's sort value and id, present only when more rows
    /// exist beyond this page. Callers encode this into an opaque
    /// continuation token.
    pub next_cursor: Option<AuditListCursorValue>,
}

/// Which audit column a facet request aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditFacetColumn {
    Actor,
    Action,
    Surface,
    Result,
    CreatedAt,
}

impl AuditFacetColumn {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "actor" => Some(Self::Actor),
            "action" => Some(Self::Action),
            "surface" => Some(Self::Surface),
            "result" => Some(Self::Result),
            "created_at" => Some(Self::CreatedAt),
            _ => None,
        }
    }

    #[must_use]
    pub const fn value_expr(self) -> &'static str {
        match self {
            Self::Actor => "ae.actor_principal_id::text",
            Self::Action => "ae.action_kind",
            Self::Surface => "ae.surface_kind::text",
            Self::Result => "ae.result_kind::text",
            Self::CreatedAt => "ae.created_at::text",
        }
    }

    /// A human label is only meaningful for the actor column; the enum and
    /// time columns label themselves.
    #[must_use]
    pub const fn is_actor(self) -> bool {
        matches!(self, Self::Actor)
    }
}

#[derive(Debug, Clone)]
pub struct AuditFacetQuery {
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub subject_filter: AuditEventSubjectFilter,
    /// Every filter except the facet column's own selection: a column's menu
    /// lists the values the *other* filters leave.
    pub filters: AuditEventMultiFilters,
    pub search: Option<String>,
    /// Which metadata the value scope's search may match; kept identical to the
    /// list's scope so a facet never counts a row the list would hide.
    pub internal: bool,
    /// Derived enum/interval hints, kept identical to the list's.
    pub search_hints: AuditSearchHints,
    pub facet_column: AuditFacetColumn,
    pub facet_search: Option<String>,
    pub facet_search_hints: AuditSearchHints,
    /// The exact selected `(column, value)` pairs to resolve labels for. When
    /// present the query returns those values' authoritative labels, without
    /// counting, so a chip outside the first page still names its actor.
    pub selected_values: Vec<(AuditFacetColumn, String)>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct AuditFacetRow {
    pub value: Option<String>,
    pub label: String,
    pub count: i64,
}

pub async fn append_audit_event(
    postgres: &PgPool,
    event: NewAuditEvent,
    subjects: &[NewAuditEventSubject],
) -> Result<AuditEventRow, sqlx::Error> {
    let audit_event = sqlx::query_as::<_, AuditEventRow>(
        "insert into audit_event (
            id,
            actor_principal_id,
            surface_kind,
            action_kind,
            request_id,
            trace_id,
            result_kind,
            created_at,
            redacted_message,
            internal_message
        )
        values ($1, $2, $3::surface_kind, $4, $5, $6, $7::audit_result_kind, now(), $8, $9)
        returning
            id,
            actor_principal_id,
            surface_kind::text as surface_kind,
            action_kind,
            request_id,
            trace_id,
            result_kind::text as result_kind,
            created_at,
            redacted_message,
            internal_message",
    )
    .bind(Uuid::now_v7())
    .bind(event.actor_principal_id)
    .bind(event.surface_kind)
    .bind(event.action_kind)
    .bind(event.request_id)
    .bind(event.trace_id)
    .bind(event.result_kind)
    .bind(event.redacted_message)
    .bind(event.internal_message)
    .fetch_one(postgres)
    .await?;

    if !subjects.is_empty() {
        let mut builder = QueryBuilder::<Postgres>::new(
            "insert into audit_event_subject (
                audit_event_id,
                subject_kind,
                subject_id,
                workspace_id,
                library_id,
                document_id
            ) ",
        );
        builder.push_values(subjects.iter(), |mut row, subject| {
            row.push_bind(audit_event.id)
                .push_bind(&subject.subject_kind)
                .push_bind(subject.subject_id)
                .push_bind(subject.workspace_id)
                .push_bind(subject.library_id)
                .push_bind(subject.document_id);
        });
        builder.build().execute(postgres).await?;
    }

    Ok(audit_event)
}

pub async fn list_audit_events(
    postgres: &PgPool,
    query: &ListAuditEventsQuery,
) -> Result<AuditEventPage, sqlx::Error> {
    let search_scope = AuditSearchScope::new(query.workspace_id, query.library_id, query.internal);
    let mut count_builder = QueryBuilder::<Postgres>::new(
        "select count(*)::bigint
         from audit_event ae
         where 1 = 1",
    );
    push_list_audit_event_filters(
        &mut count_builder,
        query.filters.clone(),
        query.search.clone(),
        query.search_hints.clone(),
        search_scope,
    );
    push_audit_scope_filters(&mut count_builder, query.workspace_id, query.library_id);
    push_audit_subject_filters(&mut count_builder, &query.subject_filter, search_scope);
    let total = count_builder.build_query_scalar::<i64>().fetch_one(postgres).await?;

    // Fetch `limit + 1` rows so a next-page cursor can be reported without a
    // second round-trip; the extra row is trimmed below.
    let fetch_limit = query.limit + 1;
    let mut builder = QueryBuilder::<Postgres>::new(
        "select
            ae.id,
            ae.actor_principal_id,
            ae.surface_kind::text as surface_kind,
            ae.action_kind,
            ae.request_id,
            ae.trace_id,
            ae.result_kind::text as result_kind,
            ae.created_at,
            ae.redacted_message,
            ae.internal_message, ",
    );
    builder.push(AUDIT_ACTOR_LABEL_SQL);
    builder.push(" as actor_label from audit_event ae where 1 = 1");
    push_list_audit_event_filters(
        &mut builder,
        query.filters.clone(),
        query.search.clone(),
        query.search_hints.clone(),
        search_scope,
    );
    push_audit_scope_filters(&mut builder, query.workspace_id, query.library_id);
    push_audit_subject_filters(&mut builder, &query.subject_filter, search_scope);
    if let Some(cursor) = query.cursor.as_ref() {
        push_audit_cursor(&mut builder, query.sort, cursor);
    }
    builder
        .push(" order by ")
        .push(query.sort.key.expr())
        .push(" ")
        .push(query.sort.order.sql())
        .push(" nulls last, ae.id ")
        .push(query.sort.order.sql())
        .push(" limit ")
        .push_bind(fetch_limit);

    let mut items = builder.build_query_as::<AuditEventRow>().fetch_all(postgres).await?;
    let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(|row| AuditListCursorValue {
            value: audit_cursor_value(row, query.sort.key),
            id: row.id,
        })
    } else {
        None
    };
    Ok(AuditEventPage { items, total, next_cursor })
}

/// The sort value a row carries for the requested key. `Actor` is a real null
/// for a system event, so the cursor records the null too.
fn audit_cursor_value(row: &AuditEventRow, key: AuditSortKey) -> Option<AuditCursorValue> {
    match key {
        AuditSortKey::CreatedAt => Some(AuditCursorValue::Time(row.created_at)),
        AuditSortKey::Actor => row.actor_label.clone().map(AuditCursorValue::Text),
        AuditSortKey::Action => Some(AuditCursorValue::Text(row.action_kind.clone())),
        AuditSortKey::Surface => Some(AuditCursorValue::Text(row.surface_kind.clone())),
        AuditSortKey::Result => Some(AuditCursorValue::Text(row.result_kind.clone())),
    }
}

/// The null-aware keyset predicate that continues the requested ordering. A
/// non-null cursor value leaves three groups: values strictly past the cursor,
/// ties with a larger/lower id, and the SQL NULLs that `nulls last` puts at the
/// end. A null cursor value leaves only later nulls; no non-null row follows a
/// trailing null.
fn push_audit_cursor(
    builder: &mut QueryBuilder<Postgres>,
    sort: AuditSort,
    cursor: &AuditListCursorValue,
) {
    let expr = sort.key.expr();
    let cmp = sort.order.cmp();
    match cursor.value.as_ref() {
        Some(value) => {
            builder.push(" and (").push(expr).push(" ").push(cmp).push(" ");
            push_audit_cursor_value(builder, value);
            builder.push(" or (").push(expr).push(" = ");
            push_audit_cursor_value(builder, value);
            builder
                .push(" and ae.id ")
                .push(cmp)
                .push(" ")
                .push_bind(cursor.id)
                .push(") or ")
                .push(expr)
                .push(" is null)");
        }
        None => {
            builder
                .push(" and (")
                .push(expr)
                .push(" is null and ae.id ")
                .push(cmp)
                .push(" ")
                .push_bind(cursor.id)
                .push(")");
        }
    }
}

fn push_audit_cursor_value(builder: &mut QueryBuilder<Postgres>, value: &AuditCursorValue) {
    match value {
        AuditCursorValue::Time(timestamp) => {
            builder.push_bind(*timestamp);
        }
        AuditCursorValue::Text(text) => {
            builder.push_bind(text.clone());
        }
    }
}

/// The workspace/library scope of the caller, applied identically to the count,
/// list and facet queries so the numbers belong to the same set as the rows.
fn push_audit_scope_filters(
    builder: &mut QueryBuilder<Postgres>,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) {
    if let Some(workspace_id) = workspace_id {
        builder.push(
            " and exists (
                select 1
                from audit_event_subject aes
                where aes.audit_event_id = ae.id
                  and aes.workspace_id = ",
        );
        builder.push_bind(workspace_id);
        builder.push(")");
    }

    if let Some(library_id) = library_id {
        builder.push(
            " and exists (
                select 1
                from audit_event_subject aes
                where aes.audit_event_id = ae.id
                  and aes.library_id = ",
        );
        builder.push_bind(library_id);
        builder.push(")");
    }
}

/// The exact-subject filters (a document/revision/execution id) shared by the
/// list and facet queries.
fn push_audit_subject_filters(
    builder: &mut QueryBuilder<Postgres>,
    filter: &AuditEventSubjectFilter,
    scope: AuditSearchScope,
) {
    if let Some(knowledge_document_id) = filter.knowledge_document_id {
        builder.push(
            " and exists (
                select 1
                from audit_event_subject aes
                where aes.audit_event_id = ae.id
                  and (
                        aes.document_id = ",
        );
        builder.push_bind(knowledge_document_id);
        builder.push(
            "        or (
                            aes.subject_kind = 'knowledge_document'
                        and aes.subject_id = ",
        );
        builder.push_bind(knowledge_document_id);
        builder.push(
            "           )
                  )
             ",
        );
        push_subject_visibility(builder, scope.workspace_id, scope.library_id);
        builder.push(")");
    }

    if let Some(knowledge_revision_id) = filter.knowledge_revision_id {
        push_exact_subject_filter(builder, "knowledge_revision", knowledge_revision_id, scope);
    }

    if let Some(context_bundle_id) = filter.context_bundle_id {
        push_exact_subject_filter(builder, "knowledge_bundle", context_bundle_id, scope);
    }

    if let Some(query_session_id) = filter.query_session_id {
        push_exact_subject_filter(builder, "query_session", query_session_id, scope);
    }

    if let Some(query_execution_id) = filter.query_execution_id {
        push_exact_subject_filter(builder, "query_execution", query_execution_id, scope);
    }

    if let Some(runtime_execution_id) = filter.runtime_execution_id {
        push_exact_subject_filter(builder, "runtime_execution", runtime_execution_id, scope);
    }

    if let Some(async_operation_id) = filter.async_operation_id {
        push_exact_subject_filter(builder, "async_operation", async_operation_id, scope);
    }
}

/// The derived search hints: closed-enum values and a UTC interval whose
/// *displayed* spelling matched the typed term. Each list is OR'd over the same
/// column the grid renders, and the interval compares the timestamp — so a
/// reader who searches what they see gets the rows the label belongs to without
/// the client ever sending a rendered string as text.
#[derive(Debug, Clone, Default)]
pub struct AuditSearchHints {
    pub action_kinds: Vec<String>,
    pub surface_kinds: Vec<String>,
    pub result_kinds: Vec<String>,
    /// The shared null token matched the term: constrain to the no-actor rows.
    pub actor_null: bool,
    pub created_at_from: Option<DateTime<Utc>>,
    pub created_at_to: Option<DateTime<Utc>>,
}

impl AuditSearchHints {
    fn is_empty(&self) -> bool {
        self.action_kinds.is_empty()
            && self.surface_kinds.is_empty()
            && self.result_kinds.is_empty()
            && !self.actor_null
            && self.created_at_from.is_none()
            && self.created_at_to.is_none()
    }
}

/// Adds OR alternatives using the same event projections in global and local
/// facet searches. The caller owns the surrounding AND group and column scope.
fn push_audit_hint_matches(
    builder: &mut QueryBuilder<Postgres>,
    hints: &AuditSearchHints,
    column: Option<AuditFacetColumn>,
) {
    for (hint_column, expression, values) in [
        (AuditFacetColumn::Action, "ae.action_kind", &hints.action_kinds),
        (AuditFacetColumn::Surface, "ae.surface_kind::text", &hints.surface_kinds),
        (AuditFacetColumn::Result, "ae.result_kind::text", &hints.result_kinds),
    ] {
        if !values.is_empty() && column.is_none_or(|column| column == hint_column) {
            builder.push(" or ").push(expression).push(" = any(");
            builder.push_bind(values.clone()).push("::text[])");
        }
    }
    if hints.actor_null && column.is_none_or(|column| column == AuditFacetColumn::Actor) {
        builder.push(" or ae.actor_principal_id is null");
    }
    if (hints.created_at_from.is_some() || hints.created_at_to.is_some())
        && column.is_none_or(|column| column == AuditFacetColumn::CreatedAt)
    {
        builder.push(" or (true");
        if let Some(from) = hints.created_at_from {
            builder.push(" and ae.created_at >= ").push_bind(from);
        }
        if let Some(to) = hints.created_at_to {
            builder.push(" and ae.created_at < ").push_bind(to);
        }
        builder.push(")");
    }
}

/// How much of an event's metadata the caller's scope lets the search match.
///
/// The whole-table search must never turn a hidden field into an oracle: the
/// non-internal view never matches `internal_message`, and no view matches a
/// subject row the caller's own workspace/library scope would hide. The
/// authorized redacted message, the scoped subject ids and the displayed
/// vocabulary stay searchable in every view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuditSearchScope {
    /// Match `internal_message` (and any other internal-only field). Only the
    /// authorized system-admin internal view sets this.
    internal: bool,
    /// The row-visibility scope applied to the event itself. A subject row is
    /// searchable only when it is inside this scope, so an out-of-scope subject
    /// id on a visible event cannot be selected by a hidden needle.
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
}

impl AuditSearchScope {
    const fn new(workspace_id: Option<Uuid>, library_id: Option<Uuid>, internal: bool) -> Self {
        Self { internal, workspace_id, library_id }
    }
}

/// The `audit_event_subject` visibility predicate shared by the row scope and
/// the search: a subject is in scope when it carries the caller's library, or
/// the caller's workspace, or nothing at all to narrow by.
fn push_subject_visibility(
    builder: &mut QueryBuilder<Postgres>,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) {
    builder.push(" and (");
    match (library_id, workspace_id) {
        (Some(_), _) => {
            builder.push("aes.library_id = ");
            builder.push_bind(library_id);
        }
        (None, Some(_)) => {
            builder.push("aes.workspace_id = ");
            builder.push_bind(workspace_id);
        }
        // A system admin with no narrowing scope sees every subject row.
        (None, None) => {
            builder.push("true");
        }
    }
    builder.push(")");
}

/// The column filters and the whole-table search, shared by the count, list and
/// facet queries. The multi-value lists are OR within a column and AND across
/// columns; the search matches every visible metadata field, including the
/// surface and result vocabularies the grid displays, the authorized redacted
/// message and the scoped subject ids — but never an internal-only field on a
/// non-internal view.
fn push_list_audit_event_filters(
    builder: &mut QueryBuilder<Postgres>,
    filters: AuditEventMultiFilters,
    search: Option<String>,
    search_hints: AuditSearchHints,
    scope: AuditSearchScope,
) {
    if !filters.actor_principal_ids.is_empty() || filters.actor_include_null {
        builder.push(" and (ae.actor_principal_id::text = any(");
        builder.push_bind(filters.actor_principal_ids).push("::text[])");
        if filters.actor_include_null {
            builder.push(" or ae.actor_principal_id is null");
        }
        builder.push(")");
    }
    push_text_any(builder, "ae.action_kind", &filters.action_kinds);
    push_text_any(builder, "ae.surface_kind::text", &filters.surface_kinds);
    push_text_any(builder, "ae.result_kind::text", &filters.result_kinds);
    push_text_any(builder, "ae.created_at::text", &filters.created_at_texts);
    if search.is_none() && search_hints.is_empty() {
        return;
    }
    builder.push(" and (false");
    push_audit_hint_matches(builder, &search_hints, None);
    if let Some(search) = search.as_ref() {
        let pattern = format!("%{search}%");
        builder.push(" or ");
        push_audit_action_search(builder, &pattern);
        builder.push(" or coalesce(ae.redacted_message, '') ilike ");
        builder.push_bind(pattern.clone());
        if scope.internal {
            builder.push(" or coalesce(ae.internal_message, '') ilike ");
            builder.push_bind(pattern.clone());
            builder.push(" or coalesce(ae.request_id, '') ilike ");
            builder.push_bind(pattern.clone());
            builder.push(" or coalesce(ae.trace_id, '') ilike ");
            builder.push_bind(pattern.clone());
        }
        builder.push(" or ae.surface_kind::text ilike ");
        builder.push_bind(pattern.clone());
        builder.push(" or ae.result_kind::text ilike ");
        builder.push_bind(pattern.clone());
        builder.push(" or coalesce(ae.actor_principal_id::text, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push(" or ").push(AUDIT_ACTOR_LABEL_SQL).push(" ilike ");
        builder.push_bind(pattern.clone());
        // A subject row is searchable only inside the caller's scope, so the
        // search cannot select a visible event through a hidden subject.
        builder.push(
            " or exists (
                    select 1
                    from audit_event_subject aes
                    where aes.audit_event_id = ae.id",
        );
        push_subject_visibility(builder, scope.workspace_id, scope.library_id);
        builder.push(
            " and (
                            aes.subject_kind ilike ",
        );
        builder.push_bind(pattern.clone());
        builder.push("       or aes.subject_id::text ilike ");
        builder.push_bind(pattern.clone());
        builder.push("       or coalesce(aes.workspace_id::text, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push("       or coalesce(aes.library_id::text, '') ilike ");
        builder.push_bind(pattern.clone());
        builder.push("       or coalesce(aes.document_id::text, '') ilike ");
        builder.push_bind(pattern);
        builder.push(
            "      )
                )",
        );
    }
    builder.push(")");
}

/// Dynamic action labels collapse dot/underscore/hyphen separators in the grid.
/// Match that displayed spelling over the same authorized set as the raw key.
fn push_audit_action_search(builder: &mut QueryBuilder<Postgres>, pattern: &str) {
    builder.push("(ae.action_kind ilike ");
    builder.push_bind(pattern.to_owned());
    builder.push(" or btrim(regexp_replace(ae.action_kind, '[._-]+', ' ', 'g')) ilike ");
    builder.push_bind(pattern.to_owned());
    builder.push(")");
}

fn push_text_any(builder: &mut QueryBuilder<Postgres>, expr: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    builder.push(" and ").push(expr).push(" = any(");
    builder.push_bind(values.to_vec());
    builder.push("::text[])");
}

/// The facet values of one audit column over the whole filtered set, with the
/// column's own selection excluded by the caller. Bounded by `limit` and paged
/// by `offset`; `facet_search` narrows the value list itself.
pub async fn list_audit_event_facets(
    postgres: &PgPool,
    query: &AuditFacetQuery,
) -> Result<Vec<AuditFacetRow>, sqlx::Error> {
    let search_scope = AuditSearchScope::new(query.workspace_id, query.library_id, query.internal);
    let value_expr = query.facet_column.value_expr();
    // One label definition, shared with the sort key. Deliberately no join in
    // the FROM: a join on iam_user could multiply one principal's rows and
    // inflate the count, and the correlated subquery already folds the user
    // columns in.
    let label_expr = if query.facet_column.is_actor() {
        format!("coalesce({AUDIT_ACTOR_LABEL_SQL}, 'system')")
    } else {
        value_expr.to_string()
    };

    let mut builder = QueryBuilder::<Postgres>::new("select ");
    builder.push(value_expr);
    builder.push(" as value, ");
    builder.push(label_expr.clone());
    builder.push(
        " as label, count(*)::bigint as count
         from audit_event ae
         where 1 = 1",
    );
    push_list_audit_event_filters(
        &mut builder,
        query.filters.clone(),
        query.search.clone(),
        query.search_hints.clone(),
        search_scope,
    );
    push_audit_scope_filters(&mut builder, query.workspace_id, query.library_id);
    push_audit_subject_filters(&mut builder, &query.subject_filter, search_scope);
    push_audit_facet_search(&mut builder, query, &label_expr);
    // Resolving the selected values' labels is a bounded lookup by exact value,
    // never a full or page-limited aggregation: the request names the columns
    // and values, so a selection made on another page still names its actor.
    if !query.selected_values.is_empty() {
        builder.push(" and (");
        for (index, (column, value)) in query.selected_values.iter().enumerate() {
            if index > 0 {
                builder.push(" or ");
            }
            builder.push("(");
            builder.push(column.value_expr());
            builder.push(" = ");
            builder.push_bind(value.clone());
            builder.push(")");
        }
        builder.push(")");
    }
    builder
        .push(" group by 1, 2 order by 3 desc, value asc nulls last limit ")
        .push_bind(query.limit + 1)
        .push(" offset ")
        .push_bind(query.offset);

    builder.build_query_as::<AuditFacetRow>().fetch_all(postgres).await
}

/// Narrows the facet value list, independently of the global event search.
fn push_audit_facet_search(
    builder: &mut QueryBuilder<Postgres>,
    query: &AuditFacetQuery,
    label_expr: &str,
) {
    if query.facet_search.is_none() && query.facet_search_hints.is_empty() {
        return;
    }
    builder.push(" and (false");
    push_audit_hint_matches(builder, &query.facet_search_hints, Some(query.facet_column));
    if let Some(facet_search) = query.facet_search.as_ref() {
        let pattern = format!("%{facet_search}%");
        builder.push(" or ");
        if query.facet_column == AuditFacetColumn::Action {
            push_audit_action_search(builder, &pattern);
        } else {
            builder.push(query.facet_column.value_expr());
            builder.push(" ilike ");
            builder.push_bind(pattern.clone());
        }
        if query.facet_column.is_actor() {
            builder.push(" or ");
            builder.push(label_expr);
            builder.push(" ilike ");
            builder.push_bind(pattern);
        }
    }
    builder.push(")");
}

fn push_exact_subject_filter(
    builder: &mut QueryBuilder<Postgres>,
    subject_kind: &'static str,
    subject_id: Uuid,
    scope: AuditSearchScope,
) {
    builder.push(
        " and exists (
            select 1
            from audit_event_subject aes
            where aes.audit_event_id = ae.id
              and aes.subject_kind = ",
    );
    builder.push_bind(subject_kind);
    builder.push(" and aes.subject_id = ");
    builder.push_bind(subject_id);
    push_subject_visibility(builder, scope.workspace_id, scope.library_id);
    builder.push(")");
}

pub async fn list_audit_event_subjects(
    postgres: &PgPool,
    audit_event_id: Uuid,
) -> Result<Vec<AuditEventSubjectRow>, sqlx::Error> {
    sqlx::query_as::<_, AuditEventSubjectRow>(
        "select
            audit_event_id,
            subject_kind,
            subject_id,
            workspace_id,
            library_id,
            document_id
         from audit_event_subject
         where audit_event_id = $1
         order by subject_kind asc, subject_id asc",
    )
    .bind(audit_event_id)
    .fetch_all(postgres)
    .await
}

pub async fn append_bootstrap_claim_event(
    postgres: &PgPool,
    actor_principal_id: Uuid,
    request_id: &str,
    redacted_message: &str,
    internal_message: &str,
) -> Result<(), sqlx::Error> {
    append_audit_event(
        postgres,
        NewAuditEvent {
            actor_principal_id: Some(actor_principal_id),
            surface_kind: "bootstrap".to_string(),
            action_kind: "iam.bootstrap.claim".to_string(),
            request_id: Some(request_id.to_string()),
            trace_id: None,
            result_kind: "succeeded".to_string(),
            redacted_message: Some(redacted_message.to_string()),
            internal_message: Some(internal_message.to_string()),
        },
        &[NewAuditEventSubject {
            subject_kind: "principal".to_string(),
            subject_id: actor_principal_id,
            workspace_id: None,
            library_id: None,
            document_id: None,
        }],
    )
    .await?;
    Ok(())
}
