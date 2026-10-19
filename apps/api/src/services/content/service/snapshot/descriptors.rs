use super::*;

pub(super) const POSTGRES_CONTENT_TABLES: &[&str] = &[
    "content_document",
    "content_revision",
    "content_chunk",
    "content_mutation_reference",
    "content_mutation",
    "content_mutation_predecessor_audit",
    "content_mutation_item",
    "content_visibility_intent",
    "content_document_head",
];

pub(super) const POSTGRES_RUNTIME_GRAPH_TABLES: &[&str] = &[
    "runtime_graph_snapshot",
    "runtime_graph_node",
    "runtime_graph_edge",
    "runtime_graph_evidence",
    "runtime_graph_\u{63}anonical_summary",
];

pub(super) const POSTGRES_KNOWLEDGE_TABLES: &[&str] = &[
    "knowledge_document",
    "knowledge_revision",
    "knowledge_structured_revision",
    "knowledge_structured_block",
    "knowledge_chunk",
    "knowledge_technical_fact",
    "knowledge_entity",
    "knowledge_entity_candidate",
    "knowledge_relation",
    "knowledge_relation_candidate",
    "knowledge_evidence",
    "knowledge_context_bundle",
    "knowledge_retrieval_trace",
    "knowledge_bundle_chunk",
    "knowledge_bundle_entity",
    "knowledge_bundle_relation",
    "knowledge_bundle_evidence",
    "knowledge_chunk_entity_mention",
    "knowledge_evidence_entity_support",
    "knowledge_evidence_relation_support",
    "knowledge_vector_relation_manifest",
];

pub(super) const POSTGRES_KNOWLEDGE_BASE_TABLES: &[&str] = &[
    "knowledge_document",
    "knowledge_revision",
    "knowledge_structured_revision",
    "knowledge_structured_block",
    "knowledge_chunk",
    "knowledge_technical_fact",
    "knowledge_entity",
    "knowledge_entity_candidate",
    "knowledge_relation",
    "knowledge_relation_candidate",
    "knowledge_evidence",
    "knowledge_context_bundle",
    "knowledge_retrieval_trace",
];

pub(super) const POSTGRES_KNOWLEDGE_EDGE_TABLES: &[&str] = &[
    "knowledge_bundle_chunk",
    "knowledge_bundle_entity",
    "knowledge_bundle_relation",
    "knowledge_bundle_evidence",
    "knowledge_chunk_entity_mention",
    "knowledge_evidence_entity_support",
    "knowledge_evidence_relation_support",
];

/// Tables a library owns that are NOT part of its archive: derived caches and
/// per-run artifacts that a restore rebuilds rather than carries.
///
/// They are still part of the library's footprint, so `overwrite=replace` must
/// clear them — otherwise a replaced library keeps decisions taken about the
/// content it no longer holds. The archive deliberately does not carry them:
/// they are derivable, and shipping them would freeze one run's intermediate
/// state into a portable artifact.
pub(super) const POSTGRES_LIBRARY_OWNED_UNEXPORTED_TABLES: &[&str] = &[
    "runtime_graph_community",
    "runtime_graph_extraction",
    "runtime_graph_filtered_artifact",
    "entity_dedup_verification_cache",
];

pub(super) const POSTGRES_WORKSPACE_TABLES: &[&str] = &["catalog_workspace"];

/// AI configuration tables exported by `IncludeKind::AiConfig`, listed in
/// FK-dependency order so a restore that re-enables FK enforcement (or a
/// human reading the archive) sees parents before children. Provider and
/// model catalogs (pricing inline on each model row) and model-purpose
/// eligibility are deployment-seeded with stable ids; accounts and bindings are
/// workspace/library-scoped config, with generation settings stored inline on
/// each `ai_binding` row.
pub(super) const POSTGRES_AI_CONFIG_TABLES: &[&str] = &[
    "ai_provider_catalog",
    "ai_model_catalog",
    "ai_model_binding_purpose",
    "ai_account",
    "ai_binding",
];

pub(super) const POSTGRES_LIBRARY_ROOT_TABLES: &[&str] = &["catalog_library"];

