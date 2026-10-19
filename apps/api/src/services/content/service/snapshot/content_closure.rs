use super::*;
use serde_json::Value;

/// Bounded archive-local proof, not another visibility resolver. Original
/// identities are retained verbatim; scope remapping never rewrites their digest.
#[derive(Default)]
pub(super) struct SnapshotContentClosure {
    tables: BTreeMap<String, BTreeMap<Uuid, Value>>,
    bytes: usize,
    item_counts: BTreeMap<Uuid, usize>,
}

impl SnapshotContentClosure {
    pub(super) fn observe(&mut self, table: &str, row: &Value) -> anyhow::Result<()> {
        let key = match table {
            "content_document"
            | "content_revision"
            | "content_mutation_reference"
            | "content_mutation_predecessor_audit"
            | "content_mutation"
            | "content_mutation_item" => "id",
            "content_document_head" => "document_id",
            "content_visibility_intent" => "mutation_item_id",
            "knowledge_revision" => "revision_id",
            _ => return Ok(()),
        };
        // This proof retains only lineage-bearing rows, never blob bytes. Refuse
        // excessive metadata rather than let an untrusted archive exhaust memory.
        let id = required_uuid_field(table, row, key)?;
        if table == "content_mutation_item" {
            *self
                .item_counts
                .entry(required_uuid_field(table, row, "mutation_id")?)
                .or_default() += 1;
        }
        let retained = if table == "knowledge_revision" {
            [
                "revision_id",
                "document_id",
                "workspace_id",
                "library_id",
                "text_state",
                "text_readable_at",
                "vector_state",
                "vector_ready_at",
                "graph_state",
            ]
            .into_iter()
            .filter_map(|field| row.get(field).map(|value| (field.to_owned(), value.clone())))
            .collect::<serde_json::Map<_, _>>()
            .into()
        } else {
            row.clone()
        };
        self.bytes = self
            .bytes
            .checked_add(serde_json::to_vec(&retained)?.len())
            .ok_or_else(|| anyhow!("snapshot content closure size overflow"))?;
        anyhow::ensure!(
            self.bytes <= 256 * 1024 * 1024,
            "snapshot content closure exceeds metadata limit"
        );
        anyhow::ensure!(
            self.tables.entry(table.to_owned()).or_default().insert(id, retained).is_none(),
            "snapshot content closure contains duplicate identity in {table}"
        );
        Ok(())
    }

    fn rows(&self, table: &str) -> impl Iterator<Item = (&Uuid, &Value)> {
        self.tables.get(table).into_iter().flat_map(|rows| rows.iter())
    }

    fn row(&self, table: &str, id: Uuid) -> anyhow::Result<&Value> {
        self.tables
            .get(table)
            .and_then(|rows| rows.get(&id))
            .ok_or_else(|| anyhow!("snapshot content closure missing reference in {table}"))
    }

    pub(super) fn validate(&self, manifest: &SnapshotManifest) -> anyhow::Result<()> {
        if manifest.include_kinds.contains(&IncludeKind::LibraryData) {
            for table in POSTGRES_CONTENT_TABLES {
                anyhow::ensure!(
                    manifest.postgres_tables.iter().any(|name| name == table),
                    "snapshot schema 4 requires complete content table inventory"
                );
            }
        }
        self.validate_documents(manifest)?;
        self.validate_revisions()?;
        for (id, knowledge) in self.rows("knowledge_revision") {
            let revision = self.row("content_revision", *id)?;
            same_scope(revision, knowledge)?;
            require_uuid_field_eq(
                "knowledge_revision",
                knowledge,
                "document_id",
                required_uuid_field("content_revision", revision, "document_id")?,
            )?;
        }
        self.validate_mutations(manifest)?;
        for (_, item) in self.rows("content_mutation_item") {
            self.validate_item(item)?;
        }
        self.validate_intents()?;
        for (_, head) in self.rows("content_document_head") {
            self.validate_head(head)?;
        }
        Ok(())
    }

