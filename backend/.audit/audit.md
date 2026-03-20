# Codebase Audit Report: /home/leader/sources/RustRAG/rustrag/backend

## Executive summary
- Files scanned: 172
- Total lines: 78568
- Marker lines (TODO/FIXME/HACK/XXX): 1
- Findings (heuristic): high=16, medium=11, total=30

## Smell summary
- By severity: high=16, medium=11, low=3
- By primary smell: Large Class=14, Long Parameter List=7, Message Chains=4, Risky Constructs=2, Oversized File=1, Primitive Obsession=1, Duplicate Code=1

## Totals
- Files scanned: 172
- Total lines: 78568
- Code lines: 72638
- Comment lines: 1067
- Detected markers (TODO/FIXME/HACK/XXX): 1

## Top languages
- .rs: 155
- .sql: 13
- .json: 2
- .md: 1
- .yaml: 1

## Hierarchy
- max depth: 4
- avg depth: 2.96
- median depth: 3.0
- flat ratio (depth <= 2): 0.2791

## Top risks
- contracts/rustrag.openapi.yaml (high): long file (10056 lines > 1200)
- src/interfaces/http/graph.rs (high): long file (1385 lines > 1200)
- src/interfaces/http/mcp_memory.rs (high): long file (1280 lines > 1200)
- src/services/mcp_memory.rs (high): long file (1543 lines > 1200)
- src/shared/file_extract.rs (high): long file (1209 lines > 1200)
- src/infra/ui_queries.rs (high): long file (2247 lines > 1200); long signatures (1 functions > 8 params)
- src/integrations/provider_catalog.rs (high): long file (1575 lines > 1200); many magic numbers (181)
- src/interfaces/http/retrieval.rs (high): long file (1261 lines > 1200); many message chains (33)
- src/interfaces/http/runtime_documents.rs (high): long file (2417 lines > 1200); long signatures (1 functions > 8 params)
- src/interfaces/http/runtime_graph.rs (high): long file (1608 lines > 1200); high branching + nesting (branches=88, depth=6)
- src/services/query_runtime.rs (high): long file (2218 lines > 1200); long signatures (1 functions > 8 params); many message chains (21)
- src/infra/repositories.rs (high): long file (11146 lines > 1200); long signatures (15 functions > 8 params); many magic numbers (152); many switch/case hits (39)
- src/services/ingestion_worker.rs (high): long file (2860 lines > 1200); long signatures (3 functions > 8 params); high branching + nesting (branches=148, depth=8); many unwrap/expect (20)
- src/services/graph_extract.rs (high): long file (2223 lines > 1200); long signatures (1 functions > 8 params); many magic numbers (29); many message chains (17); many unwrap/expect (12)
- src/services/runtime_ingestion.rs (high): long file (2861 lines > 1200); long signatures (2 functions > 8 params); many magic numbers (31); many message chains (33); high branching + nesting (branches=87, depth=8)
- src/app/config.rs (medium): many magic numbers (66)
- src/interfaces/http/router_support.rs (medium): panic! usage (1)
- src/interfaces/http/runtime_providers.rs (medium): panic! usage (2)
- src/interfaces/http/ui_graph.rs (medium): long signatures (1 functions > 8 params)
- src/services/collection_settlement.rs (medium): long signatures (1 functions > 8 params)

