# RustRAG Project Constitution

## Purpose

This constitution defines the rules that should govern development of `RustRAG` as a product and platform.

Its purpose is to stop the project from drifting into:
- local optimizations without product progress
- cosmetic refactors without scenario coverage
- fake production claims unsupported by validation
- hidden coupling between backend, frontend, data model, and operations
- undocumented operational debt

This document is normative for planning, implementation, review, and release decisions.

---

## Core principles

### Principle 1 — Product-first engineering
Engineering work must improve the product, not merely improve the appearance of the codebase.

Implications:
- code cleanliness is necessary but insufficient
- scenario completeness matters more than isolated elegance
- architectural work must be justified in product and operational terms

### Principle 2 — Honest systems over aspirational systems
The project must describe the system that actually works.

Implications:
- do not claim deployment paths that do not run
- do not mark workflows as complete if they fail outside the happy path
- document blockers and workarounds explicitly
- prefer an honest limitation over a misleading green checkmark

### Principle 3 — API and UI are one product
The frontend is not ornamental and the backend is not sufficient by itself.

Implications:
- major workflows must be coherent across API and UI
- frontend contract drift is a release risk
- generated contracts are preferred over hand-maintained duplicate models
- operator usability is a product requirement, not polish work

### Principle 4 — Operability is part of correctness
A system that works only when manually babysat is not production-ready.

Implications:
- health and readiness must be meaningful
- job state must be inspectable
- failures must be diagnosable
- recovery procedures must be documentable and realistic

### Principle 5 — Reliability before scale theater
Do not optimize for imaginary scale while basic failure handling, observability, and recoverability remain weak.

Implications:
- prioritize idempotency, retries, timeouts, isolation, and diagnostics
- scale-readiness must preserve correctness and operator trust
- avoid adding distributed complexity without evidence it solves current pain

### Principle 6 — Security boundaries are product boundaries
Auth, secrets handling, provider credentials, and privileged operations must be designed deliberately.

Implications:
- secrets must not leak into logs, generated code, or casual persistence
- privileged actions must be auditable
- scopes and permissions must be explicit enough to reason about
- safe defaults beat permissive convenience

### Principle 7 — Every durable decision must leave a trace
Important architectural, operational, and workflow decisions must be recorded in docs or stable project artifacts.

Implications:
- do not rely on session memory or tribal knowledge
- runbooks and release notes are first-class outputs
- known caveats belong in docs, not only in chat history

### Principle 8 — Enterprise quality requires layered validation
No single check is enough to declare quality.

Implications:
- static analysis is not enough
- unit/integration/scenario validation must exist where justified
- frontend validation matters as much as backend validation
- deployment and packaging validation are separate concerns, not implied by local builds

---

## Mandatory delivery rules

### Rule 1 — Work must be tied to a scenario or platform capability
Every non-trivial change must strengthen one of:
- user-facing workflow completeness
- platform operability
- security posture
- deployment readiness
- architecture sustainability

### Rule 2 — Changes must preserve explicit boundaries
Avoid hidden cross-layer coupling between:
- transport models and domain models
- provider-specific behavior and platform behavior
- frontend convenience code and backend truth
- runtime workarounds and long-term supported contracts

### Rule 3 — Repeated patterns should be standardized
If the same validation, state transition, or integration pattern appears multiple times, a shared abstraction or documented standard should be preferred.

### Rule 4 — Destructive or risky behavior requires explicit framing
Any change that can affect data integrity, auth semantics, deployment reliability, or operational safety must be:
- clearly described
- validated proportionally
- documented if it changes operator expectations

### Rule 5 — Documentation must move with reality
When implementation meaningfully changes:
- developer docs must be updated
- deployment notes must be updated
- release checklists must be updated if behavior or validation changed

### Rule 6 — Generated artifacts must be treated as generated
Generated code should not become a hand-maintained divergence point.

### Rule 7 — Technical debt must be classified honestly
Debt should be described as one of:
- blocker
- operational risk
- quality risk
- deferred enhancement
- cosmetic cleanup

Do not hide blocker-level debt inside generic TODO language.

### Rule 8 — The frontend must mature with the platform
Backend maturity alone does not satisfy platform maturity.

Required focus areas:
- operator workflows
- state visibility
- retrieval explainability
- ingestion diagnostics
- contract alignment

---

## Quality gates

A milestone should not be considered production-grade unless the following classes are addressed proportionally to scope:

### Backend quality gates
- formatting
- strict linting where enforced
- tests appropriate to the changed behavior
- contract verification where applicable
- operational error-path review

### Frontend quality gates
- lint
- formatting
- typecheck
- production build
- contract alignment verification
- empty/loading/error-state review for changed flows

### Platform quality gates
- meaningful health/readiness behavior
- dependency failure behavior reviewed
- job/retry/idempotency semantics reviewed where relevant
- diagnostics visibility reviewed

### Packaging and deployment quality gates
- documented supported path
- documented unsupported/blocked path if any
- validation of real artifact behavior, not just file presence

### Documentation quality gates
- README reflects actual state
- service-specific docs reflect actual workflows
- release checklist reflects actual validation expectations

---

## Review standards

Every substantial review should ask:
- what scenario does this improve?
- what operator burden does this reduce or introduce?
- what failure mode becomes easier or harder to diagnose?
- does this preserve clean boundaries?
- is the claimed readiness level actually validated?
- what docs or runbooks must change?

If these questions cannot be answered, the work is not mature enough.

---

## Release standards

A release candidate must not be treated as trustworthy unless:
- core workflows have been validated end-to-end
- known blockers are explicit
- deployment status is honest
- frontend and backend compatibility is verified
- major operational failure modes have a documented response path

---

## Planning standards

Plans derived from this constitution should:
- decompose large work into scenario-aligned streams
- separate architecture, implementation, validation, and documentation work
- identify blockers vs nice-to-haves clearly
- include frontend and operator workflows explicitly
- include production-readiness and observability explicitly

---

## Amendment rule

This constitution may evolve as the project matures, but changes must make the rules clearer or stronger, not weaker or vaguer.

If a new principle is added, it should constrain behavior meaningfully and improve product trustworthiness.