#[derive(Debug)]
pub(super) struct SnapshotRowScope {
    source_library_id: Uuid,
    target_library_id: Uuid,
    source_workspace_id: Option<Uuid>,
    target_workspace_id: Uuid,
    /// Slug already held by the target library row. The archive's
    /// `catalog_library.slug` is rewritten to this value on restore so a
    /// snapshot exported from one library can be restored into a
    /// freshly-created target library with a different slug without tripping the
    /// `catalog_library_workspace_id_slug_key` unique constraint against a
    /// sibling library in the same workspace. `None` only when the target
    /// row was already deleted before scope construction (it never is on
    /// the supported restore path).
    target_library_slug: Option<String>,
    document_ids: HashSet<Uuid>,
    revision_ids: HashSet<Uuid>,
    revision_storage: BTreeMap<Uuid, (Uuid, Option<String>)>,
    mutation_ids: HashSet<Uuid>,
    declared_blob_keys: HashSet<(String, String)>,
}

impl SnapshotRowScope {
    pub(super) const fn target_workspace_id(&self) -> Uuid {
        self.target_workspace_id
    }

    pub(super) fn new(
        source_library_id: Uuid,
        target_library_id: Uuid,
        target_workspace_id: Uuid,
        target_library_slug: Option<String>,
    ) -> Self {
        Self {
            source_library_id,
            target_library_id,
            source_workspace_id: None,
            target_workspace_id,
            target_library_slug,
            document_ids: HashSet::new(),
            revision_ids: HashSet::new(),
            revision_storage: BTreeMap::new(),
            mutation_ids: HashSet::new(),
            declared_blob_keys: HashSet::new(),
        }
    }

