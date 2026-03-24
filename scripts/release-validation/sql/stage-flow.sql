WITH recent AS (
  SELECT id
  FROM ingest_attempt
  WHERE started_at >= now() - interval '60 minutes'
  ORDER BY started_at DESC
  LIMIT 200
)
SELECT
  se.attempt_id,
  string_agg(se.stage_name || ':' || se.stage_state, ' -> ' ORDER BY se.recorded_at) AS stage_flow
FROM ingest_stage_event se
JOIN recent r ON r.id = se.attempt_id
GROUP BY se.attempt_id
ORDER BY max(se.recorded_at) DESC;
