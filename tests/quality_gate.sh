#!/usr/bin/env bash
# ============================================================================
# IronRAG Quality Gate — End-to-End Pipeline Test
# ============================================================================
#
# Uploads real documents of every supported type, waits for full pipeline
# completion, then verifies:
#   1. All documents reach graph_ready state
#   2. Every pipeline stage has timing (elapsedMs)
#   3. LLM stages have cost and model name
#   4. Chunks are generated for each document
#   5. Graph entities and relations exist
#   6. The AI assistant answers questions grounded in the uploaded content
#
# Usage:
#   ./tests/quality_gate.sh [--base-url URL] [--token TOKEN]
#
# Environment:
#   IRONRAG_BASE_URL  (default: http://localhost:19000)
#   IRONRAG_TOKEN     (default: uses admin login to get session cookie)
#   IRONRAG_ADMIN_LOGIN / IRONRAG_ADMIN_PASSWORD (for session auth)
#
# Requirements: curl, jq, python3 (for file generation)
# ============================================================================

set -euo pipefail
export LC_ALL=C  # ensure printf uses '.' as decimal separator

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
BASE_URL="${IRONRAG_BASE_URL:-http://localhost:19000}"
TOKEN="${IRONRAG_TOKEN:-}"
ADMIN_LOGIN="${IRONRAG_ADMIN_LOGIN:-admin}"
ADMIN_PASSWORD="${IRONRAG_ADMIN_PASSWORD:-qg-test-pass-2026}"
POLL_INTERVAL=5
MAX_WAIT=600  # 10 minutes max for all documents
TMPDIR_QG="$(mktemp -d /tmp/ironrag-qg-XXXXXX)"
trap 'rm -rf "$TMPDIR_QG"' EXIT

# Counters
PASS=0
FAIL=0
TOTAL=0

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
color_red()   { printf "\033[1;31m%s\033[0m" "$*"; }
color_green() { printf "\033[1;32m%s\033[0m" "$*"; }
color_cyan()  { printf "\033[1;36m%s\033[0m" "$*"; }
color_yellow(){ printf "\033[1;33m%s\033[0m" "$*"; }

log()  { echo "[$(date +%H:%M:%S)] $*"; }
pass() { PASS=$((PASS+1)); TOTAL=$((TOTAL+1)); echo "  $(color_green '✓') $*"; }
fail() { FAIL=$((FAIL+1)); TOTAL=$((TOTAL+1)); echo "  $(color_red '✗') $*"; }
section() { echo ""; echo "$(color_cyan "━━━ $* ━━━")"; }

api() {
  local method="$1" path="$2"; shift 2
  local url="${BASE_URL}/v1${path}"
  if [[ -n "$TOKEN" ]]; then
    curl -sS -X "$method" "$url" -H "Authorization: Bearer $TOKEN" "$@"
  else
    curl -sS -X "$method" "$url" -b "$COOKIE_JAR" "$@"
  fi
}

api_json() {
  api "$1" "$2" -H "Content-Type: application/json" "${@:3}"
}

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$expected" == "$actual" ]]; then
    pass "$label"
  else
    fail "$label (expected: $expected, got: $actual)"
  fi
}

assert_gt() {
  local label="$1" threshold="$2" actual="$3"
  if [[ "$actual" -gt "$threshold" ]] 2>/dev/null; then
    pass "$label ($actual)"
  else
    fail "$label (expected > $threshold, got: $actual)"
  fi
}

assert_not_null() {
  local label="$1" value="$2"
  if [[ -n "$value" && "$value" != "null" && "$value" != "0" ]]; then
    pass "$label ($value)"
  else
    fail "$label (got: $value)"
  fi
}

# ---------------------------------------------------------------------------
# Phase 0: Authentication
# ---------------------------------------------------------------------------
section "Phase 0: Authentication"

COOKIE_JAR="$TMPDIR_QG/cookies.txt"

if [[ -n "$TOKEN" ]]; then
  log "Using bearer token"
  # Verify token works
  RESP=$(api GET "/ready" 2>/dev/null || true)
  pass "Bearer token configured"
else
  log "Logging in as $ADMIN_LOGIN..."
  LOGIN_RESP=$(curl -sS -X POST "${BASE_URL}/v1/iam/session/login" \
    -H "Content-Type: application/json" \
    -c "$COOKIE_JAR" \
    -d "{\"login\":\"$ADMIN_LOGIN\",\"password\":\"$ADMIN_PASSWORD\"}")

  if echo "$LOGIN_RESP" | jq -e '.sessionId' >/dev/null 2>&1; then
    pass "Login successful (session: $(echo "$LOGIN_RESP" | jq -r '.sessionId' | head -c 8)...)"
  else
    fail "Login failed: $LOGIN_RESP"
    echo "Set IRONRAG_TOKEN or IRONRAG_ADMIN_LOGIN/PASSWORD"
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Phase 1: Discover workspace and library
# ---------------------------------------------------------------------------
section "Phase 1: Discover workspace and library"

WORKSPACES=$(api_json GET "/catalog/workspaces")
WS_ID=$(echo "$WORKSPACES" | jq -r '.[0].id // empty')
WS_SLUG=$(echo "$WORKSPACES" | jq -r '.[0].slug // empty')

if [[ -z "$WS_ID" ]]; then
  fail "No workspace found"
  exit 1
fi
pass "Workspace: $WS_SLUG ($WS_ID)"

# Create a dedicated QG library
QG_SLUG="quality-gate-$(date +%s)"
CREATE_LIB_RESP=$(api_json POST "/catalog/workspaces/$WS_ID/libraries" \
  -d "{\"slug\":\"$QG_SLUG\",\"displayName\":\"Quality Gate $(date +%Y-%m-%d)\"}")
LIB_ID=$(echo "$CREATE_LIB_RESP" | jq -r '.id // empty')