    pub(super) fn normalize_postgres_row(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        if is_runtime_vector_relation_name(table) {
            self.normalize_direct_library_workspace(table, row)?;
            return Ok(());
        }
        match table {
            "catalog_workspace" => {
                let workspace_id = required_uuid_field(table, row, "id")?;
                self.bind_workspace(table, workspace_id)?;
                set_uuid_field(table, row, "id", self.target_workspace_id)?;
            }
            "catalog_library" => {
                require_uuid_field_eq(table, row, "id", self.source_library_id)?;
                let workspace_id = required_uuid_field(table, row, "workspace_id")?;
                self.bind_workspace(table, workspace_id)?;
                set_uuid_field(table, row, "id", self.target_library_id)?;
                set_uuid_field(table, row, "workspace_id", self.target_workspace_id)?;
                // Keep the operator-chosen slug of the target library so a
                // restore never collides with a sibling library that already
                // owns the archive's slug in this workspace.
                if let Some(slug) = self.target_library_slug.as_deref() {
                    set_string_field(table, row, "slug", slug)?;
                }
            }
            "content_document" => {
                self.normalize_direct_library_workspace(table, row)?;
                null_field_if_present(row, "created_by_principal_id");
                let document_id = required_uuid_field(table, row, "id")?;
                self.document_ids.insert(document_id);
            }
            "content_revision" => {
                self.normalize_content_revision(table, row)?;
            }
            "content_chunk" => {
                let revision_id = required_uuid_field(table, row, "revision_id")?;
                if !self.revision_ids.contains(&revision_id) {
                    bail!(
                        "snapshot {table} row references revision {revision_id} outside target archive"
                    );
                }
            }
            "content_mutation_reference" => {
                self.normalize_direct_library_workspace(table, row)?;
                let mutation_id = required_uuid_field(table, row, "id")?;
                self.mutation_ids.insert(mutation_id);
            }
            "content_mutation" | "content_mutation_predecessor_audit" => {
                self.normalize_direct_library_workspace(table, row)?;
                self.validate_optional_member(table, row, "id", &self.mutation_ids)?;
                if table == "content_mutation" {
                    null_field_if_present(row, "requested_by_principal_id");
                }
            }
            "content_mutation_item" => {
                let mutation_id = required_uuid_field(table, row, "mutation_id")?;
                if !self.mutation_ids.contains(&mutation_id) {
                    bail!(
                        "snapshot {table} row references mutation {mutation_id} outside target archive"
                    );
                }
                self.validate_optional_member(table, row, "document_id", &self.document_ids)?;
                self.validate_optional_member(table, row, "base_revision_id", &self.revision_ids)?;
                self.validate_optional_member(
                    table,
                    row,
                    "result_revision_id",
                    &self.revision_ids,
                )?;
            }
            "content_visibility_intent" => {
                let document_id = required_uuid_field(table, row, "document_id")?;
                anyhow::ensure!(
                    self.document_ids.contains(&document_id),
                    "snapshot intent document is outside archive"
                );
                required_uuid_field(table, row, "mutation_item_id")?;
            }
            "content_document_head" => {
                // Attempts are deployment-local runtime rows, not publication
                // authority. Preserve the revision's immutable admission instead.
                null_field_if_present(row, "latest_successful_attempt_id");
                let document_id = required_uuid_field(table, row, "document_id")?;
                if !self.document_ids.contains(&document_id) {
                    bail!(
                        "snapshot {table} row references document {document_id} outside target archive"
                    );
                }
                self.validate_optional_member(
                    table,
                    row,
                    "active_revision_id",
                    &self.revision_ids,
                )?;
                self.validate_optional_member(
                    table,
                    row,
                    "readable_revision_id",
                    &self.revision_ids,
                )?;
                self.validate_optional_member(
                    table,
                    row,
                    "latest_mutation_id",
                    &self.mutation_ids,
                )?;
            }
            "runtime_graph_snapshot"
            | "runtime_graph_node"
            | "runtime_graph_edge"
            | "runtime_graph_evidence"
            | "runtime_graph_\u{63}anonical_summary" => {
                self.normalize_direct_library_workspace(table, row)?;
            }
            "knowledge_revision" => {
                self.normalize_direct_library_workspace(table, row)?;
                let revision_id = required_uuid_field(table, row, "revision_id")?;
                let document_id = required_uuid_field(table, row, "document_id")?;
                let (owner_document, storage_key) =
                    self.revision_storage.get(&revision_id).ok_or_else(|| {
                        anyhow!("snapshot knowledge revision has no archived content owner")
                    })?;
                anyhow::ensure!(
                    *owner_document == document_id,
                    "snapshot knowledge revision document differs from content owner"
                );
                row["storage_ref"] = storage_key
                    .as_ref()
                    .map_or(serde_json::Value::Null, |key| serde_json::Value::String(key.clone()));
            }
            "knowledge_document"
            | "knowledge_structured_revision"
            | "knowledge_structured_block"
            | "knowledge_chunk"
            | "knowledge_technical_fact"
            | "knowledge_entity"
            | "knowledge_entity_candidate"
            | "knowledge_relation"
            | "knowledge_relation_candidate"
            | "knowledge_evidence"
            | "knowledge_context_bundle"
            | "knowledge_retrieval_trace" => {
                self.normalize_direct_library_workspace(table, row)?;
            }
            "knowledge_bundle_chunk"
            | "knowledge_bundle_entity"
            | "knowledge_bundle_relation"
            | "knowledge_bundle_evidence"
            | "knowledge_chunk_entity_mention"
            | "knowledge_evidence_entity_support"
            | "knowledge_evidence_relation_support"
            | "knowledge_vector_relation_manifest" => {
                require_uuid_field_eq(table, row, "library_id", self.source_library_id)?;
                set_uuid_field(table, row, "library_id", self.target_library_id)?;
            }
            "ai_provider_catalog" | "ai_model_catalog" | "ai_model_binding_purpose" => {
                // System-seeded catalogs carry no workspace/library scope and
                // keep their stable ids; nothing to remap.
            }
            "ai_account" | "ai_binding" => {
                self.normalize_ai_config_scope(table, row)?;
            }
            other => bail!("snapshot import has no row-scope validator for table `{other}`"),
        }
        Ok(())
    }