## Smell-based findings (actionable)
- [high] Large Class | secondary: Long Method, Long Parameter List, Primitive Obsession, Switch Statements
  - paths: src/services/ingestion_worker.rs
  - evidence: long file: 2860 lines (threshold 1200); long signatures: 3 functions > 8 params; flag-like params detected: 13 (heuristic); magic numbers: 18 samples=[416, 511, 570, 621, 1009, 1107]; branching+nested: branches=148, indent-depth=8 samples=[99, 103, 107, 137, 139, 164]; raw issues: long file (2860 lines > 1200), long signatures (3 functions > 8 params), high branching + nesting (branches=148, depth=8), many unwrap/expect (20)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Long Method, Long Parameter List, Message Chains, Primitive Obsession
  - paths: src/services/runtime_ingestion.rs
  - evidence: long file: 2861 lines (threshold 1200); long signatures: 2 functions > 8 params; magic numbers: 31 samples=[2266, 2596, 2599, 2603, 2605, 2614]; message chains: 33 lines with 3+ chained accesses samples=[453, 455, 457, 459, 773, 774]; branching+nested: branches=87, indent-depth=8 samples=[163, 169, 175, 191, 194, 249]; raw issues: long file (2861 lines > 1200), long signatures (2 functions > 8 params), many magic numbers (31), many message chains (33), high branching + nesting (branches=87, depth=8)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Long Parameter List, Primitive Obsession, Switch Statements
  - paths: src/infra/repositories.rs
  - evidence: long file: 11146 lines (threshold 1200); long signatures: 15 functions > 8 params; magic numbers: 152 samples=[1197, 1856, 1894, 2510, 2573, 2731]; switch/case hits: 39 samples=[1342, 1453, 3045, 3054, 3060, 3070]; raw issues: long file (11146 lines > 1200), long signatures (15 functions > 8 params), many magic numbers (152), many switch/case hits (39)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Long Parameter List, Message Chains, Primitive Obsession
  - paths: src/services/graph_extract.rs
  - evidence: long file: 2223 lines (threshold 1200); long signatures: 1 functions > 8 params; magic numbers: 29 samples=[301, 304, 380, 1976, 1978, 1988]; message chains: 17 lines with 3+ chained accesses samples=[409, 647, 662, 663, 870, 880]; raw issues: long file (2223 lines > 1200), long signatures (1 functions > 8 params), many magic numbers (29), many message chains (17), many unwrap/expect (12)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Long Parameter List, Primitive Obsession
  - paths: src/interfaces/http/runtime_documents.rs
  - evidence: long file: 2417 lines (threshold 1200); long signatures: 1 functions > 8 params; magic numbers: 18 samples=[981, 2048, 2049, 2050, 2051, 2052]; raw issues: long file (2417 lines > 1200), long signatures (1 functions > 8 params)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Long Parameter List, Message Chains
  - paths: src/services/query_runtime.rs
  - evidence: long file: 2218 lines (threshold 1200); long signatures: 1 functions > 8 params; message chains: 21 lines with 3+ chained accesses samples=[270, 287, 353, 354, 415, 418]; raw issues: long file (2218 lines > 1200), long signatures (1 functions > 8 params), many message chains (21)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Long Parameter List
  - paths: src/infra/ui_queries.rs
  - evidence: long file: 2247 lines (threshold 1200); long signatures: 1 functions > 8 params; raw issues: long file (2247 lines > 1200), long signatures (1 functions > 8 params)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Primitive Obsession
  - paths: src/integrations/provider_catalog.rs
  - evidence: long file: 1575 lines (threshold 1200); magic numbers: 181 samples=[94, 96, 100, 102, 104, 106]; raw issues: long file (1575 lines > 1200), many magic numbers (181)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Message Chains
  - paths: src/interfaces/http/retrieval.rs
  - evidence: long file: 1261 lines (threshold 1200); message chains: 33 lines with 3+ chained accesses samples=[504, 564, 585, 869, 906, 907]; raw issues: long file (1261 lines > 1200), many message chains (33)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class | secondary: Long Method
  - paths: src/interfaces/http/runtime_graph.rs
  - evidence: long file: 1608 lines (threshold 1200); branching+nested: branches=88, indent-depth=6 samples=[277, 356, 378, 419, 450, 580]; raw issues: long file (1608 lines > 1200), high branching + nesting (branches=88, depth=6)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Oversized File
  - paths: contracts/rustrag.openapi.yaml
  - evidence: long file: 10056 lines (threshold 1200); raw issues: long file (10056 lines > 1200)
  - refactorings: Extract Section, Split File (by concern), Extract Constants
- [high] Primitive Obsession
  - paths: src/app/config.rs
  - evidence: magic numbers: 66 samples=[100, 104, 105, 106, 107, 111]; raw issues: many magic numbers (66)
  - refactorings: Introduce Newtype / Value Object, Replace Magic Number with Named Constant, Replace Stringly-Typed Values with Enum, Replace Magic Number with Symbolic Constant, Replace Data Value with Object
  - micro-recipes: Untangle data and behavior