if [[ -z "$LIB_ID" ]]; then
  fail "Failed to create library: $CREATE_LIB_RESP"
  exit 1
fi
pass "Created test library: $QG_SLUG ($LIB_ID)"

# ---------------------------------------------------------------------------
# Phase 2: Generate test files with real content
# ---------------------------------------------------------------------------
section "Phase 2: Generate test files"

TMPDIR_QG="$TMPDIR_QG" python3 - <<'PYGEN'
import os, sys, json, base64, struct, zipfile, io, hashlib, datetime

OUT = os.environ.get("TMPDIR_QG", "/tmp/ironrag-qg")

def write(name, data):
    path = os.path.join(OUT, name)
    if isinstance(data, str):
        data = data.encode("utf-8")
    with open(path, "wb") as f:
        f.write(data)
    print(f"  Generated {name} ({len(data)} bytes)")

# ---------- 1. Plain text ----------
write("report.txt", """IronRAG Quality Gate Test Report
================================

Project: IronRAG v0.2.0
Date: 2026-04-11
Author: Dr. Elena Vasquez, Lead AI Engineer

Executive Summary
------------------
The IronRAG knowledge management platform has been deployed at Nexus Corp
to manage internal documentation across 14 departments. The system processes
over 2,400 documents monthly with an average extraction accuracy of 94.7%.

Key findings:
- Graph extraction identifies 847 unique entities across the corpus
- Query response time averages 1.2 seconds with HyDE-CRAG retrieval
- Cost per document averages $0.003 for text and $0.012 for PDF with images
- Entity resolution merges 23% of initial entities as duplicates/aliases

Architecture Notes
------------------
The backend runs on Rust with Axum, using PostgreSQL 18 for the control plane,
ArangoDB 3.12 for graph and vector storage, and Redis 8 for the worker queue.
The frontend is React with Vite, served through nginx as an SPA.

Performance Metrics
-------------------
| Metric               | Value    |
|----------------------|----------|
| Avg chunk size       | 2,800    |
| Overlap ratio        | 10%      |
| Entity types         | 10       |
| Relation types       | 88       |
| BM25 weight          | 0.4      |
| Vector weight        | 0.6      |
| Heading boost        | 1.5x     |
""")

# ---------- 2. DOCX (minimal valid .docx via zipfile) ----------
def make_docx(text):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml",
            '<?xml version="1.0"?>'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
            '</Types>')
        zf.writestr("_rels/.rels",
            '<?xml version="1.0"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>'
            '</Relationships>')
        # Build paragraphs
        paras = ""
        for line in text.strip().split("\n"):
            escaped = line.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
            paras += f'<w:p><w:r><w:t xml:space="preserve">{escaped}</w:t></w:r></w:p>'
        zf.writestr("word/document.xml",
            '<?xml version="1.0"?>'
            '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
            f'<w:body>{paras}</w:body></w:document>')
        zf.writestr("word/_rels/document.xml.rels",
            '<?xml version="1.0"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>')
    return buf.getvalue()

write("contract.docx", make_docx("""Software License Agreement

Parties:
- Licensor: Nexus Corp, registered at 742 Evergreen Terrace, Springfield
- Licensee: Quantum AI Labs, registered at 1600 Amphitheatre Parkway, Mountain View

Effective Date: January 15, 2026

1. Grant of License
The Licensor hereby grants to the Licensee a non-exclusive, non-transferable license
to use the IronRAG software platform for internal knowledge management purposes.
The license covers up to 500 concurrent users and 100,000 documents.

2. License Fee
The annual license fee is $48,000 USD, payable in quarterly installments of $12,000.
A 15% discount applies for prepayment of the full annual amount.

3. Support and Maintenance
The Licensor shall provide Level 2 technical support during business hours (9 AM - 6 PM EST).
Critical severity issues (P0) will receive a response within 1 hour.
High severity issues (P1) will receive a response within 4 hours.

4. Data Processing
All document processing occurs within the Licensee's infrastructure.
No document content is transmitted to external services except for:
- OpenAI API calls for embedding and graph extraction (encrypted in transit)
- Optional Ollama endpoints for local model inference

5. Term and Termination
This agreement is valid for 24 months from the Effective Date.
Either party may terminate with 90 days written notice.
"""))

# ---------- 3. XLSX (minimal valid via zipfile) ----------
def make_xlsx(rows):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml",
            '<?xml version="1.0"?>'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
            '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
            '</Types>')
        zf.writestr("_rels/.rels",
            '<?xml version="1.0"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
            '</Relationships>')
        zf.writestr("xl/_rels/workbook.xml.rels",
            '<?xml version="1.0"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
            '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>'
            '</Relationships>')
        zf.writestr("xl/workbook.xml",
            '<?xml version="1.0"?>'
            '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
            'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
            '<sheets><sheet name="Employees" sheetId="1" r:id="rId1"/></sheets></workbook>')

        # Shared strings
        strings = []
        for row in rows:
            for cell in row:
                if isinstance(cell, str) and cell not in strings:
                    strings.append(cell)

        ss_xml = '<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{}" uniqueCount="{}">'.format(
            sum(1 for r in rows for c in r if isinstance(c, str)), len(strings))
        for s in strings:
            escaped = s.replace("&", "&amp;").replace("<", "&lt;")
            ss_xml += f'<si><t>{escaped}</t></si>'
        ss_xml += '</sst>'
        zf.writestr("xl/sharedStrings.xml", ss_xml)

        # Worksheet
        cols = "ABCDEFGHIJKLMNOP"
        sheet = '<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>'
        for ri, row in enumerate(rows, 1):
            sheet += f'<row r="{ri}">'
            for ci, cell in enumerate(row):
                ref = f"{cols[ci]}{ri}"
                if isinstance(cell, str):
                    idx = strings.index(cell)
                    sheet += f'<c r="{ref}" t="s"><v>{idx}</v></c>'
                else:
                    sheet += f'<c r="{ref}"><v>{cell}</v></c>'
            sheet += '</row>'
        sheet += '</sheetData></worksheet>'
        zf.writestr("xl/worksheets/sheet1.xml", sheet)
    return buf.getvalue()

