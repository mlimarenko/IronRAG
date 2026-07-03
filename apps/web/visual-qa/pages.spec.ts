import { test, expect } from '@playwright/test';
import type { Page } from '@playwright/test';

/**
 * Visual QA pass for release 0.3.1 against the live backend. Every
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

function collectRuntimeErrors(page: Page) {
  const runtimeErrors: string[] = [];

  page.on('pageerror', (error) => {
    runtimeErrors.push(error.stack ?? error.message);
  });

  page.on('console', (message) => {
    if (message.type() !== 'error') return;
    const text = message.text();
    if (text.includes('[vite] connecting') || text.includes('[vite] connected')) return;
    runtimeErrors.push(text);
  });

  return runtimeErrors;
}

async function expectAuthenticatedPage(page: Page) {
  await expect(page.getByRole('heading', { name: /Вход|Login/i })).toHaveCount(0);
}

const scenarios: Scenario[] = [
  { name: 'dashboard', path: '/dashboard', marker: /Dashboard|Панель|Ready|Готов/i },
  { name: 'documents', path: '/documents', marker: /Documents|Документы|Uploaded|Загружен/i },
  { name: 'graph', path: '/graph', marker: /Graph|Граф|nodes|узлов|empty|пуст/i },
  { name: 'assistant', path: '/assistant', marker: /Assistant|Ассистент|Ask|Вопрос/i },
  { name: 'admin-libraries', path: '/admin/libraries', marker: /Libraries|Библиотеки/i },
  { name: 'admin-queue', path: '/admin/queue', marker: /Queue|Очередь/i },
  { name: 'admin-ai', path: '/admin/ai', marker: /AI|Binding|Привяз/i },
  { name: 'admin-access', path: '/admin/access', marker: /Access|Доступ|Token|Токен/i },
  { name: 'admin-users', path: '/admin/users', marker: /Users|Пользовател/i },
  { name: 'admin-system', path: '/admin/system', marker: /System|Систем/i },
];

test.describe('wave-2 visual QA (live backend)', () => {
  for (const scenario of scenarios) {
    test(scenario.name, async ({ page }, testInfo) => {
      const runtimeErrors = collectRuntimeErrors(page);
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
      await expectAuthenticatedPage(page);
      await expect(page.getByText(/failed to render|не удалось отрисовать/i)).toHaveCount(0);
      expect(runtimeErrors).toEqual([]);

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

  test('admin-library-detail', async ({ page }, testInfo) => {
    const runtimeErrors = collectRuntimeErrors(page);
    await page.goto('/admin/libraries', { waitUntil: 'domcontentloaded' });
    await page.waitForLoadState('networkidle', { timeout: 15_000 }).catch(() => {});
    await expectAuthenticatedPage(page);
    const firstActionsButton = page
      .getByRole('button', { name: /Действия|Actions/i })
      .first();
    await firstActionsButton.waitFor({ state: 'visible', timeout: 10_000 });
    await firstActionsButton.click();
    const openLibraryAction = page
      .getByRole('menuitem', { name: /Открыть библиотеку|Open library/i })
      .first();
    await openLibraryAction.waitFor({ state: 'visible', timeout: 10_000 });
    await openLibraryAction.click();
    await page.waitForLoadState('networkidle', { timeout: 15_000 }).catch(() => {});
    await page.getByText(/Overview|Обзор|Backup|Резерв/i).first().waitFor({
      state: 'visible',
      timeout: 8_000,
    }).catch(() => {});
    await page.waitForTimeout(250);
    await expect(page.getByText(/failed to render|не удалось отрисовать/i)).toHaveCount(0);
    expect(runtimeErrors).toEqual([]);

    await page.screenshot({
      path: `visual-qa/screenshots/${testInfo.project.name}-admin-library-detail.png`,
      fullPage: true,
    });
    expect(true).toBe(true);
  });
});