    /// Registers the remapped content owner before knowledge rows and blobs can reference it.
    fn normalize_content_revision(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        self.normalize_direct_library_workspace(table, row)?;
        null_field_if_present(row, "created_by_principal_id");
        let document_id = required_uuid_field(table, row, "document_id")?;
        if !self.document_ids.contains(&document_id) {
            bail!("snapshot {table} row references document {document_id} outside target archive");
        }
        let revision_id = required_uuid_field(table, row, "id")?;
        self.revision_ids.insert(revision_id);
        if let Some(storage_key) = string_field(row, "storage_key") {
            let source_key = storage_key.to_string();
            let target_key = self.rewrite_storage_key(table, storage_key)?;
            set_string_field(table, row, "storage_key", &target_key)?;
            self.declared_blob_keys.insert((source_key, target_key));
        }
        self.revision_storage.insert(
            revision_id,
            (document_id, string_field(row, "storage_key").map(str::to_owned)),
        );
        Ok(())
    }

    pub(super) fn normalize_blob_key(&self, storage_key: &str) -> anyhow::Result<String> {
        let target_key = self.rewrite_storage_key("blob", storage_key)?;
        if !self.declared_blob_keys.contains(&(storage_key.to_string(), target_key.clone())) {
            bail!("snapshot blob `{storage_key}` is not declared by a content_revision row");
        }
        Ok(target_key)
    }

    pub(super) fn normalize_direct_library_workspace(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        require_uuid_field_eq(table, row, "library_id", self.source_library_id)?;
        set_uuid_field(table, row, "library_id", self.target_library_id)?;
        if row.get("workspace_id").is_some() {
            let workspace_id = required_uuid_field(table, row, "workspace_id")?;
            self.bind_workspace(table, workspace_id)?;
            set_uuid_field(table, row, "workspace_id", self.target_workspace_id)?;
        }
        Ok(())
    }

    /// Normalizes an AI-config row (`ai_account` or `ai_binding`).
    /// `workspace_id` / `library_id` are nullable scope columns: a
    /// workspace-scoped row carries only `workspace_id`, a library-scoped row
    /// carries both. Each non-null
    /// scope id is rewritten to the restore target; `library_id`, when
    /// present, must belong to the exported library. `iam_principal` author
    /// references are dropped because principals never travel in a snapshot.
    pub(super) fn normalize_ai_config_scope(
        &mut self,
        table: &str,
        row: &mut serde_json::Value,
    ) -> anyhow::Result<()> {
        if let Some(library_id) = optional_uuid_field(row, "library_id")
            .with_context(|| format!("parse {table}.library_id"))?
        {
            if library_id != self.source_library_id {
                bail!(
                    "snapshot {table} row references library {library_id} outside the exported library {}",
                    self.source_library_id
                );
            }
            set_uuid_field(table, row, "library_id", self.target_library_id)?;
        }
        if let Some(workspace_id) = optional_uuid_field(row, "workspace_id")
            .with_context(|| format!("parse {table}.workspace_id"))?
        {
            self.bind_workspace(table, workspace_id)?;
            set_uuid_field(table, row, "workspace_id", self.target_workspace_id)?;
        }
        null_field_if_present(row, "created_by_principal_id");
        null_field_if_present(row, "updated_by_principal_id");
        Ok(())
    }

    pub(super) fn validate_optional_member(
        &self,
        table: &str,
        row: &serde_json::Value,
        field: &str,
        allowed_ids: &HashSet<Uuid>,
    ) -> anyhow::Result<()> {
        if let Some(id) =
            optional_uuid_field(row, field).with_context(|| format!("parse {table}.{field}"))?
            && !allowed_ids.contains(&id)
        {
            bail!("snapshot {table} row references {field} {id} outside target archive");
        }
        Ok(())
    }

    pub(super) fn bind_workspace(
        &mut self,
        source: &str,
        workspace_id: Uuid,
    ) -> anyhow::Result<()> {
        match self.source_workspace_id {
            Some(current) if current != workspace_id => bail!(
                "snapshot {source} row belongs to workspace {workspace_id}, expected {current}"
            ),
            Some(_) => Ok(()),
            None => {
                self.source_workspace_id = Some(workspace_id);
                Ok(())
            }
        }
    }

