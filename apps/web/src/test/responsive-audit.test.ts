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

const repoRoot = resolve(__dirname, '../..');

function readSrc(relativePath: string): string {
  return readFileSync(resolve(repoRoot, 'src', relativePath), 'utf8');
}

type ResponsiveExpectation = {
  file: string;
  mustContain: RegExp[];
};

const expectations: ResponsiveExpectation[] = [
  {
    file: 'pages/dashboard/SummaryCards.tsx',
    mustContain: [/sm:grid-cols-2/, /xl:grid-cols-4/],
  },
  {
    file: 'pages/dashboard/LibraryHealthPanel.tsx',
    mustContain: [/sm:p-6/, /sm:items-end/],
  },
  {
    file: 'pages/dashboard/RecentDocumentsList.tsx',
    mustContain: [/sm:p-6/, /xl:grid-cols-2/],
  },
  {
    file: 'pages/dashboard/AttentionPanel.tsx',
    mustContain: [/sm:p-6/],
  },
  {
    file: 'pages/dashboard/LatestIngestPanel.tsx',
    mustContain: [/sm:p-6/],
  },
  {
    file: 'pages/assistant/SessionRail.tsx',
    mustContain: [/md:w-64/],
  },
  {
    file: 'pages/assistant/EvidencePanel.tsx',
    mustContain: [/hidden lg:block/, /lg:w-80/],
  },
  {
    file: 'components/graph/GraphInspector.tsx',
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
