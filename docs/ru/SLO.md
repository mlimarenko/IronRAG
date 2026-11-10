# SLO и алерты IronRAG

Владелец: `piping-space/products/ironrag`. Правила алертов живут в
`piping-space/infrastructure/grafana` (`grafana/provisioning/alerting/ironrag-rules.yml`).
Runbook инцидента — IRG-A-16.

## Область

Контракт покрывает API, ingest-worker, очереди, пулы PostgreSQL и исходящие
вызовы провайдеров. Используются метрики из
`apps/api/src/observability/metrics.rs` и scrape `/metrics` обеих ролей.
Стоимость провайдеров — операционный сигнал, а не SLO.

## SLO

| SLO | SLI | Цель | Окно бюджета |
| --- | --- | --- | --- |
| Доступность | доля не-5xx в `axum_http_requests_total` | 99,5% | 30 суток |
| Задержка | p95 `axum_http_requests_duration_seconds` | <= 1 с | 30 суток |
| Успех загрузки | доля `outcome="succeeded"` среди терминальных попыток | 99% | 7 суток |
| Задержка провайдера | p95 `ironrag.provider.http_duration` | <= 30 с | 30 суток |

`ironrag.provider.http_duration` и `ironrag.db.pool.connections` снимаются на
каждый вызов и по каденции обслуживания. Отсутствие серии означает отсутствие
наблюдаемого вызова или сэмпла, а не успех; правило, для которого отсутствие
является отказом, явно объявляет `noDataState: Alerting`.

## Окна

- **5m**: быстрая деградация. Используется алертами задержки, задержки
  провайдера и насыщения пула БД. Интервал scrape 30 с, поэтому в окне не
  меньше десяти сэмплов.
- **24h и 7d**: медленное выгорание. Алерт доступности требует превышения
  порога 0,5% одновременно на 24h и 7d в течение часа: короткий всплеск не
  будит, а устойчивая деградация не прячется за спокойными сутками.
- **30d**: бюджет доступности и задержки. Дашборд показывает текущие значения
  5m/24h/7d; соответствие бюджету за 30 суток проверяется теми же запросами с
  окном 30d.

Новые запросы алертов используют явные селекторы `[5m]`, `[24h]` и `[7d]`,
поэтому каждое окно можно воспроизвести против VictoriaMetrics без изменений;
существующие правила сохраняют свои окна. Не расширять окно и не поднимать
порог, чтобы погасить сработавшее правило: записывайте инцидент и исправляйте
сигнал или сервис.

## Соответствие алертов

| UID | Сигнал | Владелец | Runbook |
| --- | --- | --- | --- |
| `ironrag_scrape_targets_down` | цель scrape отсутствует | ironrag | IRG-A-16 |
| `ironrag_http_5xx` | любые 5xx в течение 10m | ironrag | IRG-A-16 |
| `ironrag_slo_availability_burn` | выгорание бюджета на 24h и 7d | ironrag | IRG-A-16 |
| `ironrag_slo_latency_p95` | HTTP p95 выше 1 с | ironrag | IRG-A-16 |
| `ironrag_ingest_failure_slo` | терминальные неудачи загрузки выше 1% | ironrag | IRG-A-16 |
| `ironrag_db_pool_saturation` | active/max пула выше 90% | ironrag | IRG-A-16 |
| `ironrag_provider_http_latency_p95` | HTTP p95 провайдера выше 30 с | ironrag | IRG-A-16 |

Алерты очереди, восстановления аренд, 429, аномалии стоимости и жизненного
цикла векторов сохраняют существующие UID и пороги. Новое правило не
переиспользует UID и не повторяет существующий сигнал с более слабым порогом.

## Проверка

```promql
# Доступность на каждом окне
1 - (sum(rate(axum_http_requests_total{job="ironrag",status=~"5.."}[5m])) or vector(0))
    / clamp_min(sum(rate(axum_http_requests_total{job="ironrag"}[5m])), 1e-9)

# p95 задержки
histogram_quantile(0.95, sum by (le) (rate(axum_http_requests_duration_seconds_bucket{job="ironrag"}[5m])))

# Насыщение пула PostgreSQL
max by (pool) ({__name__="ironrag.db.pool.connections", state="active"})
  / clamp_min(max by (pool) ({__name__="ironrag.db.pool.connections", state="max"}), 1)
```

Выполняйте provisioned-выражения против VictoriaMetrics на 5m, 24h и 7d.
Здоровый сервис возвращает значение или пустую серию, но не ошибку исполнения.

## Остаточные пробелы

- Нет SLO на библиотеку или тенанта: идентификаторы неограничены и не
  попадают в метки.
- `ironrag.provider.http_duration` измеряет время до заголовков ответа одной
  попытки, а не поток тела или генерацию токенов.
- Счётчики пула PostgreSQL публикует только worker; API-only развёртывание их
  пока не отдаёт.