    pub(super) fn rewrite_storage_key(
        &self,
        source: &str,
        storage_key: &str,
    ) -> anyhow::Result<String> {
        let source_workspace_id = self.source_workspace_id.ok_or_else(|| {
            anyhow!("snapshot {source} storage_key arrived before workspace scope")
        })?;
        let source_prefix = format!("content/{source_workspace_id}/{}/", self.source_library_id);
        let Some(suffix) = storage_key.strip_prefix(&source_prefix) else {
            bail!("snapshot {source} storage_key is outside snapshot library storage prefix");
        };
        let target_prefix =
            format!("content/{}/{}/", self.target_workspace_id, self.target_library_id);
        Ok(format!("{target_prefix}{suffix}"))
    }
}

pub(super) fn string_field<'a>(row: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    row.get(field).and_then(|value| value.as_str()).filter(|value| !value.is_empty())
}

pub(super) fn optional_uuid_field(
    row: &serde_json::Value,
    field: &str,
) -> anyhow::Result<Option<Uuid>> {
    match row.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if value.is_empty() => Ok(None),
        Some(serde_json::Value::String(value)) => {
            Uuid::parse_str(value).map(Some).with_context(|| format!("parse uuid field `{field}`"))
        }
        Some(_) => bail!("snapshot field `{field}` must be a uuid string"),
    }
}

pub(super) fn required_uuid_field(
    table: &str,
    row: &serde_json::Value,
    field: &str,
) -> anyhow::Result<Uuid> {
    optional_uuid_field(row, field)?
        .ok_or_else(|| anyhow!("snapshot {table} row missing required uuid field `{field}`"))
}

pub(super) fn require_uuid_field_eq(
    table: &str,
    row: &serde_json::Value,
    field: &str,
    expected: Uuid,
) -> anyhow::Result<()> {
    let actual = required_uuid_field(table, row, field)?;
    if actual != expected {
        bail!("snapshot {table}.{field} is {actual}, expected {expected}");
    }
    Ok(())
}

pub(super) fn set_uuid_field(
    table: &str,
    row: &mut serde_json::Value,
    field: &str,
    value: Uuid,
) -> anyhow::Result<()> {
    set_string_field(table, row, field, &value.to_string())
}

pub(super) fn set_string_field(
    table: &str,
    row: &mut serde_json::Value,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    let object =
        row.as_object_mut().ok_or_else(|| anyhow!("snapshot {table} row is not an object"))?;
    object.insert(field.to_string(), serde_json::Value::String(value.to_string()));
    Ok(())
}

/// Sets `field` to JSON null when the row already carries it. Used to drop
/// deployment-specific references (e.g. `iam_principal` author ids) that
/// must not survive a cross-stack restore.
pub(super) fn null_field_if_present(row: &mut serde_json::Value, field: &str) {
    if let Some(object) = row.as_object_mut()
        && object.contains_key(field)
    {
        object.insert(field.to_string(), serde_json::Value::Null);
    }
}

#[derive(Debug)]
pub(super) struct SnapshotManifestSections {
    pub(super) postgres_tables: HashSet<String>,
    /// Declared vector relations derived from the authoritative Postgres
    /// section list; schema 4 requires `vector_shards` to match this set exactly.
    pub(super) vector_shards: BTreeMap<String, u64>,
}

impl SnapshotManifestSections {
    pub(super) fn from_manifest(manifest: &SnapshotManifest) -> anyhow::Result<Self> {
        validate_snapshot_manifest_blob_declaration(manifest)?;
        let (postgres_tables, postgres_vector_shards) =
            collect_manifest_postgres_sections(&manifest.postgres_tables)?;
        let declared_vector_shards = collect_declared_manifest_vector_shards(
            &manifest.vector_shards,
            &postgres_vector_shards,
        )?;

        if declared_vector_shards != postgres_vector_shards {
            bail!("snapshot vector_shards must exactly match declared postgres vector tables");
        }

        Ok(Self { postgres_tables, vector_shards: postgres_vector_shards })
    }

