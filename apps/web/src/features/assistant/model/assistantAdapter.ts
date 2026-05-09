import { mapSourceAccess } from '@/shared/lib/source-access';
import { mapAssistantVerificationState } from '@/features/assistant/model/verification';
import type {
  AssistantConversationMessage,
  AssistantEvidenceBundle,
  AssistantExecutionDetail,
  AssistantSessionListItem,
} from '@/shared/api/generated';
import type {
  AssistantMessage,
  AssistantSession,
  EvidenceBundle,
} from '@/shared/types';

type AssistantEvidenceTransport = AssistantEvidenceBundle | AssistantExecutionDetail;

export function mapAssistantTurnToEvidence(
  resp: AssistantEvidenceTransport,
): EvidenceBundle {
  return {
    segmentRefs: resp.preparedSegmentReferences.map((r) => {
      const trail = r.headingTrail;
      const path = r.sectionPath;
      return {
        documentId: r.documentId ?? r.segmentId,
        documentName:
          trail.length > 0
            ? trail[trail.length - 1]
            : path.join(' / ') || r.blockKind || 'Segment',
        documentTitle: r.documentTitle ?? null,
        sourceUri: r.sourceUri ?? null,
        sourceAccess: mapSourceAccess(r.sourceAccess) ?? null,
        segmentOrdinal: r.rank ?? 0,
        excerpt: trail.join(' > ') || path.join(' > ') || '',
        relevance: r.score ?? 0,
      };
    }),
    factRefs: resp.technicalFactReferences.map((r) => ({
      factKind: r.factKind,
      value: r.displayValue || r.canonicalValue,
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
    verificationState: mapAssistantVerificationState(resp.verificationState),
    verificationWarnings: resp.verificationWarnings.map(
      (w) => w.message || w.code,
    ),
    runtimeSummary: {
      totalSegments: resp.preparedSegmentReferences.length,
      totalFacts: resp.technicalFactReferences.length,
      totalEntities: resp.entityReferences.length,
      totalRelations: resp.relationReferences.length,
      stages: resp.runtimeStageSummaries.map((s) => ({
        stage: s.stageKind,
        durationMs: 0,
        itemCount: 0,
      })),
      policyInterventions: [],
    },
  };
}

export function mapAssistantSession(s: AssistantSessionListItem): AssistantSession {
  return {
    id: s.id,
    libraryId: s.libraryId,
    title: s.title || '',
    updatedAt: s.updatedAt,
    turnCount: s.turnCount ?? 0,
  };
}

export function mapAssistantMessage(m: AssistantConversationMessage): AssistantMessage {
  return {
    id: m.id,
    role: m.role === 'user' ? 'user' : 'assistant',
    content: m.content ?? '',
    timestamp: m.timestamp,
    executionId: m.executionId ?? null,
    evidence: m.evidence ? mapAssistantTurnToEvidence(m.evidence) : undefined,
  };
}
