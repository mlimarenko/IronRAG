import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

/**
 * Static audit that every wave-2 extracted component still carries its
 * responsive breakpoint classes. This is not a full visual QA — the real
 * runtime check lives in `docs/RESPONSIVE_QA.md` — but it catches the
 * common "accidentally dropped the `sm:` / `md:` / `lg:` prefix while
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
    file: 'features/dashboard/components/SummaryCards.tsx',
    mustContain: [/sm:grid-cols-2/, /xl:grid-cols-4/],
  },
  {
    file: 'features/dashboard/components/LibraryHealthPanel.tsx',
    mustContain: [/sm:p-6/, /sm:items-end/],
  },
  {
    file: 'features/dashboard/components/RecentDocumentsList.tsx',
    mustContain: [/sm:p-6/, /xl:grid-cols-2/],
  },
  {
    file: 'features/dashboard/components/AttentionPanel.tsx',
    mustContain: [/sm:p-6/],
  },
  {
    file: 'features/dashboard/components/LatestIngestPanel.tsx',
    mustContain: [/sm:p-6/],
  },
  {
    file: 'features/assistant/components/SessionRail.tsx',
    mustContain: [/md:w-64/],
  },
  {
    file: 'features/assistant/components/EvidencePanel.tsx',
    mustContain: [/hidden lg:block/, /lg:w-80/],
  },
  {
    file: 'features/graph/components/GraphInspector.tsx',
    mustContain: [/w-\[24rem\]/, /lg:w-\[30rem\]/, /xl:w-\[34rem\]/],
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