    fn validate_documents(&self, manifest: &SnapshotManifest) -> anyhow::Result<()> {
        for (document_id, document) in self.rows("content_document") {
            require_uuid_field_eq("content_document", document, "library_id", manifest.library_id)?;
            nonnegative_integer(document, "revision_sequence")?;
            self.row("content_document_head", *document_id)?;
            if let Some(parent_id) = optional_uuid_field(document, "parent_document_id")? {
                same_scope(document, self.row("content_document", parent_id)?)?;
            }
        }
        Ok(())
    }

    fn validate_revisions(&self) -> anyhow::Result<()> {
        for (_, revision) in self.rows("content_revision") {
            let document = self.document_for("content_revision", revision)?;
            same_scope(document, revision)?;
            let number = nonnegative_integer(revision, "revision_number")?;
            anyhow::ensure!(
                number > 0 && number <= nonnegative_integer(document, "revision_sequence")?,
                "snapshot revision exceeds retained revision counter"
            );
            self.revision_reference(revision, "parent_revision_id", document)?;
            if let Some(admission) = revision_admission(revision)? {
                validate_replacement(&serde_json::to_value(admission)?, revision, false)?;
            }
        }
        Ok(())
    }

    fn validate_mutations(&self, manifest: &SnapshotManifest) -> anyhow::Result<()> {
        let mut actor_reservations = HashSet::new();
        let mut scope_reservations = HashSet::new();
        for (id, reference) in self.rows("content_mutation_reference") {
            let surface = reference["request_surface"]
                .as_str()
                .filter(|surface| !surface.is_empty())
                .ok_or_else(|| anyhow!("snapshot registry requires request surface"))?;
            let actor = explicit_optional_uuid(reference, "reservation_principal_id")?;
            let scope = optional_reservation_text(reference, "idempotency_scope")?;
            let key = optional_reservation_text(reference, "idempotency_key")?;
            if let Some(key) = key {
                anyhow::ensure!(
                    actor.is_some() || scope.is_some(),
                    "snapshot reservation has no actor or scope"
                );
                if let Some(actor) = actor {
                    anyhow::ensure!(
                        actor_reservations.insert((actor, surface, key)),
                        "snapshot duplicate actor reservation"
                    );
                }
                if let Some(scope) = scope {
                    anyhow::ensure!(
                        scope_reservations.insert((scope, surface, key)),
                        "snapshot duplicate scoped reservation"
                    );
                }
            }
            require_uuid_field_eq(
                "content_mutation_reference",
                reference,
                "library_id",
                manifest.library_id,
            )?;
            let modern = self.tables.get("content_mutation").and_then(|rows| rows.get(id));
            let audit =
                self.tables.get("content_mutation_predecessor_audit").and_then(|rows| rows.get(id));
            anyhow::ensure!(
                modern.is_some() != audit.is_some(),
                "snapshot registry requires exactly one mutation representation"
            );
            let representation = modern
                .or(audit)
                .ok_or_else(|| anyhow!("snapshot missing mutation representation"))?;
            same_scope(reference, representation)?;
            if let Some(audit) = audit {
                validate_predecessor_audit(audit)?;
            }
        }
        for (_, audit) in self.rows("content_mutation_predecessor_audit") {
            let id = required_uuid_field("predecessor audit", audit, "id")?;
            same_scope(audit, self.row("content_mutation_reference", id)?)?;
        }
        for (_, mutation) in self.rows("content_mutation") {
            let id = required_uuid_field("content_mutation", mutation, "id")?;
            same_scope(mutation, self.row("content_mutation_reference", id)?)?;
            require_uuid_field_eq("content_mutation", mutation, "library_id", manifest.library_id)?;
            validate_identity(mutation)?;
            if mutation["operation_kind"] == "visibility" {
                let mutation_id = required_uuid_field("content_mutation", mutation, "id")?;
                anyhow::ensure!(
                    self.item_counts.get(&mutation_id) == Some(&1),
                    "snapshot visibility mutation has ambiguous lineage"
                );
            }
        }
        Ok(())
    }

