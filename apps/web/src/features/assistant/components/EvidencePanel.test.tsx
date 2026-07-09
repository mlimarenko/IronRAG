import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import i18n from '@/shared/i18n';
import type { EvidenceBundle } from '@/shared/types';

import { EvidencePanel } from './EvidencePanel';

function evidenceWithLongSourceList(): EvidenceBundle {
  return {
    verificationState: 'passed',
    verificationWarnings: [],
    runtimeSummary: {
      totalSegments: 24,
      totalFacts: 0,
      totalEntities: 0,
      totalRelations: 0,
      policyInterventions: [],
      stages: [],
    },
    segmentRefs: Array.from({ length: 24 }, (_, index) => ({
      documentId: `document-${index}`,
      documentName: `document-${index}.md`,
      documentTitle: `Document ${index}`,
      sourceUri: null,
      sourceAccess: null,
      segmentOrdinal: index,
      excerpt: `Source excerpt ${index}`,
      relevance: 1 + index,
    })),
    factRefs: [],
    entityRefs: [],
    relationRefs: [],
  };
}

describe('EvidencePanel', () => {
  it('keeps long evidence lists in an internal scroll region', () => {
    const { container } = render(
      <EvidencePanel
        t={i18n.t.bind(i18n)}
        evidence={evidenceWithLongSourceList()}
        className="h-full"
        onOpenDocuments={vi.fn()}
        onOpenGraph={vi.fn()}
      />,
    );

    const panel = container.firstElementChild;
    const scrollRegion = screen.getByTestId('assistant-evidence-scroll');

    expect(panel).toHaveClass('flex', 'h-full', 'min-h-0', 'overflow-hidden');
    expect(scrollRegion).toHaveClass('min-h-0', 'flex-1', 'overflow-y-auto');
    expect(screen.getByText('Document 23')).toBeInTheDocument();
  });
});