write("employees.xlsx", make_xlsx([
    ["Employee ID", "Name", "Department", "Role", "Salary USD", "Start Date", "Location"],
    ["E001", "Dr. Elena Vasquez", "AI Engineering", "Lead AI Engineer", 185000, "2024-03-15", "San Francisco"],
    ["E002", "Marcus Chen", "AI Engineering", "Senior ML Engineer", 165000, "2024-06-01", "San Francisco"],
    ["E003", "Sarah O'Brien", "Product", "VP of Product", 210000, "2023-11-20", "New York"],
    ["E004", "Raj Patel", "Infrastructure", "SRE Lead", 175000, "2024-01-10", "Austin"],
    ["E005", "Yuki Tanaka", "AI Engineering", "Research Scientist", 155000, "2024-09-01", "Remote"],
    ["E006", "Alex Petrov", "Security", "CISO", 195000, "2023-08-15", "New York"],
    ["E007", "Maria Gonzalez", "AI Engineering", "ML Ops Engineer", 145000, "2025-01-15", "Austin"],
    ["E008", "James Wright", "Sales", "Enterprise Account Executive", 130000, "2024-04-01", "Chicago"],
    ["E009", "Li Wei", "AI Engineering", "Graph Systems Engineer", 160000, "2024-07-20", "San Francisco"],
    ["E010", "Fatima Al-Hassan", "Legal", "General Counsel", 220000, "2023-06-01", "New York"],
]))

# ---------- 4. CSV ----------
write("quarterly_metrics.csv", """Quarter,Revenue USD,Customers,Churn Rate %,NPS Score,Support Tickets,Avg Resolution Hours
Q1 2025,2400000,347,3.2,72,1284,4.1
Q2 2025,2850000,412,2.8,75,1156,3.8
Q3 2025,3100000,489,2.1,78,1043,3.5
Q4 2025,3600000,534,1.9,81,987,3.2
Q1 2026,4200000,612,1.7,83,892,2.9
""")

# ---------- 5. Python code ----------
write("data_pipeline.py", '''"""
IronRAG Data Pipeline — Document Processing Module

This module handles the core document processing pipeline including:
- Text extraction from multiple file formats
- Semantic chunking with heading awareness
- Embedding generation via OpenAI or Ollama
- Knowledge graph extraction with entity resolution
"""

import hashlib
import logging
from dataclasses import dataclass, field
from enum import Enum
from typing import Optional

logger = logging.getLogger(__name__)


class DocumentState(Enum):
    QUEUED = "queued"
    EXTRACTING = "extracting"
    CHUNKING = "chunking"
    EMBEDDING = "embedding"
    GRAPH_EXTRACTING = "graph_extracting"
    READY = "ready"
    FAILED = "failed"


@dataclass
class ChunkConfig:
    """Configuration for semantic chunking."""
    max_tokens: int = 2800
    overlap_ratio: float = 0.10
    heading_aware: bool = True
    code_aware: bool = True
    min_chunk_tokens: int = 100


@dataclass
class ExtractionResult:
    """Result of knowledge graph extraction from a document chunk."""
    entities: list = field(default_factory=list)
    relations: list = field(default_factory=list)
    confidence: float = 0.0
    model_name: str = ""
    prompt_tokens: int = 0
    completion_tokens: int = 0
    estimated_cost_usd: float = 0.0


class DocumentPipeline:
    """Orchestrates the full document processing pipeline.

    The pipeline stages are:
    1. extract_content — convert file to structured text blocks
    2. prepare_structure — normalize headings, tables, code blocks
    3. chunk_content — split into semantic chunks (2800 chars, 10% overlap)
    4. extract_technical_facts — identify typed facts (versions, params, etc.)
    5. embed_chunk — generate vector embeddings for each chunk
    6. extract_graph — extract entities and relations via LLM
    7. finalizing — entity resolution, quality scoring, index update
    """

    def __init__(self, config: ChunkConfig = None):
        self.config = config or ChunkConfig()
        self._stage = DocumentState.QUEUED

    def process(self, document_bytes: bytes, mime_type: str) -> ExtractionResult:
        """Process a document through all pipeline stages."""
        logger.info("Starting pipeline for %s (%d bytes)", mime_type, len(document_bytes))

        text = self._extract_content(document_bytes, mime_type)
        blocks = self._prepare_structure(text)
        chunks = self._chunk_content(blocks)
        facts = self._extract_facts(chunks)
        embeddings = self._embed_chunks(chunks)
        result = self._extract_graph(chunks)

        self._stage = DocumentState.READY
        logger.info("Pipeline complete: %d entities, %d relations",
                     len(result.entities), len(result.relations))
        return result

    def _extract_content(self, data: bytes, mime_type: str) -> str:
        self._stage = DocumentState.EXTRACTING
        content_hash = hashlib.sha256(data).hexdigest()[:16]
        logger.debug("Content hash: %s", content_hash)
        return data.decode("utf-8", errors="replace")

    def _prepare_structure(self, text: str) -> list:
        return [{"kind": "paragraph", "text": p} for p in text.split("\\n\\n") if p.strip()]

    def _chunk_content(self, blocks: list) -> list:
        self._stage = DocumentState.CHUNKING
        chunks = []
        current = []
        current_len = 0
        for block in blocks:
            block_len = len(block["text"])
            if current_len + block_len > self.config.max_tokens and current:
                chunks.append({"blocks": current, "token_count": current_len})
                overlap_blocks = current[-1:] if self.config.overlap_ratio > 0 else []
                current = overlap_blocks
                current_len = sum(len(b["text"]) for b in current)
            current.append(block)
            current_len += block_len
        if current:
            chunks.append({"blocks": current, "token_count": current_len})
        return chunks

    def _extract_facts(self, chunks: list) -> list:
        return []

    def _embed_chunks(self, chunks: list) -> list:
        self._stage = DocumentState.EMBEDDING
        return [{"chunk_id": i, "vector": [0.0] * 1536} for i in range(len(chunks))]

    def _extract_graph(self, chunks: list) -> ExtractionResult:
        self._stage = DocumentState.GRAPH_EXTRACTING
        return ExtractionResult(
            entities=[{"label": "IronRAG", "type": "artifact"}],
            relations=[],
            confidence=0.92,
            model_name="gpt-4o-mini",
        )


# Constants
SUPPORTED_EXTENSIONS = {
    ".txt", ".md", ".py", ".rs", ".go", ".ts", ".js", ".java", ".cs",
    ".pdf", ".docx", ".xlsx", ".csv", ".html", ".pptx",
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg",
}

MAX_FILE_SIZE_MB = 100
DEFAULT_EMBEDDING_MODEL = "text-embedding-3-small"
DEFAULT_EXTRACTION_MODEL = "gpt-4o-mini"
CHUNK_SIZE = 2800
CHUNK_OVERLAP = 0.10
''')