    fn validate_item(&self, item: &Value) -> anyhow::Result<()> {
        let mutation_id = required_uuid_field("content_mutation_item", item, "mutation_id")?;
        let reference = self.row("content_mutation_reference", mutation_id)?;
        if self
            .tables
            .get("content_mutation_predecessor_audit")
            .is_some_and(|rows| rows.contains_key(&mutation_id))
        {
            anyhow::ensure!(
                terminal_mutation_item_state(&item["item_state"])
                    && explicit_optional_uuid(item, "visibility_intent_id")?.is_none(),
                "snapshot predecessor item must be terminal and unbound"
            );
            if let Some(document_id) = optional_uuid_field(item, "document_id")? {
                let document = self.row("content_document", document_id)?;
                same_scope(reference, document)?;
                self.revision_reference(item, "base_revision_id", document)?;
                self.revision_reference(item, "result_revision_id", document)?;
            } else {
                anyhow::ensure!(
                    optional_uuid_field(item, "base_revision_id")?.is_none()
                        && optional_uuid_field(item, "result_revision_id")?.is_none(),
                    "snapshot predecessor revision has no document"
                );
            }
            return Ok(());
        }
        let mutation = self.row(
            "content_mutation",
            required_uuid_field("content_mutation_item", item, "mutation_id")?,
        )?;
        if mutation["operation_kind"] == "visibility" {
            self.row(
                "content_visibility_intent",
                required_uuid_field("content_mutation_item", item, "id")?,
            )?;
        }
        if let Some(document_id) = optional_uuid_field(item, "document_id")? {
            let document = self.row("content_document", document_id)?;
            same_scope(document, mutation)?;
            self.revision_reference(item, "base_revision_id", document)?;
            self.revision_reference(item, "result_revision_id", document)?;
            if let Some(intent_id) = explicit_optional_uuid(item, "visibility_intent_id")? {
                let intent = self.row("content_visibility_intent", intent_id)?;
                require_uuid_field_eq(
                    "content_visibility_intent",
                    intent,
                    "document_id",
                    document_id,
                )?;
                let identity = self.intent_identity(intent_id)?;
                let payload = &mutation["request_identity"];
                require_uuid_field_eq(
                    "payload request identity",
                    &payload["revision"],
                    "visibilityIntentId",
                    intent_id,
                )?;
                require_uuid_field_eq(
                    "payload request identity",
                    &payload["target"],
                    "documentId",
                    document_id,
                )?;
                same_binding(payload, identity)?;
                anyhow::ensure!(
                    mutation["operation_kind"] == "replace",
                    "snapshot bound payload is not a replacement"
                );
                let revision_id =
                    required_uuid_field("content_mutation_item", item, "result_revision_id")?;
                let revision = self.row("content_revision", revision_id)?;
                anyhow::ensure!(
                    nonnegative_integer(revision, "revision_number")?
                        > nonnegative_integer(intent, "revision_floor")?,
                    "snapshot bound payload is not above intent floor"
                );
                validate_replacement(payload, revision, false)?;
            }
        } else {
            anyhow::ensure!(
                explicit_optional_uuid(item, "visibility_intent_id")?.is_none()
                    && optional_uuid_field(item, "base_revision_id")?.is_none()
                    && optional_uuid_field(item, "result_revision_id")?.is_none(),
                "snapshot bound item has no document"
            );
        }
        Ok(())
    }

    fn validate_intents(&self) -> anyhow::Result<()> {
        for (intent_id, intent) in self.rows("content_visibility_intent") {
            let document = self.document_for("content_visibility_intent", intent)?;
            anyhow::ensure!(
                nonnegative_integer(intent, "revision_floor")?
                    <= nonnegative_integer(document, "revision_sequence")?,
                "snapshot intent floor exceeds retained revision counter"
            );
            self.intent_identity(*intent_id)?;
        }
        Ok(())
    }

