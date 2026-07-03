import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

/**
 * Static audit that every extracted dashboard component still carries its
 * responsive layout contract. This is not a full visual QA — the real
 * runtime check lives in `docs/RESPONSIVE_QA.md` — but it catches the
 * common "accidentally dropped the dense shell / breakpoint class while
 * reshuffling Tailwind classes" regression during refactors.
 */

const repoRoot = resolve(__dirname, '../../..');

function readSrc(relativePath: string): string {
  return readFileSync(resolve(repoRoot, 'src', relativePath), 'utf8');
}

type ResponsiveExpectation = {
  file: string;
  mustContain: RegExp[];
};

const expectations: ResponsiveExpectation[] = [
  {
    // Summary strip is 3 cards (graph-coverage dedup removed the 4th): 1-col
    // on mobile, 3-col from sm up.
    file: 'features/dashboard/components/SummaryCards.tsx',
    mustContain: [/sm:grid-cols-3/],
  },
  {
    file: 'features/dashboard/components/LibraryHealthPanel.tsx',
    mustContain: [/workbench-surface p-4/, /sm:items-end/],
  },
  {
    // `h-full` added so the panel fills its equal-height column wrapper.
    file: 'features/dashboard/components/RecentDocumentsList.tsx',
    mustContain: [/workbench-surface h-full p-4/, /xl:grid-cols-2/],
  },
  {
    file: 'features/dashboard/components/AttentionPanel.tsx',
    mustContain: [/workbench-surface p-4/],
  },
  {
    // `h-full` added so the panel fills its equal-height column wrapper.
    file: 'features/dashboard/components/LatestIngestPanel.tsx',
    mustContain: [/workbench-surface h-full p-4/],
  },
  {
    file: 'features/assistant/components/SessionRail.tsx',
    mustContain: [/w-12/, /w-64/, /aria-expanded/],
  },
  {
    // Assistant evidence now uses the shared DataView inspector contract
    // instead of page-local fixed/lg drawer classes.
    file: 'features/assistant/AssistantPage.tsx',
    mustContain: [/DataView/, /inspectorOpen=\{showEvidencePanel\}/, /showDrawerHeader=\{false\}/],
  },
  {
    file: 'features/graph/components/GraphInspector.tsx',
    mustContain: [/md:w-\[24rem\]/, /lg:w-\[28rem\]/, /xl:w-\[30rem\]/],
  },
];

describe('wave-2 responsive breakpoint audit', () => {
  for (const { file, mustContain } of expectations) {
    it(`${file} keeps its breakpoint classes`, () => {
      const source = readSrc(file);
      for (const pattern of mustContain) {
        expect(source, `${file} must still contain ${pattern}`).toMatch(pattern);
      }
    });
  }
});
