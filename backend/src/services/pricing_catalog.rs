use std::collections::BTreeSet;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use tracing::info;
use uuid::Uuid;

use crate::{
    app::state::{AppState, PricingCatalogBootstrapSettings},
    domains::{
        pricing_catalog::{PricingResolution, PricingResolutionStatus},
        usage_governance::{PricingCoverageStatus, PricingCoverageSummary, PricingCoverageWarning},
    },
    infra::repositories::{self, ModelPricingCatalogEntryRow},
    integrations::provider_catalog,
};

#[derive(Debug, Clone, Default)]
pub struct PricingCatalogService;

#[derive(Debug, Clone)]
pub struct UpsertPricingCatalogEntry {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub source_kind: String,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PricingLookupRequest {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct PricingCatalogFilters {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: Option<String>,
    pub billing_unit: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UsageCostLookupRequest {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UsageCostResolution {
    pub status: PricingResolutionStatus,
    pub entry: Option<ModelPricingCatalogEntryRow>,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub pricing_snapshot_json: serde_json::Value,
}

pub async fn list_pricing_entries(
    state: &AppState,
    workspace_id: Option<Uuid>,
) -> anyhow::Result<Vec<ModelPricingCatalogEntryRow>> {
    repositories::list_model_pricing_catalog_entries(&state.persistence.postgres, workspace_id)
        .await
        .context("failed to list model pricing catalog entries")
}

pub async fn list_pricing_entries_filtered(
    state: &AppState,
    mut filters: PricingCatalogFilters,
) -> anyhow::Result<Vec<ModelPricingCatalogEntryRow>> {
    normalize_optional_string(&mut filters.provider_kind, |value| value.to_ascii_lowercase());
    normalize_optional_string(&mut filters.model_name, |value| value.trim().to_string());
    normalize_optional_string(&mut filters.capability, |value| value.to_ascii_lowercase());
    normalize_optional_string(&mut filters.billing_unit, |value| value.to_ascii_lowercase());
    normalize_optional_string(&mut filters.status, |value| value.to_ascii_lowercase());

    let rows = list_pricing_entries(state, filters.workspace_id).await?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            filters
                .provider_kind
                .as_deref()
                .is_none_or(|value| row.provider_kind.eq_ignore_ascii_case(value))
                && filters
                    .model_name
                    .as_deref()
                    .is_none_or(|value| row.model_name.eq_ignore_ascii_case(value))
                && filters
                    .capability
                    .as_deref()
                    .is_none_or(|value| row.capability.eq_ignore_ascii_case(value))
                && filters
                    .billing_unit
                    .as_deref()
                    .is_none_or(|value| row.billing_unit.eq_ignore_ascii_case(value))
                && filters
                    .status
                    .as_deref()
                    .is_none_or(|value| row.status.eq_ignore_ascii_case(value))
        })
        .collect())
}

pub async fn create_pricing_entry(
    state: &AppState,
    mut entry: UpsertPricingCatalogEntry,
) -> anyhow::Result<ModelPricingCatalogEntryRow> {
    normalize_pricing_entry(&mut entry);
    validate_pricing_entry(&entry)?;
    reject_overlapping_window(state, &entry, None).await?;

    repositories::create_model_pricing_catalog_entry(
        &state.persistence.postgres,
        &repositories::NewModelPricingCatalogEntry {
            workspace_id: entry.workspace_id,
            provider_kind: entry.provider_kind,
            model_name: entry.model_name,
            capability: entry.capability,
            billing_unit: entry.billing_unit,
            input_price: entry.input_price,
            output_price: entry.output_price,
            currency: entry.currency,
            source_kind: entry.source_kind,
            note: entry.note,
            effective_from: entry.effective_from,
        },
    )
    .await
    .context("failed to create model pricing catalog entry")
}

pub async fn update_pricing_entry(
    state: &AppState,
    pricing_id: Uuid,
    mut entry: UpsertPricingCatalogEntry,
) -> anyhow::Result<Option<ModelPricingCatalogEntryRow>> {
    normalize_pricing_entry(&mut entry);
    validate_pricing_entry(&entry)?;
    let existing = repositories::get_model_pricing_catalog_entry_by_id(
        &state.persistence.postgres,
        pricing_id,
    )
    .await
    .context("failed to load model pricing catalog entry for supersede")?;
    let Some(existing) = existing else {
        return Ok(None);
    };

    if existing.status != "active" {
        return Err(anyhow!("only active pricing entries can be superseded"));
    }
    if existing.workspace_id != entry.workspace_id
        || !existing.provider_kind.eq_ignore_ascii_case(&entry.provider_kind)
        || !existing.model_name.eq_ignore_ascii_case(&entry.model_name)
        || !existing.capability.eq_ignore_ascii_case(&entry.capability)
        || !existing.billing_unit.eq_ignore_ascii_case(&entry.billing_unit)
    {
        return Err(anyhow!("pricing target identity cannot change during supersede update"));
    }
    if entry.effective_from <= existing.effective_from {
        return Err(anyhow!(
            "supersede effective_from must be later than the existing pricing entry"
        ));
    }

    reject_overlapping_window(state, &entry, Some(existing.id)).await?;
    repositories::supersede_overlapping_model_pricing_catalog_entries(
        &state.persistence.postgres,
        existing.workspace_id,
        &existing.provider_kind,
        &existing.model_name,
        &existing.capability,
        &existing.billing_unit,
        entry.effective_from,
    )
    .await
    .context("failed to supersede overlapping pricing entries")?;

    let row = repositories::create_model_pricing_catalog_entry(
        &state.persistence.postgres,
        &repositories::NewModelPricingCatalogEntry {
            workspace_id: existing.workspace_id,
            provider_kind: existing.provider_kind,
            model_name: existing.model_name,
            capability: existing.capability,
            billing_unit: existing.billing_unit,
            input_price: entry.input_price,
            output_price: entry.output_price,
            currency: entry.currency,
            source_kind: "manual".to_string(),
            note: entry.note,
            effective_from: entry.effective_from,
        },
    )
    .await
    .context("failed to create superseding model pricing catalog entry")?;

    Ok(Some(row))
}

pub async fn deactivate_pricing_entry(
    state: &AppState,
    pricing_id: Uuid,
) -> anyhow::Result<Option<ModelPricingCatalogEntryRow>> {
    if let Some(existing) =
        repositories::get_model_pricing_catalog_entry_by_id(&state.persistence.postgres, pricing_id)
            .await
            .context("failed to load pricing catalog entry before deactivation")?
    {
        if existing.status == "inactive" {
            return Ok(Some(existing));
        }
    } else {
        return Ok(None);
    }

    repositories::deactivate_model_pricing_catalog_entry(&state.persistence.postgres, pricing_id)
        .await
        .context("failed to deactivate model pricing catalog entry")
}

pub async fn build_pricing_coverage_summary(
    state: &AppState,
    workspace_id: Uuid,
    profile: &crate::domains::provider_profiles::EffectiveProviderProfile,
) -> anyhow::Result<PricingCoverageSummary> {
    let targets = provider_catalog::pricing_targets_for_profile(profile);
    let mut warnings = Vec::new();
    let mut covered_targets = 0usize;

    for target in targets {
        let resolution = resolve_pricing(
            state,
            PricingLookupRequest {
                workspace_id: Some(workspace_id),
                provider_kind: target.provider_kind.as_str().to_string(),
                model_name: target.model_name.clone(),
                capability: target.capability.clone(),
                billing_unit: target.billing_unit.clone(),
                at: Utc::now(),
            },
        )
        .await?;

        if matches!(resolution.status, PricingResolutionStatus::Priced) {
            covered_targets += 1;
        } else {
            warnings.push(PricingCoverageWarning {
                provider_kind: target.provider_kind.as_str().to_string(),
                model_name: target.model_name,
                capability: target.capability,
                billing_unit: target.billing_unit,
                message: format!("No active price entry is configured for role {}.", target.role),
            });
        }
    }

    let missing_targets = warnings.len();
    let status = if missing_targets == 0 {
        PricingCoverageStatus::Covered
    } else if covered_targets == 0 {
        PricingCoverageStatus::Missing
    } else {
        PricingCoverageStatus::Partial
    };

    Ok(PricingCoverageSummary { status, covered_targets, missing_targets, warnings })
}

pub async fn resolve_pricing(
    state: &AppState,
    mut request: PricingLookupRequest,
) -> anyhow::Result<PricingResolution> {
    normalize_lookup_request(&mut request);
    let entry = find_effective_pricing_entry(state, &request).await?;
    Ok(PricingResolution {
        status: if entry.is_some() {
            PricingResolutionStatus::Priced
        } else {
            PricingResolutionStatus::PricingMissing
        },
        entry_id: entry.as_ref().map(|row| row.id),
        pricing_snapshot_json: build_pricing_snapshot(
            entry.as_ref(),
            if entry.is_some() {
                PricingResolutionStatus::Priced
            } else {
                PricingResolutionStatus::PricingMissing
            },
            None,
            None,
            None,
            &request.provider_kind,
            &request.model_name,
            &request.capability,
            &request.billing_unit,
            request.at,
        ),
    })
}

pub async fn resolve_usage_cost(
    state: &AppState,
    mut request: UsageCostLookupRequest,
) -> anyhow::Result<UsageCostResolution> {
    normalize_usage_request(&mut request);
    let lookup = PricingLookupRequest {
        workspace_id: request.workspace_id,
        provider_kind: request.provider_kind.clone(),
        model_name: request.model_name.clone(),
        capability: request.capability.clone(),
        billing_unit: request.billing_unit.clone(),
        at: request.at,
    };
    let entry = find_effective_pricing_entry(state, &lookup).await?;
    let Some(entry) = entry else {
        return Ok(UsageCostResolution {
            status: PricingResolutionStatus::PricingMissing,
            entry: None,
            estimated_cost: None,
            currency: None,
            pricing_snapshot_json: build_pricing_snapshot(
                None,
                PricingResolutionStatus::PricingMissing,
                request.prompt_tokens,
                request.completion_tokens,
                request.total_tokens,
                &request.provider_kind,
                &request.model_name,
                &request.capability,
                &request.billing_unit,
                request.at,
            ),
        });
    };

    let resolution = estimate_usage_cost_with_entry(
        &entry,
        request.prompt_tokens,
        request.completion_tokens,
        request.total_tokens,
    );
    let status = resolution.0.clone();
    Ok(UsageCostResolution {
        status: status.clone(),
        entry: Some(entry.clone()),
        estimated_cost: resolution.1,
        currency: resolution.1.as_ref().map(|_| entry.currency.clone()),
        pricing_snapshot_json: build_pricing_snapshot(
            Some(&entry),
            status,
            request.prompt_tokens,
            request.completion_tokens,
            request.total_tokens,
            &request.provider_kind,
            &request.model_name,
            &request.capability,
            &request.billing_unit,
            request.at,
        ),
    })
}

pub async fn bootstrap_from_env_if_enabled(state: &AppState) -> anyhow::Result<usize> {
    if !state.pricing_catalog_bootstrap.seed_from_env {
        return Ok(0);
    }

    let now = Utc::now();
    let mut seen = BTreeSet::new();
    let mut seeded = 0usize;
    let mut synced = 0usize;
    let existing_rows = list_pricing_entries(state, None).await?;

    for seed in build_seed_entries(&state.pricing_catalog_bootstrap, now)? {
        let dedupe_key = (
            seed.workspace_id,
            seed.provider_kind.clone(),
            seed.model_name.clone(),
            seed.capability.clone(),
            seed.billing_unit.clone(),
        );
        if !seen.insert(dedupe_key) {
            continue;
        }

        let existing = existing_rows
            .iter()
            .find(|row| {
                row.status.eq_ignore_ascii_case("active")
                    && row.workspace_id == seed.workspace_id
                    && row.provider_kind.eq_ignore_ascii_case(&seed.provider_kind)
                    && row.model_name.eq_ignore_ascii_case(&seed.model_name)
                    && row.capability.eq_ignore_ascii_case(&seed.capability)
                    && row.billing_unit.eq_ignore_ascii_case(&seed.billing_unit)
            })
            .cloned();

        match existing {
            Some(row) if pricing_row_matches_seed(&row, &seed) => {}
            Some(row) if row.source_kind.eq_ignore_ascii_case("seeded") => {
                sync_seeded_pricing_entry(state, row, seed, now).await?;
                synced += 1;
            }
            Some(_) => {}
            None => {
                create_pricing_entry(state, seed).await?;
                seeded += 1;
            }
        }
    }

    if seeded > 0 || synced > 0 {
        info!(
            seeded_pricing_entries = seeded,
            synced_seeded_pricing_entries = synced,
            "bootstrapped pricing catalog from built-in provider catalog"
        );
    }

    Ok(seeded + synced)
}

#[must_use]
pub fn bootstrap_settings(state: &AppState) -> PricingCatalogBootstrapSettings {
    state.pricing_catalog_bootstrap.clone()
}

async fn reject_overlapping_window(
    state: &AppState,
    entry: &UpsertPricingCatalogEntry,
    exclude_id: Option<Uuid>,
) -> anyhow::Result<()> {
    let rows = list_pricing_entries(state, entry.workspace_id).await?;
    let overlaps = rows.into_iter().any(|row| {
        exclude_id != Some(row.id)
            && row.workspace_id == entry.workspace_id
            && row.provider_kind.eq_ignore_ascii_case(&entry.provider_kind)
            && row.model_name.eq_ignore_ascii_case(&entry.model_name)
            && row.capability.eq_ignore_ascii_case(&entry.capability)
            && row.billing_unit.eq_ignore_ascii_case(&entry.billing_unit)
            && row.effective_to.is_none_or(|value| value > entry.effective_from)
    });
    if overlaps {
        return Err(anyhow!(
            "pricing window overlap: another price entry already covers this effective_from"
        ));
    }
    Ok(())
}

fn normalize_optional_string(value: &mut Option<String>, map: impl FnOnce(&str) -> String) {
    if let Some(inner) = value.take() {
        let normalized = map(inner.trim());
        if normalized.is_empty() {
            *value = None;
        } else {
            *value = Some(normalized);
        }
    }
}

fn build_seed_entries(
    bootstrap: &PricingCatalogBootstrapSettings,
    effective_from: DateTime<Utc>,
) -> anyhow::Result<Vec<UpsertPricingCatalogEntry>> {
    provider_catalog::built_in_pricing_catalog_seeds()
        .into_iter()
        .map(|seed| {
            Ok(UpsertPricingCatalogEntry {
                workspace_id: None,
                provider_kind: seed.provider_kind.as_str().to_string(),
                model_name: seed.model_name.to_string(),
                capability: seed.capability.to_string(),
                billing_unit: seed.billing_unit.to_string(),
                input_price: parse_optional_seed_price(seed.input_price)?,
                output_price: parse_optional_seed_price(seed.output_price)?,
                currency: bootstrap.default_currency.clone(),
                source_kind: "seeded".to_string(),
                note: Some(seed.note.to_string()),
                effective_from,
            })
        })
        .collect()
}

fn parse_optional_seed_price(value: Option<&str>) -> anyhow::Result<Option<Decimal>> {
    value.map(Decimal::from_str_exact).transpose().context("invalid built-in seed price")
}

fn pricing_row_matches_seed(
    row: &ModelPricingCatalogEntryRow,
    seed: &UpsertPricingCatalogEntry,
) -> bool {
    row.source_kind.eq_ignore_ascii_case("seeded")
        && row.currency.eq_ignore_ascii_case(&seed.currency)
        && row.input_price == seed.input_price
        && row.output_price == seed.output_price
        && row.note.as_deref() == seed.note.as_deref()
}

async fn sync_seeded_pricing_entry(
    state: &AppState,
    existing: ModelPricingCatalogEntryRow,
    mut replacement: UpsertPricingCatalogEntry,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let effective_from = if existing.effective_from >= now {
        existing.effective_from + Duration::seconds(1)
    } else {
        now
    };

    repositories::supersede_overlapping_model_pricing_catalog_entries(
        &state.persistence.postgres,
        existing.workspace_id,
        &existing.provider_kind,
        &existing.model_name,
        &existing.capability,
        &existing.billing_unit,
        effective_from,
    )
    .await
    .context("failed to supersede stale seeded pricing entry")?;

    replacement.effective_from = effective_from;
    create_pricing_entry(state, replacement)
        .await
        .context("failed to replace stale seeded pricing entry")?;
    Ok(())
}

async fn find_effective_pricing_entry(
    state: &AppState,
    request: &PricingLookupRequest,
) -> anyhow::Result<Option<ModelPricingCatalogEntryRow>> {
    if let Some(workspace_id) = request.workspace_id {
        if let Some(row) = repositories::get_effective_model_pricing_catalog_entry(
            &state.persistence.postgres,
            Some(workspace_id),
            &request.provider_kind,
            &request.model_name,
            &request.capability,
            &request.billing_unit,
            request.at,
        )
        .await
        .context("failed to resolve workspace pricing entry")?
        {
            return Ok(Some(row));
        }
    }

    repositories::get_effective_model_pricing_catalog_entry(
        &state.persistence.postgres,
        None,
        &request.provider_kind,
        &request.model_name,
        &request.capability,
        &request.billing_unit,
        request.at,
    )
    .await
    .context("failed to resolve global pricing entry")
}

fn estimate_usage_cost_with_entry(
    entry: &ModelPricingCatalogEntryRow,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    total_tokens: Option<i32>,
) -> (PricingResolutionStatus, Option<Decimal>) {
    match entry.billing_unit.as_str() {
        "per_1m_input_tokens" => {
            let tokens = prompt_tokens.or(total_tokens);
            let Some(tokens) = tokens else {
                return (PricingResolutionStatus::UsageMissing, None);
            };
            let Some(input_price) = entry.input_price else {
                return (PricingResolutionStatus::PricingMissing, None);
            };
            (PricingResolutionStatus::Priced, Some(scale_token_cost(tokens, input_price)))
        }
        "per_1m_output_tokens" => {
            let Some(tokens) = completion_tokens else {
                return (PricingResolutionStatus::UsageMissing, None);
            };
            let Some(output_price) = entry.output_price else {
                return (PricingResolutionStatus::PricingMissing, None);
            };
            (PricingResolutionStatus::Priced, Some(scale_token_cost(tokens, output_price)))
        }
        "fixed_per_call" => {
            let price = entry.input_price.or(entry.output_price);
            match price {
                Some(value) => (PricingResolutionStatus::Priced, Some(value)),
                None => (PricingResolutionStatus::PricingMissing, None),
            }
        }
        _ => {
            estimate_bidirectional_token_cost(entry, prompt_tokens, completion_tokens, total_tokens)
        }
    }
}

fn estimate_bidirectional_token_cost(
    entry: &ModelPricingCatalogEntryRow,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    total_tokens: Option<i32>,
) -> (PricingResolutionStatus, Option<Decimal>) {
    let mut estimated_cost = Decimal::ZERO;
    let mut saw_usage = false;

    if let Some(tokens) = prompt_tokens.or(total_tokens) {
        if tokens > 0 {
            let Some(input_price) = entry.input_price else {
                return (PricingResolutionStatus::PricingMissing, None);
            };
            estimated_cost += scale_token_cost(tokens, input_price);
            saw_usage = true;
        }
    }

    if let Some(tokens) = completion_tokens {
        if tokens > 0 {
            let Some(output_price) = entry.output_price else {
                return (PricingResolutionStatus::PricingMissing, None);
            };
            estimated_cost += scale_token_cost(tokens, output_price);
            saw_usage = true;
        }
    }

    if !saw_usage {
        return (PricingResolutionStatus::UsageMissing, None);
    }

    (PricingResolutionStatus::Priced, Some(estimated_cost))
}

fn scale_token_cost(tokens: i32, price_per_million: Decimal) -> Decimal {
    if tokens <= 0 {
        return Decimal::ZERO;
    }
    (Decimal::from(tokens) * price_per_million) / Decimal::from(1_000_000u64)
}

fn build_pricing_snapshot(
    entry: Option<&ModelPricingCatalogEntryRow>,
    status: PricingResolutionStatus,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    total_tokens: Option<i32>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    at: DateTime<Utc>,
) -> serde_json::Value {
    json!({
        "pricing_status": pricing_status_label(&status),
        "provider_kind": provider_kind,
        "model_name": model_name,
        "capability": capability,
        "billing_unit": billing_unit,
        "resolved_at": at,
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens,
        },
        "catalog_entry": entry.map(|row| json!({
            "id": row.id,
            "workspace_id": row.workspace_id,
            "provider_kind": row.provider_kind,
            "model_name": row.model_name,
            "capability": row.capability,
            "billing_unit": row.billing_unit,
            "input_price": row.input_price.map(|value| value.normalize().to_string()),
            "output_price": row.output_price.map(|value| value.normalize().to_string()),
            "currency": row.currency,
            "status": row.status,
            "source_kind": row.source_kind,
            "effective_from": row.effective_from,
            "effective_to": row.effective_to,
        })),
    })
}