    fn validate_head(&self, head: &Value) -> anyhow::Result<()> {
        let document = self.document_for("content_document_head", head)?;
        self.revision_reference(head, "active_revision_id", document)?;
        self.revision_reference(head, "readable_revision_id", document)?;
        if let Some(latest) = optional_uuid_field(head, "latest_mutation_id")? {
            same_scope(document, self.row("content_mutation_reference", latest)?)?;
        }
        let obligation = explicit_optional_uuid(head, "visibility_intent_id")?;
        if let Some(intent_id) = obligation {
            let intent = self.row("content_visibility_intent", intent_id)?;
            require_uuid_field_eq(
                "content_visibility_intent",
                intent,
                "document_id",
                required_uuid_field("content_document", document, "id")?,
            )?;
        }
        anyhow::ensure!(
            head.get("readable_mutation_item_id").is_none(),
            "snapshot rejects retired published item pointer"
        );
        if let Some(revision_id) = explicit_optional_uuid(head, "readable_revision_id")? {
            let document_id = required_uuid_field("content_document", document, "id")?;
            let revision = self.row("content_revision", revision_id)?;
            let admission = revision_admission(revision)?
                .ok_or_else(|| anyhow!("snapshot readable revision lacks admission identity"))?;
            let knowledge = self.row("knowledge_revision", revision_id)?;
            same_scope(document, knowledge)?;
            require_uuid_field_eq("knowledge_revision", knowledge, "document_id", document_id)?;
            anyhow::ensure!(
                knowledge["text_state"] == "text_readable"
                    && knowledge.get("text_readable_at").is_some_and(|value| !value.is_null()),
                "snapshot published revision is not text readable"
            );
            if let Some(intent_id) = obligation {
                let identity = serde_json::to_value(&admission)?;
                anyhow::ensure!(
                    matches!(admission, ironrag_contracts::documents::RevisionAdmissionIdentity::Mutation {
                        visibility_intent_id: Some(id), ..
                    } if id == intent_id),
                    "snapshot revision admission is not bound to current intent"
                );
                same_binding(&identity, self.intent_identity(intent_id)?)?;
                let intent = self.row("content_visibility_intent", intent_id)?;
                anyhow::ensure!(
                    nonnegative_integer(revision, "revision_number")?
                        > nonnegative_integer(intent, "revision_floor")?,
                    "snapshot published admission is not above intent floor"
                );
                validate_replacement(&identity, revision, true)?;
                anyhow::ensure!(
                    knowledge["vector_state"] == "ready"
                        && knowledge.get("vector_ready_at").is_some_and(|value| !value.is_null())
                        && matches!(
                            knowledge["graph_state"].as_str(),
                            Some("ready" | "graph_degraded")
                        ),
                    "snapshot bound published revision is not READY"
                );
            }
        } else if obligation.is_some() {
            anyhow::ensure!(
                optional_uuid_field(head, "active_revision_id")?.is_none(),
                "snapshot unresolved intent carries an active public revision"
            );
        }
        Ok(())
    }

    fn document_for(&self, table: &str, row: &Value) -> anyhow::Result<&Value> {
        self.row("content_document", required_uuid_field(table, row, "document_id")?)
    }

    fn revision_reference(&self, row: &Value, field: &str, document: &Value) -> anyhow::Result<()> {
        if let Some(id) = optional_uuid_field(row, field)? {
            let revision = self.row("content_revision", id)?;
            require_uuid_field_eq(
                "content_revision",
                revision,
                "document_id",
                required_uuid_field("content_document", document, "id")?,
            )?;
        }
        Ok(())
    }