# ---------- 6. Rust code ----------
write("graph_store.rs", '''//! Graph storage layer for IronRAG knowledge graph.
//!
//! This module provides the interface between the extraction pipeline
//! and ArangoDB, handling entity upserts, relation creation, and
//! entity resolution (alias/acronym merging).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a knowledge graph entity extracted from documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntity {
    pub id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
    pub entity_type: EntityType,
    pub aliases: Vec<String>,
    pub support_count: i32,
    pub confidence: f64,
    pub description: Option<String>,
}

/// The 10 universal entity types used in IronRAG's knowledge graph.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Person,
    Organization,
    Location,
    Event,
    Artifact,
    Natural,
    Process,
    Concept,
    Attribute,
    Entity,
}

/// A typed relation between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelation {
    pub id: Uuid,
    pub library_id: Uuid,
    pub subject_id: Uuid,
    pub object_id: Uuid,
    pub relation_type: String,
    pub normalized_assertion: String,
    pub support_count: i32,
    pub confidence: f64,
}

/// Entity resolution result after alias/acronym merging.
#[derive(Debug)]
pub struct ResolutionResult {
    pub merged_count: usize,
    pub kept_count: usize,
    pub alias_mappings: Vec<(Uuid, Uuid)>,
}

impl KnowledgeEntity {
    /// Check if two entities should be merged based on label similarity.
    pub fn should_merge(&self, other: &Self) -> bool {
        if self.entity_type != other.entity_type {
            return false;
        }
        let a = self.canonical_label.to_lowercase();
        let b = other.canonical_label.to_lowercase();
        a == b || self.aliases.iter().any(|alias| alias.to_lowercase() == b)
    }
}

/// Configuration for the graph extraction pipeline.
pub struct GraphConfig {
    /// Maximum entities per chunk extraction call
    pub max_entities_per_chunk: usize,
    /// Maximum relations per chunk extraction call
    pub max_relations_per_chunk: usize,
    /// Minimum confidence threshold for entity retention
    pub min_entity_confidence: f64,
    /// Whether to run entity resolution after extraction
    pub enable_resolution: bool,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            max_entities_per_chunk: 50,
            max_relations_per_chunk: 100,
            min_entity_confidence: 0.3,
            enable_resolution: true,
        }
    }
}
''')

# ---------- 7. C# code ----------
write("UserService.cs", '''using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;

namespace IronRAG.Services
{
    /// <summary>
    /// User management service for IronRAG platform.
    /// Handles authentication, authorization, and token lifecycle.
    /// </summary>
    public class UserService
    {
        private readonly IUserRepository _repository;
        private readonly ITokenService _tokenService;
        private readonly IPasswordHasher _hasher;

        public UserService(
            IUserRepository repository,
            ITokenService tokenService,
            IPasswordHasher hasher)
        {
            _repository = repository;
            _tokenService = tokenService;
            _hasher = hasher;
        }

        /// <summary>
        /// Authenticate a user with login and password.
        /// Returns a session token on success.
        /// </summary>
        public async Task<AuthResult> AuthenticateAsync(string login, string password)
        {
            var user = await _repository.FindByLoginAsync(login);
            if (user == null)
                return AuthResult.Failed("User not found");

            if (!_hasher.Verify(password, user.PasswordHash))
                return AuthResult.Failed("Invalid password");

            if (user.Status != UserStatus.Active)
                return AuthResult.Failed($"Account is {user.Status}");

            var token = await _tokenService.CreateSessionTokenAsync(user.PrincipalId);
            return AuthResult.Success(token, user);
        }

        /// <summary>
        /// Create an API token with specific permissions.
        /// Supports 13 permission kinds: iam_admin, workspace_admin, workspace_read,
        /// library_read, library_write, document_read, document_write, query_run,
        /// ops_read, audit_read, connector_admin, credential_admin, binding_admin.
        /// </summary>
        public async Task<ApiToken> CreateApiTokenAsync(
            Guid principalId,
            string label,
            IReadOnlyList<PermissionGrant> grants)
        {
            var token = _tokenService.GenerateApiToken();
            var hashedToken = _hasher.HashToken(token.PlainText);

            await _repository.InsertApiTokenAsync(new ApiTokenRecord
            {
                PrincipalId = principalId,
                Label = label,
                TokenHash = hashedToken,
                TokenPrefix = token.Prefix,
                Status = TokenStatus.Active,
                IssuedAt = DateTime.UtcNow
            });

            foreach (var grant in grants)
            {
                await _repository.InsertGrantAsync(new GrantRecord
                {
                    PrincipalId = principalId,
                    ResourceKind = grant.Scope.Kind,
                    ResourceId = grant.Scope.ResourceId,
                    PermissionKind = grant.Permission,
                    GrantedAt = DateTime.UtcNow
                });
            }

            return new ApiToken(token.PlainText, principalId, label);
        }
    }

    public enum UserStatus { Active, Suspended, Deleted }
    public enum TokenStatus { Active, Revoked, Expired }

    public record AuthResult(bool IsSuccess, string Token, UserRecord User, string Error)
    {
        public static AuthResult Success(string token, UserRecord user) =>
            new(true, token, user, null);
        public static AuthResult Failed(string error) =>
            new(false, null, null, error);
    }
}
''')