fn normalize_pricing_entry(entry: &mut UpsertPricingCatalogEntry) {
    entry.provider_kind = entry.provider_kind.trim().to_ascii_lowercase();
    entry.model_name = entry.model_name.trim().to_string();
    entry.capability = entry.capability.trim().to_ascii_lowercase();
    entry.billing_unit = entry.billing_unit.trim().to_ascii_lowercase();
    entry.currency = entry.currency.trim().to_ascii_uppercase();
    entry.source_kind = entry.source_kind.trim().to_ascii_lowercase();
    entry.note =
        entry.note.take().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
}

fn normalize_lookup_request(request: &mut PricingLookupRequest) {
    request.provider_kind = request.provider_kind.trim().to_ascii_lowercase();
    request.model_name = request.model_name.trim().to_string();
    request.capability = request.capability.trim().to_ascii_lowercase();
    request.billing_unit = request.billing_unit.trim().to_ascii_lowercase();
}

fn normalize_usage_request(request: &mut UsageCostLookupRequest) {
    request.provider_kind = request.provider_kind.trim().to_ascii_lowercase();
    request.model_name = request.model_name.trim().to_string();
    request.capability = request.capability.trim().to_ascii_lowercase();
    request.billing_unit = request.billing_unit.trim().to_ascii_lowercase();
}

