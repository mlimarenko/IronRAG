//! Guarded transition to text-readable publication.

use sqlx::PgPool;

use super::baseline_delta::{AdditiveBaselineDeltaSpec, apply_additive_baseline_delta};
use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_TEXT_PUBLICATION,
    REQUIRED_BASELINE_OBJECT_GROUPS_USER_PREFERENCE,
};

pub(crate) const TEXT_PUBLICATION_DELTA: AdditiveBaselineDeltaSpec = AdditiveBaselineDeltaSpec {
    operator_command: "ironrag-maintenance migrate text-publication-schema",
    predecessor_sha384_hex: super::baseline_delta::BASELINE_REVISION_USER_PREFERENCE_SHA384,
    target_sha384_hex: super::baseline_delta::BASELINE_REVISION_TEXT_PUBLICATION_SHA384,
    prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_USER_PREFERENCE,
    required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_TEXT_PUBLICATION,
    additive_sql: include_str!("../../../ops/apply-text-publication-schema.sql"),
    precondition: None,
    delta_present: (
        "select exists (select 1 from pg_attribute where attrelid=to_regclass('public.content_current_revision') and attnum>6 and not attisdropped)",
        "text publication view columns already present under predecessor checksum; refusing to stamp",
    ),
    predecessor_error: "baseline checksum is not the exact text-publication predecessor; refusing to apply or stamp",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPublicationSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_text_publication_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<TextPublicationSchemaMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &TEXT_PUBLICATION_DELTA, apply).await?;
    Ok(TextPublicationSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}
