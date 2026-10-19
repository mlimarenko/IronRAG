use super::*;

pub async fn create_usage<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &NewBillingUsage<'_>,
) -> Result<BillingUsageRow, sqlx::Error> {
    sqlx::query_as::<_, BillingUsageRow>(
        "insert into billing_usage (
            id,
            provider_call_id,
            usage_kind,
            billing_unit,
            quantity,
            observed_at
        )
        values ($1, $2, $3, $4::billing_unit, $5, coalesce($6, now()))
        returning
            id,
            provider_call_id,
            usage_kind,
            billing_unit::text as billing_unit,
            quantity,
            observed_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.provider_call_id)
    .bind(input.usage_kind)
    .bind(input.billing_unit)
    .bind(input.quantity)
    .bind(input.observed_at)
    .fetch_one(executor)
    .await
}

pub async fn list_usage_by_provider_call(
    postgres: &PgPool,
    provider_call_id: Uuid,
) -> Result<Vec<BillingUsageRow>, sqlx::Error> {
    sqlx::query_as::<_, BillingUsageRow>(
        "select
            id,
            provider_call_id,
            usage_kind,
            billing_unit::text as billing_unit,
            quantity,
            observed_at
         from billing_usage
         where provider_call_id = $1
         order by observed_at asc, id asc",
    )
    .bind(provider_call_id)
    .fetch_all(postgres)
    .await
}

pub async fn create_charge<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    input: &NewBillingCharge,
) -> Result<BillingChargeRow, sqlx::Error> {
    sqlx::query_as::<_, BillingChargeRow>(concat!(
        "insert into billing_charge (
            id,
            usage_id,
            currency_code,
            unit_price,
            total_price,
            priced_at
        )
        values ($1, $2, $3, $4, $5, coalesce($6, now()))
        returning
            ",
        billing_charge_projection!(""),
        "
"
    ))
    .bind(Uuid::now_v7())
    .bind(input.usage_id)
    .bind(&input.currency_code)
    .bind(input.unit_price)
    .bind(input.total_price)
    .bind(input.priced_at)
    .fetch_one(executor)
    .await
}

/// Keyset page over one execution's billing charges, ordered newest-first
/// (`priced_at desc, id desc`).
/// `cursor` is the `(priced_at, id)` of the last row of the previous page —
/// `None` starts from the top. Fetches `limit + 1` rows so the caller can
/// derive `has_more` without a second round trip; callers must trim the
/// extra row before returning it to clients.
pub async fn list_charges_by_execution_page(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<BillingChargeRow>, sqlx::Error> {
    let (cursor_priced_at, cursor_id) = cursor.unzip();
    sqlx::query_as::<_, BillingChargeRow>(concat!(
        "select
            ",
        billing_charge_projection!("bc."),
        "
         from billing_charge bc
         join billing_usage bu on bu.id = bc.usage_id
         join billing_provider_call bpc on bpc.id = bu.provider_call_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = $2
           and (
             $3::timestamptz is null
             or (bc.priced_at, bc.id) < ($3::timestamptz, $4::uuid)
           )
         order by bc.priced_at desc, bc.id desc
         limit $5"
    ))
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .bind(cursor_priced_at)
    .bind(cursor_id)
    .bind(limit)
    .fetch_all(postgres)
    .await
}

/// Total number of billing charges attributed to one execution, for the
/// optional `total` field on the paginated charges envelope.
pub async fn count_charges_by_execution<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from billing_charge bc
         join billing_usage bu on bu.id = bc.usage_id
         join billing_provider_call bpc on bpc.id = bu.provider_call_id
         where bpc.owning_execution_kind = $1::billing_owning_execution_kind
           and bpc.owning_execution_id = $2",
    )
    .bind(owning_execution_kind)
    .bind(owning_execution_id)
    .fetch_one(executor)
    .await
}