    fn intent_identity(&self, id: Uuid) -> anyhow::Result<&Value> {
        let intent = self.row("content_visibility_intent", id)?;
        let item = self.row("content_mutation_item", id)?;
        let document_id = required_uuid_field("content_visibility_intent", intent, "document_id")?;
        require_uuid_field_eq("content_mutation_item", item, "document_id", document_id)?;
        let mutation_id = required_uuid_field("content_mutation_item", item, "mutation_id")?;
        let mutation = self.row("content_mutation", mutation_id)?;
        same_scope(self.row("content_document", document_id)?, mutation)?;
        anyhow::ensure!(
            mutation["operation_kind"] == "visibility"
                && mutation["mutation_state"] == "applied"
                && item["item_state"] == "applied"
                && optional_uuid_field(item, "base_revision_id")?.is_none()
                && optional_uuid_field(item, "result_revision_id")?.is_none()
                && explicit_optional_uuid(item, "visibility_intent_id")?.is_none(),
            "snapshot visibility lineage is invalid"
        );
        anyhow::ensure!(
            self.item_counts.get(&mutation_id) == Some(&1),
            "snapshot visibility lineage is ambiguous"
        );
        let identity = &mutation["request_identity"];
        require_uuid_field_eq("request_identity", identity, "documentId", document_id)?;
        anyhow::ensure!(
            identity["operationKind"] == "visibility"
                && identity["sourceIdentity"].as_str().is_some_and(|s| !s.trim().is_empty())
                && identity.get("materializationToken").is_some(),
            "snapshot visibility identity is incomplete"
        );
        let replacement: ironrag_contracts::documents::ReplacementIdentity =
            serde_json::from_value(identity["replacement"].clone())
                .context("snapshot visibility replacement is invalid")?;
        anyhow::ensure!(
            replacement.clone().normalize()? == replacement,
            "snapshot visibility replacement is not normalized"
        );
        Ok(identity)
    }
}

fn nonnegative_integer(row: &Value, field: &str) -> anyhow::Result<i64> {
    row.get(field)
        .and_then(Value::as_i64)
        .filter(|value| (0..=i64::from(i32::MAX)).contains(value))
        .ok_or_else(|| anyhow!("snapshot requires nonnegative integer {field}"))
}

fn explicit_optional_uuid(row: &Value, field: &str) -> anyhow::Result<Option<Uuid>> {
    anyhow::ensure!(row.get(field).is_some(), "snapshot schema 4 missing {field}");
    optional_uuid_field(row, field)
}

fn same_scope(left: &Value, right: &Value) -> anyhow::Result<()> {
    for field in ["workspace_id", "library_id"] {
        require_uuid_field_eq(
            "content scope",
            right,
            field,
            required_uuid_field("content scope", left, field)?,
        )?;
    }
    Ok(())
}

fn validate_identity(mutation: &Value) -> anyhow::Result<()> {
    validate_representation_namespace(mutation)?;
    anyhow::ensure!(
        mutation.get("visibility_policy").is_none(),
        "snapshot schema 4 rejects retired policy column"
    );
    let identity = mutation
        .get("request_identity")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow!("snapshot mutation missing immutable request identity"))?;
    anyhow::ensure!(
        identity.get("visibilityPolicy").is_none(),
        "snapshot schema 4 rejects retired policy identity input"
    );
    let digest =
        format!("v1:sha256:{}", hex::encode(Sha256::digest(serde_json::to_vec(identity)?)));
    // The request records its original authenticated scope. A restore remaps
    // relational ownership but must not edit semantic identity or its digest;
    // a subsequent backup must still accept that same immutable admission.
    required_uuid_field("request_identity", identity, "workspaceId")?;
    required_uuid_field("request_identity", identity, "libraryId")?;
    anyhow::ensure!(
        mutation["request_fingerprint"].as_str() == Some(digest.as_str()),
        "snapshot immutable request fingerprint mismatch"
    );
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalMutationState {
    Applied,
    Failed,
    Conflicted,
    Canceled,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalMutationItemState {
    Applied,
    Failed,
    Conflicted,
    Skipped,
}

fn terminal_mutation_state(state: &Value) -> bool {
    state.is_string() && serde_json::from_value::<TerminalMutationState>(state.clone()).is_ok()
}

fn terminal_mutation_item_state(state: &Value) -> bool {
    state.is_string() && serde_json::from_value::<TerminalMutationItemState>(state.clone()).is_ok()
}

fn optional_reservation_text<'a>(row: &'a Value, field: &str) -> anyhow::Result<Option<&'a str>> {
    match row.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => bail!("snapshot registry requires explicit text or NULL {field}"),
    }
}