- [high] Large Class
  - paths: src/interfaces/http/graph.rs
  - evidence: long file: 1385 lines (threshold 1200); raw issues: long file (1385 lines > 1200)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class
  - paths: src/interfaces/http/mcp_memory.rs
  - evidence: long file: 1280 lines (threshold 1200); raw issues: long file (1280 lines > 1200)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class
  - paths: src/services/mcp_memory.rs
  - evidence: long file: 1543 lines (threshold 1200); raw issues: long file (1543 lines > 1200)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [high] Large Class
  - paths: src/shared/file_extract.rs
  - evidence: long file: 1209 lines (threshold 1200); raw issues: long file (1209 lines > 1200)
  - refactorings: Split Module (by concern), Extract Module, Move Function, Extract Struct, Extract Trait (when behavior varies), Extract Class, Move Method, Move Field, Extract Interface
  - patterns: Facade
  - micro-recipes: Untangle data and behavior
- [medium] Long Parameter List
  - paths: src/interfaces/http/ui_graph.rs
  - evidence: long signatures: 1 functions > 8 params; raw issues: long signatures (1 functions > 8 params)
  - refactorings: Introduce Parameter Object, Preserve Whole Object, Replace Parameter with Explicit Methods
  - patterns: Builder
  - micro-recipes: Replace flags and type switches
- [medium] Long Parameter List
  - paths: src/services/collection_settlement.rs
  - evidence: long signatures: 1 functions > 8 params; raw issues: long signatures (1 functions > 8 params)
  - refactorings: Introduce Parameter Object, Preserve Whole Object, Replace Parameter with Explicit Methods
  - patterns: Builder
  - micro-recipes: Replace flags and type switches
- [medium] Message Chains
  - paths: src/services/document_reconciliation.rs
  - evidence: message chains: 13 lines with 3+ chained accesses samples=[206, 207, 242, 312, 317, 318]; raw issues: many message chains (13)
  - refactorings: Bind Intermediate Variables (reduce deep access), Hide Delegate (via helper/facade functions), Move Function (closer to data owner), Hide Delegate, Move Method, Extract Class
  - patterns: Facade, Mediator
- [medium] Long Parameter List
  - paths: src/services/graph_diagnostics_snapshot.rs
  - evidence: long signatures: 1 functions > 8 params; raw issues: long signatures (1 functions > 8 params)
  - refactorings: Introduce Parameter Object, Preserve Whole Object, Replace Parameter with Explicit Methods
  - patterns: Builder
  - micro-recipes: Replace flags and type switches
- [medium] Long Parameter List
  - paths: src/services/graph_projection_guard.rs
  - evidence: long signatures: 1 functions > 8 params; raw issues: long signatures (1 functions > 8 params)
  - refactorings: Introduce Parameter Object, Preserve Whole Object, Replace Parameter with Explicit Methods
  - patterns: Builder
  - micro-recipes: Replace flags and type switches
- [medium] Long Parameter List
  - paths: src/services/pricing_catalog.rs
  - evidence: long signatures: 1 functions > 8 params; raw issues: long signatures (1 functions > 8 params)
  - refactorings: Introduce Parameter Object, Preserve Whole Object, Replace Parameter with Explicit Methods
  - patterns: Builder
  - micro-recipes: Replace flags and type switches
- [medium] Long Parameter List
  - paths: src/services/provider_failure_classification.rs
  - evidence: long signatures: 2 functions > 8 params; raw issues: long signatures (2 functions > 8 params)
  - refactorings: Introduce Parameter Object, Preserve Whole Object, Replace Parameter with Explicit Methods
  - patterns: Builder
  - micro-recipes: Replace flags and type switches
- [medium] Message Chains
  - paths: src/services/query_intelligence.rs
  - evidence: message chains: 21 lines with 3+ chained accesses samples=[104, 105, 106, 107, 112, 113]; raw issues: many message chains (21)
  - refactorings: Bind Intermediate Variables (reduce deep access), Hide Delegate (via helper/facade functions), Move Function (closer to data owner), Hide Delegate, Move Method, Extract Class
  - patterns: Facade, Mediator
- [medium] Long Parameter List
  - paths: src/services/terminal_settlement.rs
  - evidence: long signatures: 1 functions > 8 params; raw issues: long signatures (1 functions > 8 params)
  - refactorings: Introduce Parameter Object, Preserve Whole Object, Replace Parameter with Explicit Methods
  - patterns: Builder
  - micro-recipes: Replace flags and type switches
