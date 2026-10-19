import { mapSourceAccess } from '@/shared/lib/source-access'
import type {
  AssistantConversationMessage,
  AssistantEvidenceBundle,
  AssistantExecutionDetail,
  AssistantSessionListItem,
} from '@/shared/api/generated'
import type { AssistantMessage, AssistantSession, EvidenceBundle } from '@/shared/types'

type AssistantEvidenceTransport = AssistantEvidenceBundle | AssistantExecutionDetail

type AssistantChunkCitation = {
  chunkId: string
  executionId: string
  rank: number
  score: number
  /** Absent when the chunk's document has been deleted since the answer. */
  documentId?: string
  documentTitle?: string
  sectionPath: string[]
  excerpt?: string
}

export type AssistantEvidenceView = EvidenceBundle & {
  chunkRefs: AssistantChunkCitation[]
  referenceSummary: {
    returnedCount: number
    totalCount: number
    truncated: boolean
  } | null
}

export function mapAssistantTurnToEvidence(
  resp: AssistantEvidenceTransport,
): AssistantEvidenceView {
  return {
    chunkRefs: resp.chunkReferences.map((reference) => ({
      chunkId: reference.chunkId,
      executionId: reference.executionId,
      rank: reference.rank,
      score: reference.score,
      ...(reference.documentId ? { documentId: reference.documentId } : {}),
      ...(reference.documentTitle ? { documentTitle: reference.documentTitle } : {}),
      sectionPath: reference.sectionPath ?? [],
      ...(reference.excerpt ? { excerpt: reference.excerpt } : {}),
    })),
    referenceSummary: resp.referenceSummary
      ? {
          returnedCount: resp.referenceSummary.returnedCount,
          totalCount: resp.referenceSummary.totalCount,
          truncated: resp.referenceSummary.truncated,
        }
      : null,
    segmentRefs: resp.preparedSegmentReferences.map((r) => {
      const trail = r.headingTrail
      const path = r.sectionPath
      return {
        documentId: r.documentId ?? r.segmentId,
        documentName:
          (trail.length > 0 ? trail.at(-1) : undefined) ??
          (path.join(' / ') || r.blockKind || 'Segment'),
        documentTitle: r.documentTitle ?? null,
        sourceUri: r.sourceAccess?.href ?? null,
        sourceAccess: mapSourceAccess(r.sourceAccess) ?? null,
        segmentOrdinal: r.rank ?? 0,
        excerpt: trail.join(' > ') || path.join(' > ') || '',
        relevance: r.score ?? 0,
      }
    }),
    factRefs: resp.technicalFactReferences.map((r) => ({
      factKind: r.factKind,
      value: r.displayValue || r['\u0063anonicalValue'],
      confidence: r.score ?? 0,
      documentName: '',
    })),
    entityRefs: resp.entityReferences.map((r) => ({
      entityId: r.nodeId,
      label: r.label,
      type: r.entityType ?? 'unknown',
      relevance: r.score ?? 0,
    })),
    relationRefs: resp.relationReferences.map((r) => ({
      sourceLabel: r.predicate,
      targetLabel: r.normalizedAssertion ?? '',
      relation: r.predicate,
      weight: r.score ?? 0,
    })),
    verificationState: resp.verificationState,
    verificationWarnings: resp.verificationWarnings.map((w) => w.message || w.code),
    answerDisposition: resp.answerDisposition ?? 'non_terminal',
    clarification: {
      required: resp.clarification?.required ?? false,
      question: resp.clarification?.question ?? null,
      answerCandidates: (resp.clarification?.answerCandidates ?? []).map((candidate) => ({
        label: candidate.label,
        kind: candidate.kind,
        confidence: candidate.confidence ?? null,
        provenance: {
          entityId: candidate.provenance.entityId ?? null,
          documentId: candidate.provenance.documentId ?? null,
          chunkId: candidate.provenance.chunkId ?? null,
        },
      })),
    },
    runtimeSummary: {
      totalSegments: resp.preparedSegmentReferences.length,
      totalFacts: resp.technicalFactReferences.length,
      totalEntities: resp.entityReferences.length,
      totalRelations: resp.relationReferences.length,
      stages: resp.runtimeStageSummaries.map((s) => ({
        stage: s.stageKind,
        durationMs: s.durationMs ?? 0,
      })),
    },
  }
}

export function mapAssistantSession(s: AssistantSessionListItem): AssistantSession {
  return {
    id: s.id,
    libraryId: s.libraryId,
    title: s.title || '',
    updatedAt: s.updatedAt,
    turnCount: s.turnCount ?? 0,
  }
}

export function mapAssistantMessage(m: AssistantConversationMessage): AssistantMessage {
  const acceptedAt = m.evidence?.runtimeSummary.acceptedAt
  const completedAt = m.evidence?.runtimeSummary.completedAt
  const acceptedMs = acceptedAt ? Date.parse(acceptedAt) : Number.NaN
  const completedMs = completedAt ? Date.parse(completedAt) : Number.NaN
  const durationMs =
    Number.isFinite(acceptedMs) && Number.isFinite(completedMs) && completedMs >= acceptedMs
      ? completedMs - acceptedMs
      : undefined

  return {
    id: m.id,
    role: m.role === 'user' ? 'user' : 'assistant',
    content: m.content ?? '',
    timestamp: m.timestamp,
    ...(m.elapsedMs != null ? { elapsedMs: m.elapsedMs } : {}),
    ...(durationMs !== undefined ? { durationMs } : {}),
    executionId: m.executionId ?? null,
    ...(m.evidence ? { evidence: mapAssistantTurnToEvidence(m.evidence) } : {}),
  }
}
