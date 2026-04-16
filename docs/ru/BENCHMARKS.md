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
│   └── fixtures/    upload-path smoke fixtures
├── *.json           определения suite'ов
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

## Прямые скрипты

```bash
python3 apps/api/benchmarks/grounded_query/run_live_benchmark.py --help
python3 apps/api/benchmarks/grounded_query/compare_benchmarks.py old.json new.json
```

## Контракт результатов

По умолчанию результаты пишутся в `tmp-grounded-benchmarks/` и включают:

- per-case pass/fail details,
- `failedChecks` для каждого нарушенного ожидания,
- suite-level `failureReasonCounts`,
- latency и evidence metadata для каждого кейса.

Цель — не только pass/fail. Вывод должен показывать, упало ли качество на retrieval, answer assembly, evidence selection или verification.