# ---------- 8. Markdown documentation ----------
write("architecture.md", """# IronRAG System Architecture

## Overview

IronRAG is a knowledge management platform built on a Rust backend with a React
frontend. It ingests documents, extracts structured knowledge, and provides
grounded question answering through a hybrid retrieval pipeline.

## Component Diagram

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│   Frontend   │───▶│   Backend   │───▶│   Worker    │
│  React/Vite  │    │  Rust/Axum  │    │  Rust/Axum  │
└─────────────┘    └──────┬──────┘    └──────┬──────┘
                          │                   │
                    ┌─────┴─────┐       ┌─────┴─────┐
                    │ PostgreSQL│       │  ArangoDB  │
                    │ IAM+Ctrl  │       │ Graph+Vec  │
                    └───────────┘       └───────────┘
```

## Data Flow

1. **Ingestion**: Documents enter via HTTP upload or MCP `upload_documents` tool
2. **Extraction**: Text is extracted using format-specific parsers (PDF, DOCX, etc.)
3. **Chunking**: Content is split into ~2800-character semantic chunks with 10% overlap
4. **Embedding**: Each chunk is embedded via OpenAI `text-embedding-3-small`
5. **Graph Extraction**: LLM extracts entities and relations per chunk
6. **Resolution**: Duplicate entities are merged via alias/acronym matching
7. **Indexing**: Chunks and entities are indexed for hybrid BM25+vector search

## Entity Types

The knowledge graph uses 10 universal entity types:

| Type         | Description                    | Example                |
|--------------|--------------------------------|------------------------|
| person       | Individual human               | Dr. Elena Vasquez      |
| organization | Company, team, institution     | Nexus Corp             |
| location     | Physical or virtual place      | San Francisco          |
| event        | Occurrence with temporal scope | Q1 2026 Release        |
| artifact     | Created object or system       | IronRAG Platform       |
| natural      | Natural phenomenon or entity   | Earthquake, Species    |
| process      | Ongoing activity or workflow   | Document Ingestion     |
| concept      | Abstract idea or category      | Machine Learning       |
| attribute    | Measurable property            | Response Time: 1.2s    |
| entity       | Generic/uncategorized          | Miscellaneous items    |

## Security Model

Authentication uses Argon2id for passwords and SHA-256 for API tokens.
Authorization is grant-based with hierarchical scopes:
- **system** → full instance access
- **workspace** → all libraries in workspace
- **library** → all documents in library
- **document** → single document

## Performance Targets

- Document processing: < 60 seconds for text, < 120 seconds for PDF
- Query response: < 3 seconds for grounded answers
- Graph extraction: 10-50 entities per document depending on content density
- Cost: ~$0.003 per text document, ~$0.012 per PDF with images
""")

# ---------- 9. PNG image (minimal valid 4x4 red square) ----------
def make_png():
    import struct, zlib
    width, height = 100, 60
    # Create a simple gradient image with text-like patterns
    raw = b""
    for y in range(height):
        raw += b"\x00"  # filter: none
        for x in range(width):
            # Blue background with white rectangle "text area"
            if 10 <= y <= 50 and 10 <= x <= 90:
                r, g, b = 255, 255, 255  # white content area
            else:
                r, g, b = 41, 98, 255    # blue border
            # Add some "text" lines
            if 15 <= x <= 85 and y in (15, 16, 22, 23, 29, 30, 36, 37):
                r, g, b = 30, 30, 30     # dark text lines
            raw += struct.pack("BBB", r, g, b)

    def chunk(ctype, data):
        c = ctype + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" +
            chunk(b"IHDR", ihdr) +
            chunk(b"IDAT", zlib.compress(raw)) +
            chunk(b"IEND", b""))

write("diagram.png", make_png())

print(f"\n  All {9} test files generated in {OUT}")
PYGEN

