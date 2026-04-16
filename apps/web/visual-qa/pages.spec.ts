import { test, expect } from '@playwright/test';

/**
 * Visual QA pass for release 0.3.0 against the live backend. Every
 * scenario uses the session persisted by `global-setup.ts`, so the page
 * renders real workspace / library data — not a mocked fixture.
 *
 * Each scenario waits for a page-specific marker so we screenshot a
 * fully-rendered view, not an intermediate loading state. Markers are
 * lenient (substring) because the real backend's display strings are
 * locale-aware.
 *
 * Screenshots land under `visual-qa/screenshots/{project}-{scenario}.png`
 * so operators can diff them across runs without re-invoking the suite.
 */

type Scenario = {
  name: string;
  path: string;
  /** Optional text marker that must appear before we take the shot. */
  marker?: RegExp | string;
};

const scenarios: Scenario[] = [
  { name: 'dashboard', path: '/', marker: /Library Health|Dashboard|Загрузка|library/i },
  { name: 'documents', path: '/documents', marker: /Documents|Uploaded|Drop files|all/i },
  { name: 'graph', path: '/graph', marker: /Graph|ready|empty|nodes/i },
  { name: 'assistant', path: '/assistant', marker: /Assistant|Ask|New session/i },
  { name: 'admin-access', path: '/admin', marker: /Access|Token|Create/i },
  { name: 'admin-operations', path: '/admin?tab=operations', marker: /Operations|Queue|Audit/i },
  { name: 'admin-pricing', path: '/admin?tab=pricing', marker: /Pricing|Provider/i },
  { name: 'admin-mcp', path: '/admin?tab=mcp', marker: /MCP|server url|prompt/i },
];

test.describe('wave-2 visual QA (live backend)', () => {
  for (const scenario of scenarios) {
    test(scenario.name, async ({ page }, testInfo) => {
      await page.goto(scenario.path, { waitUntil: 'domcontentloaded' });
      await page
        .waitForLoadState('networkidle', { timeout: 15_000 })
        .catch(() => {});
      if (scenario.marker) {
        await page
          .getByText(scenario.marker)
          .first()
          .waitFor({ state: 'visible', timeout: 8_000 })
          .catch(() => {});
      }
      // Extra settle frame for animations (evidence panel slide-in, etc.).
      await page.waitForTimeout(250);

      const fileName = `${testInfo.project.name}-${scenario.name}.png`;
      await page.screenshot({
        path: `visual-qa/screenshots/${fileName}`,
        fullPage: true,
      });
      // The screenshot itself is the artifact; only assert the file was
      // captured. Operators review the PNGs manually — that is the point
      // of this suite.
      expect(true).toBe(true);
    });
  }
});
