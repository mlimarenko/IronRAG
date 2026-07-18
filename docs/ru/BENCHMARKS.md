# Бенчмарки IronRAG

Grounded-query датасеты лежат в `apps/api/benchmarks/grounded_query/`. Markdown внутри corpus — это тестовые данные, а не операторская документация; команды и контракт оценки зафиксированы здесь.

## Структура corpus

```text
apps/api/benchmarks/grounded_query/
├── corpus/
│   ├── wikipedia/   статьи общего знания
│   ├── docs/        технические документы и contract fixtures
│   ├── code/        код и config-файлы
│   ├── documents/   PDF, DOCX, PPTX fixtures
│   ├── graph/       multi-hop graph topology fixtures
│   └── fixtures/    upload-path smoke fixtures
├── *.json           определения suite'ов
├── rank_relevance.json
├── run_live_benchmark.py
└── compare_benchmarks.py
```

## Наборы

| Suite | Назначение |
|---|---|
| `api_baseline_suite` | single-document retrieval quality |
| `workflow_strict_suite` | multi-document grounded QA |
| `layout_noise_suite` | устойчивость extraction на шумном layout |
| `graph_multihop_suite` | качество graph-backed traversal |
| `multiformat_surface_suite` | multi-format upload и extraction |
| `technical_contract_suite` | exact technical literals: endpoint'ы, параметры, отсутствующие capability, transport comparison |
| `golden_*_suite` | более широкое покрытие programming, infrastructure, protocol, code и multi-format сценариев |

`technical_contract_suite` — quality gate для exact-literal вопросов. Его нужно гонять при любых изменениях retrieval, grounding, MCP search/read behavior или answer assembly.

## Запуск

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="..."
export IRONRAG_BENCHMARK_RUNTIME_ARTIFACT_DIGEST="sha256:<64 lowercase hex>"

make benchmark-grounded-seed
make benchmark-grounded-all
make benchmark-grounded-technical
make benchmark-golden
```

Session cookie принимается только через `IRONRAG_SESSION_COOKIE` или заранее
созданный файл, указанный в `IRONRAG_SESSION_COOKIE_FILE`. Аргумент командной
строки для cookie намеренно отсутствует, чтобы секрет не попадал в process list.

`make benchmark-grounded` использует матрицу `IRONRAG_BENCHMARK_SUITES` и пишет
результаты в `tmp-grounded-benchmarks/`. `make benchmark-golden` переключается
на расширенную матрицу `golden_*_suite` и пишет в `tmp-golden-benchmarks/`.

Для release candidates публичные suite'ы нужно дополнять private live smoke на
репрезентативных operator data. Private prompts, document labels и expected
strings не попадают в git; публикуется только sanitized aggregate evidence:
HTTP status, lifecycle state, verifier state, answer length, source count,
matched structural markers и отсутствие запрещённых generic markers.
Для setup/procedure изменений smoke должен покрывать минимум:

- broad multi-variant setup request, который должен отвечать, а не уточнять;
- focused setup request, который должен остаться на focused evidence path;
- versioned procedure request, который должен достать transition
  procedure, а не adjacent transition или compatibility page;
- application-update procedure request, который должен показать ordered
  grounded sequence.

## Прямые скрипты

```bash
python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py --help
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py baseline-dir candidate-dir
```

`compare_benchmarks.py` принимает две директории результатов и работает как
fail-closed regression gate. Он отклоняет регрессию ранее проходившего кейса,
исчезновение paired case/latency sample, снижение размеченных MRR/hit metrics и
рост любого из p50/p95/p99 больше чем на 10% по умолчанию. Кроме того, все
strict-кейсы candidate обязаны пройти, а абсолютные границы grounded answer
равны p50 <= 12 s и p95 <= 30 s. Пороги задаются через
`--max-latency-regression-percent`, `--max-candidate-p50-ms` и
`--max-candidate-p95-ms`; `--json-output` сохраняет machine-readable решение.

Latency percentiles считаются только по одинаковым парам `suiteId/caseId` в
baseline и candidate. Добавленные кейсы не могут заменить пропавший baseline
sample или изменить regression percentiles. p50/p95/p99 считаются консервативным
nearest-rank методом без интерполяции к меньшему sample.

Каждый результат также содержит SHA-256 fingerprints полного определения
кейса, suite, упорядоченных байтов corpus, прочитанных обратно из работающего
сервиса, и runtime-параметров матрицы (`queryTopK`, cache policy, round id,
изолированной session policy и режима переиспользования corpus). Сравнение fail-closed при
отсутствии или несовпадении любого fingerprint. Поэтому изменение вопроса,
expected literal, relevance label, порога, байта fixture, порядка suite'ов или
top-k нельзя ошибочно принять за улучшение продукта. Baseline, созданный до
этого integrity-контракта, нужно переснять.

Эквивалентная Make-команда:

```bash
make benchmark-regression \
  IRONRAG_BENCHMARK_BASELINE_DIR=results/baseline \
  IRONRAG_BENCHMARK_CANDIDATE_DIR=results/candidate
