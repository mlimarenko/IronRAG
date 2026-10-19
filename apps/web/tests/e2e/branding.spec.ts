import { expect, test } from '@playwright/test'

import { openMockedApp, watchBrowserIssues } from './mock-app'

const displayName = 'Team Knowledge'
const longName = 'W'.repeat(120)

test('uses deployment branding in shell, footer, translations and browser metadata', async ({
  page,
}) => {
  const issues = watchBrowserIssues(page)
  await openMockedApp(page, '/dashboard', {
    authenticated: true,
    branding: {
      displayName,
      logoUrl: '/favicon.svg?brand=logo',
      iconUrl: '/favicon.svg?brand=icon',
      faviconUrl: '/favicon.svg?brand=favicon',
    },
  })
  await expect(page.locator('.dashboard-metrics')).toBeVisible()
  await expect(page).toHaveTitle(displayName)
  await expect(page.locator('.sidebar .brand')).toHaveAttribute('aria-label', displayName)
  await expect(page.locator('.sidebar .brand-mark')).toHaveAttribute(
    'src',
    '/favicon.svg?brand=logo',
  )
  await expect(page.locator('.shell-footer')).toContainText(`${displayName} v`)
  await expect(page.locator('link[rel="icon"]')).toHaveAttribute(
    'href',
    '/favicon.svg?brand=favicon',
  )
  await page.locator('.sidebar .collapse-button').click()
  await expect(page.locator('.sidebar .brand-mark')).toHaveAttribute(
    'src',
    '/favicon.svg?brand=icon',
  )

  await page.goto('/unrecognized-route?mocks=1')
  await expect(page.getByRole('heading', { level: 1 })).toContainText(displayName)
  await issues.assertClean()
})

test('uses deployment name and logo on the login screen', async ({ page }) => {
  const issues = watchBrowserIssues(page)
  await openMockedApp(page, '/login', {
    authenticated: false,
    branding: { displayName, logoUrl: '/favicon.svg?brand=logo' },
  })
  await expect(page.locator('.login-header .brand')).toHaveAttribute('aria-label', displayName)
  await expect(page.getByRole('heading', { level: 1, name: 'Sign in' })).toBeVisible()
  await expect(page.locator('.login-header .brand-mark')).toHaveAttribute(
    'src',
    '/favicon.svg?brand=logo',
  )
  await issues.assertClean()
})

test('truncates long deployment names without pushing the shell beyond its viewport', async ({
  page,
}) => {
  await page.setViewportSize({ width: 900, height: 1000 })
  await openMockedApp(page, '/dashboard', {
    authenticated: true,
    branding: { displayName: longName },
  })
  await expect(page.locator('.dashboard-metrics')).toBeVisible()
  const label = page.locator('.sidebar .brand-type')
  await expect(label).toHaveAttribute('title', longName)
  const boundaries = await label.evaluate((element) => ({
    labelRight: element.getBoundingClientRect().right,
    containerRight: element.closest('.sidebar-brand')!.getBoundingClientRect().right,
    documentWidth: document.documentElement.scrollWidth,
    viewportWidth: document.documentElement.clientWidth,
  }))
  expect(boundaries.labelRight).toBeLessThanOrEqual(boundaries.containerRight)
  expect(boundaries.documentWidth).toBeLessThanOrEqual(boundaries.viewportWidth)

  await page.setViewportSize({ width: 390, height: 844 })
  await page.locator('.topbar .mobile-menu').click()
  await expect(page.locator('.mobile-drawer')).toBeVisible()
  const drawer = await page.locator('.mobile-drawer').evaluate((element) => ({
    labelRight: element.querySelector('.brand-type')!.getBoundingClientRect().right,
    closeLeft: element.querySelector(':scope > .button')!.getBoundingClientRect().left,
  }))
  expect(drawer.labelRight).toBeLessThanOrEqual(drawer.closeLeft - 8)
  await page.keyboard.press('Escape')
  await page.goto('/unrecognized-route?mocks=1')
  await expect(page.getByRole('heading', { level: 1 })).toContainText(longName)
  for (const width of [390, 1440]) {
    await page.setViewportSize({ width, height: 1000 })
    const content = await page.locator('.page-content').evaluate((element) => ({
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
    }))
    expect(content.scrollWidth).toBeLessThanOrEqual(content.clientWidth)
  }
})

test('keeps a long deployment name inside the login form on desktop and mobile', async ({
  page,
}) => {
  await openMockedApp(page, '/login', {
    authenticated: false,
    branding: { displayName: longName },
  })
  await expect(page.getByRole('heading', { level: 1, name: 'Sign in' })).toBeVisible()
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 900 })
    const content = await page.evaluate(() => ({
      clientWidth: document.documentElement.clientWidth,
      scrollWidth: document.documentElement.scrollWidth,
    }))
    expect(content.scrollWidth).toBeLessThanOrEqual(content.clientWidth)
  }
})

test('rejects invalid branding without rendering the application or raw configuration', async ({
  page,
}) => {
  const pageErrors: string[] = []
  page.on('pageerror', (error) => pageErrors.push(error.message))
  await openMockedApp(page, '/login', {
    branding: { displayName, logoUrl: 'javascript:alert(1)' },
  })
  await expect(page.getByRole('heading', { name: 'Something went wrong.' })).toBeVisible()
  await expect(page.locator('.app-shell, .login-form')).toHaveCount(0)
  await expect(page.getByText('javascript:alert(1)', { exact: false })).toHaveCount(0)
  expect(pageErrors).toEqual([])
})