- [medium] Message Chains
  - paths: tests/mcp_memory_audit.rs
  - evidence: message chains: 15 lines with 3+ chained accesses samples=[74, 93, 189, 200, 214, 221]; raw issues: many message chains (15)
  - refactorings: Bind Intermediate Variables (reduce deep access), Hide Delegate (via helper/facade functions), Move Function (closer to data owner), Hide Delegate, Move Method, Extract Class
  - patterns: Facade, Mediator
- [medium] Message Chains
  - paths: tests/mcp_memory_mutations.rs
  - evidence: message chains: 17 lines with 3+ chained accesses samples=[56, 61, 66, 71, 84, 164]; raw issues: many message chains (17)
  - refactorings: Bind Intermediate Variables (reduce deep access), Hide Delegate (via helper/facade functions), Move Function (closer to data owner), Hide Delegate, Move Method, Extract Class
  - patterns: Facade, Mediator
- [low] Duplicate Code
  - paths: tests/fixtures/runtime/deepseek-fixture-manifest.json:1, tests/fixtures/runtime/openai-fixture-manifest.json:1
  - evidence: duplicate group: 2 locations; hash: ffb1ecafe173
  - refactorings: Extract Method, Introduce Parameter Object, Form Template Method
  - patterns: Strategy
  - micro-recipes: Kill duplication with variants
- [low] Risky Constructs
  - paths: src/interfaces/http/router_support.rs
  - evidence: panic!: 1 samples=[415]; multiple heuristic issues present; defaulting to Long Method treatment; raw issues: panic! usage (1)
  - refactorings: Encapsulate Unsafe, Replace unwrap/expect with Result propagation (?), Introduce Newtype / Value Object, Replace panic!/todo! with errors, Introduce Invariants (newtypes, constructors, validation), Replace panic!/todo! with errors or explicit unreachable
- [low] Risky Constructs
  - paths: src/interfaces/http/runtime_providers.rs
  - evidence: panic!: 2 samples=[664, 692]; multiple heuristic issues present; defaulting to Long Method treatment; raw issues: panic! usage (2)
  - refactorings: Encapsulate Unsafe, Replace unwrap/expect with Result propagation (?), Introduce Newtype / Value Object, Replace panic!/todo! with errors, Introduce Invariants (newtypes, constructors, validation), Replace panic!/todo! with errors or explicit unreachable

## Duplicates
- 2x (ffb1ecaf):
  - locations: tests/fixtures/runtime/deepseek-fixture-manifest.json:1, tests/fixtures/runtime/openai-fixture-manifest.json:1
  - snippet:

    ```
{
"x": "x",
"x": [
{
"x": "x",
"x": "x",
"x": "x"
},
{
"x": "x",
    ```

## Recommendations
- Some duplicated logic exists; review top duplicate groups for safe extraction.
- High-priority fix list: 15 files with major risk indicators (long files / large magic number usage).

## Refactor plan (ranked, minimal-change)
1. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/services/ingestion_worker.rs
2. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/services/runtime_ingestion.rs
3. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/infra/repositories.rs
4. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/services/graph_extract.rs
5. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/interfaces/http/runtime_documents.rs
6. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/services/query_runtime.rs
7. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/infra/ui_queries.rs
8. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/integrations/provider_catalog.rs
9. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/interfaces/http/retrieval.rs
10. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/interfaces/http/runtime_graph.rs
11. [high] Oversized File -> Extract Section, Split File (by concern), Extract Constants
   - scope: contracts/rustrag.openapi.yaml
12. [high] Primitive Obsession -> Introduce Newtype / Value Object, Replace Magic Number with Named Constant, Replace Stringly-Typed Values with Enum, Replace Magic Number with Symbolic Constant
   - scope: src/app/config.rs
13. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/interfaces/http/graph.rs
14. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/interfaces/http/mcp_memory.rs
15. [high] Large Class -> Split Module (by concern), Extract Module, Move Function, Extract Struct
   - scope: src/services/mcp_memory.rs

## Constitution update candidates
- No new duplication without extraction plan + tests for the extracted behavior.
- Cap function parameter count; require Parameter Objects for clumps/flags.
- Replace magic numbers/strings with named constants or value objects at module boundaries.
- Avoid message chains; introduce facades/delegation-hiding methods where chains repeat.
- Refactor-only commits must keep tests green; behavior changes must be isolated.