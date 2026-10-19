use super::*;

impl BillingService {
    /// Lists a keyset page of provider-call rows for a single execution,
    /// newest-first. `cursor` is the `(started_at, id)` of the last row of
    /// the previous page; `None` starts from the top.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the repository query fails or a persisted provider call cannot
    /// be mapped back into the domain type.
    pub async fn list_execution_provider_calls_page(
        &self,
        state: &AppState,
        query: BillingExecutionPageQuery,
    ) -> Result<BillingSubResourcePage<BillingProviderCall>, ApiError> {
        let execution_kind_key = execution_owner_kind_key(query.execution_kind);
        let mut rows = billing_repository::list_provider_calls_by_execution_page(
            &state.persistence.postgres,
            execution_kind_key,
            query.execution_id,
            query.cursor,
            query.limit + 1,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let has_more = rows.len() as i64 > query.limit;
        rows.truncate(usize::try_from(query.limit).unwrap_or(usize::MAX));
        let total = if query.include_total {
            Some(
                billing_repository::count_provider_calls_by_execution(
                    &state.persistence.postgres,
                    execution_kind_key,
                    query.execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
            )
        } else {
            None
        };
        let items = rows
            .into_iter()
            .map(map_provider_call_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(ApiError::BadRequest)?;
        Ok(BillingSubResourcePage { items, has_more, total })
    }

    /// Lists a keyset page of billing charges for a single execution,
    /// newest-first. `cursor` is the `(priced_at, id)` of the last row of
    /// the previous page; `None` starts from the top.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the repository query fails.
    pub async fn list_execution_charges_page(
        &self,
        state: &AppState,
        query: BillingExecutionPageQuery,
    ) -> Result<BillingSubResourcePage<BillingCharge>, ApiError> {
        let execution_kind_key = execution_owner_kind_key(query.execution_kind);
        let mut rows = billing_repository::list_charges_by_execution_page(
            &state.persistence.postgres,
            execution_kind_key,
            query.execution_id,
            query.cursor,
            query.limit + 1,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let has_more = rows.len() as i64 > query.limit;
        rows.truncate(usize::try_from(query.limit).unwrap_or(usize::MAX));
        let total = if query.include_total {
            Some(
                billing_repository::count_charges_by_execution(
                    &state.persistence.postgres,
                    execution_kind_key,
                    query.execution_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
            )
        } else {
            None
        };
        Ok(BillingSubResourcePage {
            items: rows.into_iter().map(map_charge_row).collect(),
            has_more,
            total,
        })
    }

    /// Loads the rolled-up billing cost for a single execution.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the execution kind is invalid, the repository query fails, or a
    /// persisted billing row cannot be mapped back into the domain type.
    pub async fn get_execution_cost(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<BillingExecutionCost, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(execution_kind))?;
        let execution_kind_key = execution_owner_kind_key(execution_kind);
        let snapshot = billing_repository::get_execution_cost_read_snapshot(
            &state.persistence.postgres,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_no_terminal_billing_rollup(
            "execution",
            execution_id,
            snapshot.terminal_error_code.as_deref(),
        )?;
        let expected_aggregate_count =
            i32::try_from(snapshot.grounded_answer_provider_call_count).unwrap_or(i32::MAX);
        let aggregate_missing_or_mismatched = snapshot.execution_cost.as_ref().map_or_else(
            || snapshot.grounded_answer_provider_call_count > 0,
            |cost| {
                snapshot.grounded_answer_provider_call_count == 0
                    || cost.provider_call_count != expected_aggregate_count
            },
        );
        let untracked_pipeline_cost = !snapshot.rollup_state_present
            && (snapshot.grounded_answer_provider_call_count > 0
                || snapshot.execution_cost.is_some());
        if snapshot.rollup_dirty || untracked_pipeline_cost || aggregate_missing_or_mismatched {
            // Dirty generations, missing durable cursors, and defensive count
            // mismatches repair under the same advisory lock used by writers.
            // Clean, matching rows no longer pay for an unconditional
            // aggregate rebuild on every GET.
            return self
                .roll_up_execution_cost_with_retry(state, execution_kind_key, execution_id)
                .await?
                .ok_or_else(|| {
                    ApiError::resource_not_found("billing_execution_cost", execution_id)
                });
        }

        if let Some(row) = snapshot.execution_cost {
            return map_execution_cost_row(row).map_err(ApiError::BadRequest);
        }

        // An execution with no rollup row captured no billable provider call
        // at all, so zero cost is the complete truth here. Executions whose
        // calls exist but carry no catalog price are a different case: they
        // do have a rollup row, and it reports its unpriced calls.
        Ok(BillingExecutionCost {
            id: Uuid::now_v7(),
            owning_execution_kind: execution_kind,
            owning_execution_id: execution_id,
            total_cost: Decimal::ZERO,
            currency_code: "USD".to_string(),
            provider_call_count: 0,
            unpriced_provider_call_count: 0,
            cost_coverage: CostCoverageStatus::Complete,
            updated_at: Utc::now(),
        })
    }

    /// Lists document-level cost summaries for a library, with the freshness
    /// of the owning library rollup.
    ///
    /// While the rollup is reconciling the list is withheld (`items` empty,
    /// `cost_rollup_freshness = Reconciling`) — a per-document total cannot be
    /// exact mid-reconciliation, and resolving the page to empty would fake a
    /// costless library. Terminal mixed-currency state still fails closed.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails, and the
    /// terminal mixed-currency error when the library has no single currency.
    pub async fn list_document_costs_for_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<DocumentCostList, ApiError> {
        let snapshot = billing_repository::get_document_costs_read_snapshot(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_no_terminal_billing_rollup(
            "library",
            library_id,
            snapshot.terminal_error_code.as_deref(),
        )?;
        let freshness = CostRollupFreshness::from_rollup_dirty(snapshot.rollup_dirty);
        if freshness == CostRollupFreshness::Reconciling {
            return Ok(DocumentCostList { cost_rollup_freshness: freshness, items: Vec::new() });
        }
        let mut seen_documents = HashSet::with_capacity(snapshot.rows.len());
        let items: Vec<DocumentCostSummary> = snapshot
            .rows
            .into_iter()
            .map(|r| {
                if !seen_documents.insert(r.document_id) {
                    return Err(mixed_currency_billing_scope("document", r.document_id));
                }
                Ok(DocumentCostSummary {
                    document_id: r.document_id,
                    total_cost: r.total_cost,
                    currency_code: r.currency_code,
                    provider_call_count: r.provider_call_count,
                    unpriced_provider_call_count: r.unpriced_provider_call_count,
                    cost_coverage: CostCoverageStatus::from_call_counts(
                        r.provider_call_count,
                        r.unpriced_provider_call_count,
                    ),
                })
            })
            .collect::<Result<Vec<DocumentCostSummary>, ApiError>>()?;
        Ok(DocumentCostList { cost_rollup_freshness: freshness, items })
    }

    /// Lists an offset page of document-level cost summaries for a library.
    ///
    /// The underlying repository read is one atomic rollup-health snapshot
    /// (see [`list_document_costs_for_library`](Self::list_document_costs_for_library)); pagination
    /// is applied in-memory over that already-materialized, already-ordered
    /// result rather than pushed into the snapshot query, so the
    /// rollup-dirty/mixed-currency guarantees it provides are untouched. A
    /// reconciling library short-circuits to an empty page with no cursor and
    /// no count.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn list_document_costs_for_library_page(
        &self,
        state: &AppState,
        library_id: Uuid,
        offset: usize,
        limit: usize,
        include_total: bool,
    ) -> Result<DocumentCostListPage, ApiError> {
        let list = self.list_document_costs_for_library(state, library_id).await?;
        if list.cost_rollup_freshness == CostRollupFreshness::Reconciling {
            return Ok(DocumentCostListPage {
                cost_rollup_freshness: list.cost_rollup_freshness,
                items: Vec::new(),
                has_more: false,
                total: None,
            });
        }
        let total = include_total.then_some(list.items.len() as i64);
        let has_more = offset.saturating_add(limit) < list.items.len();
        let items = list.items.into_iter().skip(offset).take(limit).collect();
        Ok(DocumentCostListPage {
            cost_rollup_freshness: list.cost_rollup_freshness,
            items,
            has_more,
            total,
        })
    }

    /// Loads the rolled-up cost summary for a library.
    ///
    /// A terminal mixed-currency rollup still fails closed (409); a merely
    /// reconciling rollup returns `costRollupFreshness: "reconciling"` with
    /// every value field `null` instead of a retryable 503, so a clean library
    /// keeps returning exact totals while a sibling library reconciles.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn get_library_cost_summary(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<LibraryCostSummary, ApiError> {
        let snapshot = billing_repository::get_library_cost_read_snapshot(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_no_terminal_billing_rollup(
            "library",
            library_id,
            snapshot.terminal_error_code.as_deref(),
        )?;
        if snapshot.rollup_dirty {
            return Ok(reconciling_library_summary());
        }
        match snapshot.rows.as_slice() {
            [r] => Ok(LibraryCostSummary {
                cost_rollup_freshness: CostRollupFreshness::Current,
                total_cost: Some(r.total_cost),
                currency_code: Some(r.currency_code.clone()),
                document_count: Some(r.document_count),
                provider_call_count: Some(r.provider_call_count),
                unpriced_provider_call_count: Some(r.unpriced_provider_call_count),
                cost_coverage: Some(CostCoverageStatus::from_call_counts(
                    r.provider_call_count,
                    r.unpriced_provider_call_count,
                )),
            }),
            [] => Ok(LibraryCostSummary {
                cost_rollup_freshness: CostRollupFreshness::Current,
                total_cost: Some(Decimal::ZERO),
                currency_code: Some("USD".to_string()),
                document_count: Some(0),
                provider_call_count: Some(0),
                unpriced_provider_call_count: Some(0),
                cost_coverage: Some(CostCoverageStatus::Complete),
            }),
            _ => Err(mixed_currency_billing_scope("library", library_id)),
        }
    }

    /// Loads the rolled-up cost summary for a workspace.
    ///
    /// A workspace aggregate spans every library, so any single dirty library
    /// makes it `reconciling`; other libraries are unaffected.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn get_workspace_cost_summary(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<WorkspaceCostSummary, ApiError> {
        let snapshot = billing_repository::get_workspace_cost_read_snapshot(
            &state.persistence.postgres,
            workspace_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_no_terminal_billing_rollup(
            "workspace",
            workspace_id,
            snapshot.terminal_error_code.as_deref(),
        )?;
        if snapshot.rollup_dirty {
            return Ok(reconciling_workspace_summary());
        }
        match snapshot.rows.as_slice() {
            [r] => Ok(WorkspaceCostSummary {
                cost_rollup_freshness: CostRollupFreshness::Current,
                total_cost: Some(r.total_cost),
                currency_code: Some(r.currency_code.clone()),
                library_count: Some(r.library_count),
                document_count: Some(r.document_count),
                provider_call_count: Some(r.provider_call_count),
                unpriced_provider_call_count: Some(r.unpriced_provider_call_count),
                cost_coverage: Some(CostCoverageStatus::from_call_counts(
                    r.provider_call_count,
                    r.unpriced_provider_call_count,
                )),
            }),
            [] => Ok(WorkspaceCostSummary {
                cost_rollup_freshness: CostRollupFreshness::Current,
                total_cost: Some(Decimal::ZERO),
                currency_code: Some("USD".to_string()),
                library_count: Some(0),
                document_count: Some(0),
                provider_call_count: Some(0),
                unpriced_provider_call_count: Some(0),
                cost_coverage: Some(CostCoverageStatus::Complete),
            }),
            _ => Err(mixed_currency_billing_scope("workspace", workspace_id)),
        }
    }

    /// Resolves the library that owns a billing execution.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the execution kind is invalid or the execution scope cannot be
    /// resolved.
    pub async fn resolve_execution_library_id(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let scope = self
            .resolve_execution_scope(
                state,
                parse_execution_owner_kind(execution_kind)
                    .ok_or_else(|| invalid_execution_owner_kind(execution_kind))?,
                execution_id,
            )
            .await?;
        Ok(scope.library_id)
    }
}