fn validate_pricing_entry(entry: &UpsertPricingCatalogEntry) -> anyhow::Result<()> {
    if entry.provider_kind.is_empty()
        || entry.model_name.is_empty()
        || entry.capability.is_empty()
        || entry.billing_unit.is_empty()
        || entry.currency.is_empty()
    {
        return Err(anyhow!(
            "provider, model, capability, billing unit, and currency are required"
        ));
    }

    match entry.billing_unit.as_str() {
        "per_1m_input_tokens" if entry.input_price.is_none() => {
            Err(anyhow!("input_price is required for per_1m_input_tokens"))
        }
        "per_1m_output_tokens" if entry.output_price.is_none() => {
            Err(anyhow!("output_price is required for per_1m_output_tokens"))
        }
        "per_1m_tokens" | "fixed_per_call"
            if entry.input_price.is_none() && entry.output_price.is_none() =>
        {
            Err(anyhow!("at least one price is required for the selected billing unit"))
        }
        "per_1m_input_tokens" | "per_1m_output_tokens" | "per_1m_tokens" | "fixed_per_call" => {
            Ok(())
        }
        other => Err(anyhow!("invalid billing_unit: {other}")),
    }
}

#[must_use]
pub fn pricing_status_label(status: &PricingResolutionStatus) -> &'static str {
    match status {
        PricingResolutionStatus::Priced => "priced",
        PricingResolutionStatus::Unpriced => "unpriced",
        PricingResolutionStatus::UsageMissing => "usage_missing",
        PricingResolutionStatus::PricingMissing => "pricing_missing",
    }
}