    pub(super) fn require_postgres_table<'a>(&self, table: &'a str) -> anyhow::Result<&'a str> {
        let table = validate_snapshot_pg_table_name(table)?;
        if self.postgres_tables.contains(table) {
            Ok(table)
        } else {
            bail!("snapshot entry references undeclared postgres table `{table}`")
        }
    }
}

pub(super) fn validate_snapshot_manifest_blob_declaration(
    manifest: &SnapshotManifest,
) -> anyhow::Result<()> {
    IncludeKind::validate(&manifest.include_kinds)?;
    let declares_blobs = manifest.include_kinds.contains(&IncludeKind::Blobs);
    if manifest.has_blobs != declares_blobs {
        bail!("snapshot manifest has inconsistent blob declaration");
    }
    Ok(())
}

/// Declared Postgres relations paired with the per-dim vector shards derived
/// from them. The pair travels together from manifest validation into restore
/// preflight, so it carries one name instead of an anonymous tuple.
pub(super) type ManifestPostgresSections = (HashSet<String>, BTreeMap<String, u64>);

pub(super) fn collect_manifest_postgres_sections(
    tables: &[String],
) -> anyhow::Result<ManifestPostgresSections> {
    let mut postgres_tables = HashSet::new();
    let mut postgres_vector_shards = BTreeMap::new();
    for table in tables {
        let table = validate_snapshot_pg_table_name(table)?;
        if !postgres_tables.insert(table.to_string()) {
            bail!("snapshot manifest declares postgres table `{table}` more than once");
        }
        if is_runtime_vector_relation_name(table) {
            let dim = parse_per_dim_vector_collection_dim(table).ok_or_else(|| {
                anyhow!("snapshot manifest declares malformed vector relation `{table}`")
            })?;
            checked_vector_dim_i32(dim).with_context(|| {
                format!("snapshot manifest vector relation `{table}` has an invalid dimension")
            })?;
            postgres_vector_shards.insert(table.to_string(), dim);
        }
    }
    Ok((postgres_tables, postgres_vector_shards))
}

pub(super) fn collect_declared_manifest_vector_shards(
    shards: &[VectorShardEntry],
    postgres_vector_shards: &BTreeMap<String, u64>,
) -> anyhow::Result<BTreeMap<String, u64>> {
    let mut declared_vector_shards = BTreeMap::new();
    for shard in shards {
        let expected_name =
            expected_per_dim_vector_relation_name(&shard.name).ok_or_else(|| {
                anyhow!("snapshot manifest declares malformed vector shard `{}`", shard.name)
            })?;
        if expected_name != shard.name {
            bail!(
                "snapshot manifest vector shard `{}` is not the expected relation name",
                shard.name
            );
        }
        let name_dimension = parse_per_dim_vector_collection_dim(&expected_name)
            .ok_or_else(|| anyhow!("snapshot manifest vector shard has no dimension"))?;
        checked_vector_dim_i32(name_dimension).with_context(|| {
            format!("snapshot manifest vector shard `{}` has an invalid dimension", shard.name)
        })?;
        if shard.dim != name_dimension {
            bail!(
                "snapshot manifest vector shard `{}` declares dimension {} but its name encodes {}",
                shard.name,
                shard.dim,
                name_dimension
            );
        }
        if declared_vector_shards.insert(expected_name.clone(), shard.dim).is_some() {
            bail!("snapshot manifest declares vector shard `{expected_name}` more than once");
        }
        if postgres_vector_shards.get(&expected_name) != Some(&shard.dim) {
            bail!(
                "snapshot manifest vector shard `{expected_name}` has no matching postgres table"
            );
        }
    }
    Ok(declared_vector_shards)
}

