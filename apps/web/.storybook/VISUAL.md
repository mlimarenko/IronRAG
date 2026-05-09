# Storybook Visual Baselines

Storybook visual regression uses the existing Playwright runner. The visual suite is advisory and is not part of `frontend-check`.

Run from the repository root:

```bash
make frontend-visual
```

Run the same steps directly from `apps/web`:

```bash
npm run build-storybook
npx playwright test tests/visual
```

Update committed baselines after an intentional visual change:

```bash
npm run build-storybook
npx playwright test tests/visual --update-snapshots
```

Baselines live in `tests/visual/__screenshots__/`. The test reads Storybook's `index.json`, filters `src/**/*.stories.tsx` story entries, opens each `iframe.html?id=<story-id>` URL on `localhost:6006`, and compares a PNG screenshot against the matching committed baseline.

Expect legitimate baseline drift when component styling changes intentionally, when app fonts change, or when Tailwind theme tokens/global CSS change. Review the PNG diff before updating snapshots.