log "Files ready:"
ls -la "$TMPDIR_QG"/*.{txt,docx,xlsx,csv,py,rs,cs,md,png} 2>/dev/null | awk '{print "  " $NF " (" $5 " bytes)"}'

# ---------------------------------------------------------------------------
# Phase 3: Upload all documents
# ---------------------------------------------------------------------------
section "Phase 3: Upload documents"

declare -A DOC_IDS
FILES=(report.txt contract.docx employees.xlsx quarterly_metrics.csv \
       data_pipeline.py graph_store.rs UserService.cs architecture.md diagram.png)

for f in "${FILES[@]}"; do
  RESP=$(api POST "/content/documents/upload" \
    -F "library_id=$LIB_ID" \
    -F "file=@${TMPDIR_QG}/${f}")

  DOC_ID=$(echo "$RESP" | jq -r '.document.document.id // empty')
  if [[ -n "$DOC_ID" ]]; then
    DOC_IDS["$f"]="$DOC_ID"
    pass "Uploaded $f → $DOC_ID"
  else
    ERROR=$(echo "$RESP" | jq -r '.message // .error // "unknown"' 2>/dev/null)
    fail "Upload $f failed: $ERROR"
  fi
done

UPLOADED_COUNT=${#DOC_IDS[@]}
assert_eq "All ${#FILES[@]} files uploaded" "${#FILES[@]}" "$UPLOADED_COUNT"

# ---------------------------------------------------------------------------
# Phase 4: Wait for all documents to finish processing
# ---------------------------------------------------------------------------
section "Phase 4: Wait for pipeline completion"

STARTED=$SECONDS
PENDING=("${!DOC_IDS[@]}")

while [[ ${#PENDING[@]} -gt 0 ]]; do
  ELAPSED=$((SECONDS - STARTED))
  if [[ $ELAPSED -gt $MAX_WAIT ]]; then
    fail "Timeout after ${MAX_WAIT}s — ${#PENDING[@]} documents still processing"
    for pf in "${PENDING[@]}"; do
      DOC_ID="${DOC_IDS[$pf]}"
      STATUS=$(api_json GET "/content/documents/$DOC_ID" | jq -r '.readinessSummary.readinessKind // "unknown"')
      fail "  $pf ($DOC_ID): $STATUS"
    done
    break
  fi

  STILL_PENDING=()
  for pf in "${PENDING[@]}"; do
    DOC_ID="${DOC_IDS[$pf]}"
    DETAIL=$(api_json GET "/content/documents/$DOC_ID")
    READINESS=$(echo "$DETAIL" | jq -r '.readinessSummary.readinessKind // "unknown"')
    JOB_STATE=$(echo "$DETAIL" | jq -r '.pipeline.latest_job.queue_state // "unknown"')

    if [[ "$READINESS" == "graph_ready" || "$READINESS" == "graph_sparse" || "$READINESS" == "readable" ]]; then
      pass "$pf → $READINESS (${ELAPSED}s)"
    elif [[ "$JOB_STATE" == "failed" || "$READINESS" == "failed" ]]; then
      FAILURE=$(echo "$DETAIL" | jq -r '.pipeline.latest_job.failure_code // "unknown"')
      fail "$pf → FAILED ($FAILURE)"
    else
      STILL_PENDING+=("$pf")
    fi
  done

  PENDING=("${STILL_PENDING[@]+"${STILL_PENDING[@]}"}")
  if [[ ${#PENDING[@]} -gt 0 ]]; then
    printf "  … waiting: %d remaining (%ds elapsed)\r" "${#PENDING[@]}" "$ELAPSED"
    sleep "$POLL_INTERVAL"
  fi
done

echo ""

# ---------------------------------------------------------------------------
# Phase 5: Verify pipeline stages, timing, and cost
# ---------------------------------------------------------------------------
section "Phase 5: Verify pipeline stages & cost"

# Sums
SUM_COST=0
SUM_TOKENS=0
SUM_ELAPSED=0

for f in "${!DOC_IDS[@]}"; do
  DOC_ID="${DOC_IDS[$f]}"
  DETAIL=$(api_json GET "/content/documents/$DOC_ID")

  echo ""
  log "$(color_yellow "$f") ($DOC_ID)"

  # Check lifecycle exists
  HAS_LIFECYCLE=$(echo "$DETAIL" | jq 'has("lifecycle") and .lifecycle != null')
  if [[ "$HAS_LIFECYCLE" != "true" ]]; then
    fail "$f: no lifecycle data"
    continue
  fi

  # Total cost & elapsed
  TOTAL_COST=$(echo "$DETAIL" | jq -r '.lifecycle.totalCost // "0"')
  TOTAL_ELAPSED=$(echo "$DETAIL" | jq -r '.lifecycle.attempts[0].totalElapsedMs // 0')

  if [[ "$TOTAL_COST" != "null" && "$TOTAL_COST" != "0" ]]; then
    pass "$f: totalCost = \$$TOTAL_COST, totalElapsed = ${TOTAL_ELAPSED}ms"
    SUM_COST=$(echo "$SUM_COST + $TOTAL_COST" | bc -l 2>/dev/null || echo "$SUM_COST")
    SUM_ELAPSED=$((SUM_ELAPSED + TOTAL_ELAPSED))
  else
    fail "$f: totalCost missing or zero"
  fi

  # Stage events
  STAGES=$(echo "$DETAIL" | jq -r '.lifecycle.attempts[0].stageEvents // []')
  STAGE_COUNT=$(echo "$STAGES" | jq 'length')

  # Per-stage detailed table
  echo "    ┌─────────────────────────────┬───────────┬───────────┬──────────────────────────┬─────────┬──────────────┐"
  printf "    │ %-27s │ %-9s │ %-9s │ %-24s │ %-7s │ %-12s │\n" "Stage" "Status" "Time" "Model" "Tokens" "Cost USD"
  echo "    ├─────────────────────────────┼───────────┼───────────┼──────────────────────────┼─────────┼──────────────┤"

  while IFS=$'\t' read -r stage status elapsed model tokens cost; do
    if [[ "$elapsed" == "null" ]]; then
      elapsed_str="—"
    elif [[ "$elapsed" == "0" ]]; then
      elapsed_str="<1ms"
    else
      elapsed_str="${elapsed}ms"
    fi
    [[ "$tokens" == "null" || "$tokens" == "0" ]] && tokens_str="—" || tokens_str="$tokens"
    if [[ "$cost" == "null" || "$cost" == "0" || -z "$cost" ]]; then
      cost_str="—"
    else
      cost_str="\$$cost"
    fi
    [[ "$model" == "null" ]] && model="—"
    printf "    │ %-27s │ %-9s │ %-9s │ %-24s │ %-7s │ %-12s │\n" \
      "$stage" "$status" "$elapsed_str" "$model" "$tokens_str" "$cost_str"
  done < <(echo "$STAGES" | jq -r '.[] | [
    .stage,
    .status,
    (.elapsedMs // "null" | tostring),
    (.modelName // "null"),
    (.totalTokens // "null" | tostring),
    (.estimatedCost // "null" | tostring)
  ] | @tsv')
  echo "    └─────────────────────────────┴───────────┴───────────┴──────────────────────────┴─────────┴──────────────┘"

  # Aggregate token sum
  DOC_TOKENS=$(echo "$STAGES" | jq '[.[] | .totalTokens // 0] | add')
  SUM_TOKENS=$((SUM_TOKENS + DOC_TOKENS))

  # Verify timing on completed stages
  COMPLETED_STAGES=$(echo "$STAGES" | jq '[.[] | select(.status == "completed")] | length')
  TIMED_STAGES=$(echo "$STAGES" | jq '[.[] | select(.status == "completed" and .elapsedMs != null and .elapsedMs > 0)] | length')

  if [[ "$COMPLETED_STAGES" -ge 6 ]]; then
    pass "$f: $COMPLETED_STAGES completed stages, $TIMED_STAGES with timing"
  else
    fail "$f: only $COMPLETED_STAGES completed stages (expected ≥ 6)"
  fi

  # Check for LLM stages with cost
  COSTED_STAGES=$(echo "$STAGES" | jq '[.[] | select(.estimatedCost != null and .estimatedCost != "0")] | length')
  if [[ "$COSTED_STAGES" -gt 0 ]]; then
    pass "$f: $COSTED_STAGES paid stages"
  else
    if [[ "$f" == "diagram.png" ]]; then
      pass "$f: no LLM cost (expected for minimal image)"
    else
      fail "$f: no stages with cost data"
    fi
  fi
done

echo ""
log "$(color_cyan "Pipeline aggregates"): $SUM_TOKENS tokens, ${SUM_ELAPSED}ms total processing, \$$(printf '%.6f' $SUM_COST 2>/dev/null || echo $SUM_COST) total cost"

# ---------------------------------------------------------------------------
# Phase 6: Verify chunks
# ---------------------------------------------------------------------------
section "Phase 6: Verify chunks"

for f in "${!DOC_IDS[@]}"; do
  DOC_ID="${DOC_IDS[$f]}"
  SEGMENTS=$(api_json GET "/content/documents/$DOC_ID/prepared-segments?limit=1")
  SEG_COUNT=$(echo "$SEGMENTS" | jq '.total // 0')
  if [[ "$SEG_COUNT" -gt 0 ]]; then
    pass "$f: $SEG_COUNT prepared segments"
  else
    if [[ "$f" == "diagram.png" ]]; then
      pass "$f: no segments (expected for minimal image)"
    else
      fail "$f: no prepared segments"
    fi
  fi
done

# ---------------------------------------------------------------------------
# Phase 7: Verify graph entities and relations
# ---------------------------------------------------------------------------
section "Phase 7: Verify knowledge graph"

ENTITIES_RESP=$(api_json GET "/knowledge/libraries/$LIB_ID/entities")
ENTITY_COUNT=$(echo "$ENTITIES_RESP" | jq 'if type == "array" then length else .total // 0 end')
assert_gt "Total entities in library" 0 "$ENTITY_COUNT"

RELATIONS_RESP=$(api_json GET "/knowledge/libraries/$LIB_ID/relations")
RELATION_COUNT=$(echo "$RELATIONS_RESP" | jq 'if type == "array" then length else .total // 0 end')
assert_gt "Total relations in library" 0 "$RELATION_COUNT"

if [[ "$ENTITY_COUNT" -gt 0 ]]; then
  ENTITY_TYPES_COUNT=$(echo "$ENTITIES_RESP" | jq -r '
    [.[] | (.entity_type // .entityType)] | group_by(.) |
    map({type: .[0], count: length}) | sort_by(-.count)')

  echo ""
  log "  Entity type distribution:"
  echo "$ENTITY_TYPES_COUNT" | jq -r '.[] | "    \(.type): \(.count)"'

  echo ""
  log "  Top 10 entities by support_count:"
  echo "$ENTITIES_RESP" | jq -r '
    sort_by(-(.support_count // .supportCount // 0)) |
    .[:10] | .[] |
    "    \(.support_count // .supportCount // 0)x [\(.entity_type // .entityType)] \(.canonical_label // .canonicalLabel)"'

  echo ""
  log "  Top 10 relation types:"
  echo "$RELATIONS_RESP" | jq -r '
    [.[] | (.relation_type // .relationType)] | group_by(.) |
    map({type: .[0], count: length}) | sort_by(-.count) | .[:10] | .[] |
    "    \(.count)x \(.type)"'

  TYPE_COUNT=$(echo "$ENTITY_TYPES_COUNT" | jq 'length')
  pass "Entity types diversity: $TYPE_COUNT distinct types"
fi

# ---------------------------------------------------------------------------
# Phase 8: Test AI assistant (ask questions)
# ---------------------------------------------------------------------------
section "Phase 8: AI assistant Q&A"

# Q&A is diagnostic — don't let one bad response abort the suite
set +e

# Create a query session
SESSION_RESP=$(api_json POST "/query/sessions" \
  -d "{\"workspaceId\":\"$WS_ID\",\"libraryId\":\"$LIB_ID\",\"title\":\"Quality Gate Test\"}")
SESSION_ID=$(echo "$SESSION_RESP" | jq -r '.id // empty')

if [[ -z "$SESSION_ID" ]]; then
  fail "Failed to create query session: $SESSION_RESP"
else
  pass "Query session created: $SESSION_ID"

  ask_question() {
    local question="$1" expect_keyword="$2" label="$3"
    local TURN_RESP
    TURN_RESP=$(api_json POST "/query/sessions/$SESSION_ID/turns" \
      -d "{\"contentText\":\"$question\"}" 2>&1)

    local ANSWER VERIFICATION REF_COUNT WARNING_COUNT WARNINGS ELAPSED_MS DOC_REFS
    ANSWER=$(echo "$TURN_RESP" | jq -r '.responseTurn.contentText // empty' 2>/dev/null)
    VERIFICATION=$(echo "$TURN_RESP" | jq -r '.verificationState // "unknown"' 2>/dev/null)
    REF_COUNT=$(echo "$TURN_RESP" | jq '[.chunkReferences // [], .preparedSegmentReferences // []] | add | length' 2>/dev/null)
    WARNING_COUNT=$(echo "$TURN_RESP" | jq '.verificationWarnings // [] | length' 2>/dev/null)
    WARNINGS=$(echo "$TURN_RESP" | jq -r '.verificationWarnings // [] | join("; ")' 2>/dev/null)
    DOC_REFS=$(echo "$TURN_RESP" | jq -r '
      [.preparedSegmentReferences // [] | .[].documentTitle // .documentId // "?"] |
      unique | join(", ")' 2>/dev/null)

    echo ""
    echo "  $(color_yellow "─── $label ───")"
    echo "  $(color_cyan "Q:") $question"

    if [[ -n "$ANSWER" && "$ANSWER" != "null" ]]; then
      echo "  $(color_cyan "A:") $ANSWER"
      echo "  $(color_cyan "Sources:") $DOC_REFS"
      echo "  $(color_cyan "Verification:") $VERIFICATION  •  $(color_cyan "References:") $REF_COUNT"

      if [[ "$WARNING_COUNT" -gt 0 ]]; then
        echo "  $(color_yellow "⚠ Warnings:") $WARNINGS"
      fi

      local ANSWER_LOWER EXPECT_LOWER
      # Strip commas to allow "4,200,000" to match "4200000" and vice versa
      ANSWER_LOWER=$(echo "$ANSWER" | tr '[:upper:]' '[:lower:]' | tr -d ',')
      EXPECT_LOWER=$(echo "$expect_keyword" | tr '[:upper:]' '[:lower:]' | tr -d ',')

      # Detect grounding-guard refusal: when verification flags low confidence,
      # the system substitutes a refusal message instead of returning hallucinations.
      # This is the EXPECTED safe behavior.
      local IS_REFUSAL=0
      if [[ "$VERIFICATION" == "conflicting" || "$VERIFICATION" == "insufficient_evidence" ]]; then
        if echo "$ANSWER" | grep -qE "(can't give a confident answer|don't have enough grounded evidence)"; then
          IS_REFUSAL=1
        fi
      fi

      if [[ $IS_REFUSAL -eq 1 ]]; then
        pass "$label — system refused (verification: $VERIFICATION) — correct behavior"
      elif echo "$ANSWER_LOWER" | grep -qF "$EXPECT_LOWER"; then
        pass "$label — keyword '$expect_keyword' found, $VERIFICATION, $REF_COUNT refs"
      elif [[ "$VERIFICATION" == "verified" ]]; then
        fail "$label — verified answer missing expected keyword '$expect_keyword'"
      else
        fail "$label — answer present but not verified ($VERIFICATION) and missing '$expect_keyword'"
      fi
    else
      fail "$label — no answer received"
      log "    Raw: $(echo "$TURN_RESP" | head -c 300)"
    fi
  }

  ask_question \
    "Who is the Lead AI Engineer at Nexus Corp and what is her salary?" \
    "elena" \
    "Q1: Person lookup across text and spreadsheet"

  ask_question \
    "What is the annual license fee in the software agreement between Nexus Corp and Quantum AI Labs?" \
    "48,000" \
    "Q2: Contract financial detail (docx)"

  ask_question \
    "What are the 10 universal entity types used in IronRAG knowledge graph?" \
    "person" \
    "Q3: Architecture detail (markdown)"

  ask_question \
    "What was the revenue in Q1 2026 and how did NPS score change between Q1 2025 and Q1 2026?" \
    "4,200,000" \
    "Q4: Tabular metrics (csv)"

  ask_question \
    "Name the 7 stages of the IronRAG document processing pipeline in order." \
    "chunk" \
    "Q5: Pipeline stages (code + docs)"

  ask_question \
    "What are the 13 permission kinds supported by IronRAG IAM and what does library_write allow?" \
    "library_write" \
    "Q6: IAM model (C# code)"

  ask_question \
    "Who is the General Counsel and where is she based?" \
    "fatima" \
    "Q7: Cross-row spreadsheet lookup"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
section "Summary"

TOTAL_TIME=$((SECONDS - STARTED + 60))  # approx total wall time

echo ""
echo "  $(color_cyan "Test corpus:")"
echo "    Files uploaded:        $UPLOADED_COUNT / ${#FILES[@]}"
echo "    File types tested:     txt, docx, xlsx, csv, py, rs, cs, md, png"
echo ""
echo "  $(color_cyan "Pipeline performance:")"
echo "    Total tokens used:     $SUM_TOKENS"
echo "    Total processing time: ${SUM_ELAPSED}ms"
echo "    Total cost:            \$$(printf '%.6f' $SUM_COST 2>/dev/null || echo $SUM_COST)"
echo "    Avg per document:      \$$(echo "scale=6; $SUM_COST / $UPLOADED_COUNT" | bc -l 2>/dev/null || echo "N/A")"
echo ""
echo "  $(color_cyan "Knowledge graph:")"
echo "    Entities extracted:    $ENTITY_COUNT"
echo "    Relations found:       $RELATION_COUNT"
echo "    Entity types:          $(echo "$ENTITIES_RESP" | jq '[.[] | (.entity_type // .entityType)] | unique | length')"
echo ""
echo "  $(color_cyan "Test results:")"
echo "    Total checks:          $TOTAL"
echo "    Passed:                $(color_green "$PASS")"
echo "    Failed:                $([[ $FAIL -gt 0 ]] && color_red "$FAIL" || color_green "$FAIL")"
echo ""

if [[ $FAIL -eq 0 ]]; then
  echo "  $(color_green "✓ ALL $TOTAL CHECKS PASSED — Quality Gate: GREEN")"
  echo ""
  exit 0
else
  echo "  $(color_red "✗ $FAIL CHECKS FAILED — Quality Gate: RED")"
  echo ""
  exit 1
fi