pub(super) fn require_known_snapshot_pg_table(table: &str) -> anyhow::Result<&'static str> {
    POSTGRES_WORKSPACE_TABLES
        .iter()
        .chain(POSTGRES_AI_CONFIG_TABLES.iter())
        .chain(POSTGRES_LIBRARY_ROOT_TABLES.iter())
        .chain(POSTGRES_CONTENT_TABLES.iter())
        .chain(POSTGRES_RUNTIME_GRAPH_TABLES.iter())
        .chain(POSTGRES_KNOWLEDGE_TABLES.iter())
        .copied()
        .find(|candidate| *candidate == table)
        .ok_or_else(|| anyhow!("unknown snapshot postgres table `{table}`"))
}

pub(super) fn validate_snapshot_pg_table_name(table: &str) -> anyhow::Result<&str> {
    if require_known_snapshot_pg_table(table).is_ok() || is_runtime_vector_relation_name(table) {
        Ok(table)
    } else {
        bail!("unknown snapshot postgres table `{table}`")
    }
}

pub(super) fn is_runtime_vector_relation_name(name: &str) -> bool {
    is_expected_per_dim_vector_collection_name(name)
}

pub(super) fn quote_pg_identifier(identifier: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!identifier.is_empty(), "empty SQL identifier");
    anyhow::ensure!(
        identifier.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
        "unsafe SQL identifier {identifier}"
    );
    anyhow::ensure!(
        identifier.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_'),
        "SQL identifier must start with a letter or underscore: {identifier}"
    );
    Ok(format!("\"{}\"", identifier.replace('"', "\"\"")))
}

/// Max attempts for a savepoint-guarded vector-shard write (Workstream R / R2,
/// R3). The first attempt plus a small bounded set of retries.
pub(super) const RESTORE_SAVEPOINT_MAX_ATTEMPTS: u32 = 5;
/// Base backoff between savepoint retries; multiplied by the attempt index.
pub(super) const RESTORE_SAVEPOINT_BACKOFF_BASE: std::time::Duration =
    std::time::Duration::from_millis(25);
/// Archive replay is idempotent only for the exact physical key. A different
/// key carrying the same logical object/profile/generation must reach the
/// logical unique index and abort the outer restore transaction rather than
/// being silently selected as a winner.
pub(super) const PG_VECTOR_RESTORE_KEY_REPLAY_CONFLICT: &str = "ON CONFLICT (key) DO NOTHING";
pub(super) const PG_VECTOR_LOGICAL_KEY_INDEX_SUFFIX: &str = "_logical_key";

/// `PostgreSQL` deadlock SQLSTATE — the transaction was chosen as the deadlock
/// victim and rolled back. Safe to retry after rolling back to a savepoint.
pub(super) const PG_SQLSTATE_DEADLOCK_DETECTED: &str = "40P01";

/// Returns `true` when a `SQLx` error inside the restore transaction is a
/// transient contention that a savepoint rollback + retry can recover from
/// (Workstream R / R2 + in-transaction R3):
///
/// - `40P01` `deadlock_detected` — parallel restores fight over the shared
///   per-dim vector shard; the loser is rolled back and can replay.
/// - `42P07` `duplicate_table` / `42710` `duplicate_object` — two sessions both
///   passed the `CREATE ... IF NOT EXISTS` existence check and raced the
///   catalog insert; on retry the relation already exists and the create
///   no-ops.
/// - `23505` `unique_violation` — the same race surfacing as a `pg_catalog`
///   unique-index collision. A violation of the vector logical-key index is a
///   deterministic archive conflict and is explicitly not retried.
pub(super) fn pg_error_is_retryable_restore_contention(error: &sqlx::Error) -> bool {
    let Some(database_error) = error.as_database_error() else {
        return false;
    };
    let Some(code) = database_error.code() else {
        return false;
    };
    pg_restore_sqlstate_is_retryable(code.as_ref(), database_error.constraint())
}

pub(super) fn pg_restore_sqlstate_is_retryable(code: &str, constraint: Option<&str>) -> bool {
    match code {
        PG_SQLSTATE_DEADLOCK_DETECTED | "42P07" | "42710" => true,
        "23505" => {
            !constraint.is_some_and(|name| name.ends_with(PG_VECTOR_LOGICAL_KEY_INDEX_SUFFIX))
        }
        _ => false,
    }
}
