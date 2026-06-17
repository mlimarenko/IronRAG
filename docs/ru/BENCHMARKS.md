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

make benchmark-grounded-seed
make benchmark-grounded-all
make benchmark-grounded-technical
make benchmark-golden
```

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
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py old.json new.json
```

`compare_benchmarks.py` принимает две директории результатов и показывает
движение pass/fail, дельты topology графа и дельты retrieval rank metrics.

## Контракт результатов

По умолчанию результаты пишутся в `tmp-grounded-benchmarks/` и включают:

- per-case pass/fail details,
- `failedChecks` для каждого нарушенного ожидания,
- suite-level `failureReasonCounts`,
- suite-level и matrix-level `summary.rankMetrics`,
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
