WITH recent_attempts AS (
  SELECT id
  FROM ingest_attempt
  WHERE started_at >= now() - interval '60 minutes'
  ORDER BY started_at DESC
  LIMIT 200
),
calls AS (
  SELECT owning_execution_id AS attempt_id, count(*)::int AS provider_call_count
  FROM billing_provider_call
  WHERE owning_execution_kind = 'ingest_attempt'
    AND owning_execution_id IN (SELECT id FROM recent_attempts)
  GROUP BY owning_execution_id
),
costs AS (
  SELECT owning_execution_id AS attempt_id, total_cost, provider_call_count
  FROM billing_execution_cost
  WHERE owning_execution_kind = 'ingest_attempt'
    AND owning_execution_id IN (SELECT id FROM recent_attempts)
)
SELECT
  a.id AS attempt_id,
  coalesce(c.provider_call_count, 0) AS provider_calls_seen,
  costs.total_cost,
  costs.provider_call_count AS rollup_provider_calls,
  CASE
    WHEN coalesce(c.provider_call_count, 0) > 0 AND costs.attempt_id IS NULL THEN 'missing_cost_rollup'
    WHEN costs.attempt_id IS NOT NULL
         AND costs.provider_call_count <> coalesce(c.provider_call_count, 0) THEN 'rollup_count_mismatch'
    ELSE 'ok'
  END AS consistency
FROM recent_attempts a
LEFT JOIN calls c ON c.attempt_id = a.id
LEFT JOIN costs ON costs.attempt_id = a.id
ORDER BY a.id DESC;
