WITH recent AS (
  SELECT id
  FROM ingest_attempt
  WHERE started_at >= now() - interval '60 minutes'
  ORDER BY started_at DESC
  LIMIT 200
),
paired AS (
  SELECT
    s.attempt_id,
    s.stage_name,
    s.recorded_at AS started_at,
    c.recorded_at AS completed_at,
    extract(epoch FROM (c.recorded_at - s.recorded_at)) * 1000 AS duration_ms
  FROM ingest_stage_event s
  JOIN ingest_stage_event c
    ON c.attempt_id = s.attempt_id
   AND c.stage_name = s.stage_name
   AND c.stage_state = 'completed'
   AND s.stage_state = 'started'
   AND c.recorded_at >= s.recorded_at
  WHERE s.attempt_id IN (SELECT id FROM recent)
)
SELECT
  stage_name,
  round(avg(duration_ms)::numeric, 1) AS avg_ms,
  round(percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms)::numeric, 1) AS p95_ms,
  round(max(duration_ms)::numeric, 1) AS max_ms,
  count(*) AS samples
FROM paired
GROUP BY stage_name
ORDER BY stage_name;