```

Runner создаёт отдельную query session для каждого независимого кейса и сначала
замеряет answer, а уже потом выполняет вспомогательный rank-search. Поэтому
история предыдущих ответов и прогрев rank-кэша не искажают latency. Reused
library принимается только при точном совпадении primary-document inventory и
исходных байтов с локальными fixtures.

Baseline и candidate должны измеряться минимум в трёх чередующихся paired
rounds. Задавай `IRONRAG_BENCHMARK_RUNTIME_LABEL`, обязательный immutable
`IRONRAG_BENCHMARK_RUNTIME_ARTIFACT_DIGEST`, одну из явных cache policy `cold`,
`warm` или `mixed`, а также `IRONRAG_BENCHMARK_ROUND_ID`. SHA-256 permutation,
зависящая от round, меняет порядок кейсов между rounds, но сохраняет одинаковый
порядок в паре baseline/candidate. Comparator требует непустые разные artifact
digest и одинаковые cache policy, round и fingerprint окружения.

Release eligibility пересчитывается из raw pre/mid/post snapshots, а не
доверяет сохранённому boolean. Каждый snapshot обязан пройти gates по
load-per-CPU, доступной памяти, занятому swap и CPU/memory/I/O PSI; ослаблять
дефолтную host policy нельзя. Busy-host override или отсутствующая/несовместимая
идентичность hardware, kernel, boot, cgroup, CPU или памяти делает замер только
диагностическим.

Latency policy разделена явно: grounded answer — p50 <= 12 s и p95 <= 30 s;
полный agent turn имеет hard p95 <= 90 s; rollout canary намеренно использует
более строгий target 25 s. Relative regression gate обязателен даже при
прохождении абсолютного порога.

Legacy-команда `scripts/bench/compare_pg_vs_baseline.py` делегирует grounded
решение тому же canonical comparator. Для combined release verdict она также
требует валидные baseline/candidate `agent_turn_p95.result.json`, прохождение
candidate agent quality gate, agent p95 <= 90 s и не более 10% relative
регрессии agent p95.

## Контракт результатов

По умолчанию результаты пишутся в `tmp-grounded-benchmarks/` и включают:

- per-case pass/fail details,
- integrity fingerprints для case, suite, corpus и matrix,
- runtime version/label и обязательный immutable artifact digest,
- constrained cache policy, paired round id, deterministic case order и
  совместимую environment identity,
- независимо проверенные pre/mid/post snapshots load, memory, swap и PSI,
- `failedChecks` для каждого нарушенного ожидания,
- suite-level `failureReasonCounts`,
- suite-level и matrix-level `summary.rankMetrics`,
- suite-level и matrix-level `summary.answerLatencyMs` с sample count и
  p50/p95/p99,
- append-only `rank_metrics_trend.jsonl` в output-директории,
- latency и evidence metadata для каждого кейса.

Цель — не только pass/fail. Вывод должен показывать, упало ли качество на retrieval, answer assembly, evidence selection или verification.

## Retrieval rank metrics

Для кейсов с известной релевантностью live-runner фиксирует ordered retrieval
quality отдельно от корректности ответа:

- document rank metrics берут ожидаемые документы из suite'ов, если
  `rank_relevance.json` не задаёт явные `relevantDocuments`;
- chunk rank metrics используют опциональные marker-строки `relevantChunks`,
  которые ищутся в тексте retrieved chunks;
- каждая metric family публикует `hit@1`, `hit@3`, `hit@5`, `hit@10`, `MRR` и
  `caseCount`.

Relevance data остаётся только синтетической fixture data. Private или
operator-specific corpora не попадают в эту репу; публикуйте только sanitized
aggregate evidence.

`searchQuery` — это точный вход в question-agnostic search endpoint. Если
ожидание по document-rank зависит от публичного предметного уточнения из
benchmark question, сохраняйте это уточнение в `searchQuery`, а не ожидайте,
что search выведет его сам.

## Large-document ingest smoke

Крупные private ingest corpora не хранятся в публичной репе. При изменениях в
Docling, chunking, embedding, graph extraction или worker leases прогоняй
private large-document smoke и публикуй только sanitized evidence:

- все файлы дошли до `ready`;
- resumed jobs переиспользовали завершённые PDF page-range units;
- graph topology после finalization не пустой;
- encoding scanners не нашли mojibake в persisted graph labels или page units;
- document UI показал stage progress, model, duration, calls и cost;
- публичный `make check` прошёл.
