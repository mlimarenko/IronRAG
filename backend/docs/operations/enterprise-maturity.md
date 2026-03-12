# RustRAG Enterprise Maturity Operations

This document tracks the runtime-facing implementation of enterprise maturity work from `../spec-kit/specs/002-rustrag-enterprise/`.

## Current focus
- operational state visibility
- governance/admin maturity
- ingestion reliability and retry semantics
- grounded query transparency
- operator-console usefulness
- deployment honesty and release discipline

## Current known blocker
- frontend container build path is blocked in the current environment by `esbuild` postinstall `spawn sh EACCES`

## Rule
Do not claim a workflow is production-ready unless:
- validation exists
- support status is reflected in runtime docs
- operator remediation path is documented when relevant