fn validate_predecessor_audit(audit: &Value) -> anyhow::Result<()> {
    validate_representation_namespace(audit)?;
    use crate::services::maintenance::content_visibility_intent_schema::PREDECESSOR_BASELINE_SHA384;
    let certificate = format!("\\x{PREDECESSOR_BASELINE_SHA384}");
    anyhow::ensure!(
        audit["predecessor_baseline_sha384"].as_str() == Some(certificate.as_str()),
        "snapshot predecessor audit lacks exact certified predecessor"
    );
    required_uuid_field("predecessor audit", audit, "origin_workspace_id")?;
    required_uuid_field("predecessor audit", audit, "origin_library_id")?;
    anyhow::ensure!(
        audit.get("request_identity") == Some(&Value::Null)
            && audit.get("request_fingerprint").is_some()
            && terminal_mutation_state(&audit["mutation_state"])
            && audit["operation_kind"] != "visibility"
            && audit.get("visibility_policy").is_none(),
        "snapshot predecessor audit is not terminal NULL-identity history"
    );
    Ok(())
}

fn validate_representation_namespace(row: &Value) -> anyhow::Result<()> {
    for field in
        ["request_surface", "idempotency_scope", "idempotency_key", "reservation_principal_id"]
    {
        anyhow::ensure!(
            row.get(field).is_none(),
            "snapshot reservation belongs only to mutation registry"
        );
    }
    Ok(())
}

fn same_binding(payload: &Value, intent: &Value) -> anyhow::Result<()> {
    for field in ["replacement", "sourceIdentity", "materializationToken"] {
        anyhow::ensure!(
            payload.get(field).is_some() && payload.get(field) == intent.get(field),
            "snapshot payload intent binding mismatch"
        );
    }
    Ok(())
}

fn revision_admission(
    revision: &Value,
) -> anyhow::Result<Option<ironrag_contracts::documents::RevisionAdmissionIdentity>> {
    let value = revision
        .get("admission_identity")
        .ok_or_else(|| anyhow!("snapshot revision missing admission_identity"))?;
    if value.is_null() {
        return Ok(None);
    }
    let admission: ironrag_contracts::documents::RevisionAdmissionIdentity =
        serde_json::from_value(value.clone()).context("snapshot revision admission is invalid")?;
    anyhow::ensure!(
        admission.clone().normalize()? == admission,
        "snapshot revision admission is not normalized"
    );
    // Replacement option fields must be explicit, not silently defaulted by serde.
    let replacement = &value["replacement"];
    for field in ["title", "languageCode", "sourceUri", "documentHint"] {
        anyhow::ensure!(replacement.get(field).is_some(), "snapshot admission missing {field}");
    }
    Ok(Some(admission))
}

fn validate_replacement(identity: &Value, revision: &Value, bound: bool) -> anyhow::Result<()> {
    let replacement = &identity["replacement"];
    anyhow::ensure!(
        replacement.is_object()
            && replacement["fileName"].as_str().is_some_and(|name| !name.is_empty()),
        "snapshot payload missing immutable replacement"
    );
    for (descriptor, column) in [
        ("checksum", "checksum"),
        ("byteSize", "byte_size"),
        ("mimeType", "mime_type"),
        ("title", "title"),
        ("languageCode", "language_code"),
        ("sourceUri", "source_uri"),
        ("documentHint", "document_hint"),
    ] {
        // Extraction hints can be updated on an unbound revision. Storage keys
        // and document labels are never part of immutable admission identity.
        if descriptor == "documentHint" && !bound {
            continue;
        }
        anyhow::ensure!(
            replacement.get(descriptor).is_some()
                && revision.get(column).is_some()
                && replacement.get(descriptor) == revision.get(column),
            "snapshot replacement does not match revision"
        );
    }
    Ok(())
}
