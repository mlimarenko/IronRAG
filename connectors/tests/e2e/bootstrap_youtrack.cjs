#!/usr/bin/env node

const { execFileSync } = require('node:child_process');
const { randomBytes } = require('node:crypto');
const {
  closeSync,
  constants,
  fchmodSync,
  fstatSync,
  fsyncSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} = require('node:fs');
const Module = require('node:module');
const path = require('node:path');

try {
  require.resolve('playwright');
} catch {
  process.env.NODE_PATH = execFileSync('npm', ['root', '-g'], { encoding: 'utf8' }).trim();
  Module.Module._initPaths();
}

const { chromium } = require('playwright');

const container = process.env.YOUTRACK_E2E_CONTAINER || 'ironrag-youtrack-e2e-youtrack-1';
const baseUrl = new URL(process.env.YOUTRACK_E2E_URL || 'http://127.0.0.1:18080');
const envPath = path.resolve(
  process.env.YOUTRACK_E2E_ENV_FILE || path.join(__dirname, '.env.local'),
);

if (!['127.0.0.1', 'localhost', '::1'].includes(baseUrl.hostname)) {
  throw new Error('Bootstrap is restricted to a loopback YouTrack URL');
}

function readLocalEnv() {
  let file;
  try {
    file = openSync(envPath, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  } catch (error) {
    if (error?.code === 'ENOENT') return {};
    throw error;
  }

  let contents;
  try {
    if (!fstatSync(file).isFile()) throw new Error('YouTrack E2E environment path is not a regular file');
    contents = readFileSync(file, 'utf8');
  } finally {
    closeSync(file);
  }

  const values = {};
  for (const line of contents.split(/\r?\n/)) {
    const match = line.match(/^([A-Z0-9_]+)='(.*)'$/);
    if (match) values[match[1]] = match[2].replace(/'\\''/g, "'");
  }
  return values;
}

function quoteEnv(value) {
  if (value.includes('\n') || value.includes('\r')) throw new Error('Invalid multiline secret');
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function saveLocalEnv({ password, token }) {
  const lines = [
    `YOUTRACK_E2E_URL=${quoteEnv(baseUrl.origin)}`,
    `YOUTRACK_E2E_ADMIN_LOGIN='admin'`,
    `YOUTRACK_E2E_ADMIN_PASSWORD=${quoteEnv(password)}`,
  ];
  if (token) lines.push(`YOUTRACK_E2E_TOKEN=${quoteEnv(token)}`);

  const temporaryPath = `${envPath}.${process.pid}.${randomBytes(4).toString('hex')}.tmp`;
  let file;
  try {
    file = openSync(
      temporaryPath,
      constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0),
      0o600,
    );
    writeFileSync(file, `${lines.join('\n')}\n`, 'utf8');
    fchmodSync(file, 0o600);
    fsyncSync(file);
    closeSync(file);
    file = undefined;
    renameSync(temporaryPath, envPath);
  } finally {
    if (file !== undefined) closeSync(file);
    try {
      unlinkSync(temporaryPath);
    } catch (error) {
      if (error?.code !== 'ENOENT') throw error;
    }
  }
}

function getWizardUrl() {
  const logs = execFileSync('docker', ['logs', container], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const match = logs.match(/wizard_token=([^\]\s&]+)/);
  if (!match) throw new Error('YouTrack wizard token was not found in container logs');
  const url = new URL(baseUrl.origin);
  url.searchParams.set('wizard_token', match[1]);
  return url;
}

async function clickWhenEnabled(locator, timeout = 30_000, action = 'wizard action') {
  await locator.waitFor({ state: 'visible', timeout });
  const deadline = Date.now() + timeout;
  while (await locator.isDisabled()) {
    if (Date.now() >= deadline) throw new Error(`Timed out waiting for enabled ${action}`);
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  await locator.click();
}

async function apiIsReady() {
  try {
    const response = await fetch(`${baseUrl.origin}/api/users/me?fields=id`);
    return [200, 401, 403].includes(response.status);
  } catch {
    return false;
  }
}

async function openWizardSetup(page, timeout = 300_000) {
  const deadline = Date.now() + timeout;
  const target = getWizardUrl();
  while (Date.now() < deadline) {
    await page.goto(target.toString(), { waitUntil: 'domcontentloaded', timeout: 120_000 });
    const setup = page.locator('[anchor-id="setupLink"]');
    const baseUrlInput = page.locator('[anchor-id="baseUrl"]');
    if (await setup.isVisible()) return true;
    if (await baseUrlInput.isVisible()) return true;

    const tokenInput = page.locator('input[name="token"]');
    if (await tokenInput.isVisible()) {
      const wizardToken = target.searchParams.get('wizard_token');
      if (!wizardToken) throw new Error('YouTrack wizard URL did not contain a token');
      await tokenInput.fill(wizardToken);
      await page.getByRole('button', { name: 'Log in' }).click();
      await page.waitForTimeout(1_000);
      continue;
    }
    if (await apiIsReady()) return false;
    await page.waitForTimeout(2_000);
  }
  throw new Error('YouTrack wizard did not return to a setup-ready state');
}

async function configureWizard(page, password) {
  if (!(await openWizardSetup(page))) return;

  const setup = page.locator('[anchor-id="setupLink"]');
  if (await setup.isVisible()) await setup.click();

  await page.locator('[anchor-id="baseUrl"]').fill(baseUrl.origin);
  await page.locator('[anchor-id="applicationListenPort"]').fill('8080');
  let next = page.locator('[anchor-id="nextButton"]');
  if (await next.isDisabled()) {
    const checkbox = page.locator('input[type="checkbox"]');
    if (await checkbox.count()) await checkbox.first().check();
  }
  await clickWhenEnabled(next, 30_000, 'settings Next');

  const builtInHub = page.locator('[anchor-id="useBuiltInHubTab"]');
  if (await builtInHub.isVisible()) await builtInHub.click();
  await page.waitForFunction(
    () => {
      const element = document.querySelector('[anchor-id="rootUserName"]');
      const controller = element && window.angular?.element(element).scope()?.ctrl;
      return Boolean(
        controller?.hubSettings?.$resolved &&
          controller?.defaultSettingsResource?.$resolved &&
          controller?.revertDefault?.model,
      );
    },
    undefined,
    { timeout: 60_000 },
  );
  await page.locator('[anchor-id="rootUserName"]').fill('admin');
  const passwordInput = page.locator('[anchor-id="rootUserPassword"]');
  const repeatInput = page.locator('[anchor-id="rootUserPasswordRepeat"]');
  await passwordInput.fill('');
  await repeatInput.fill('');
  await passwordInput.pressSequentially(password, { delay: 10 });
  await passwordInput.press('Tab');
  await page.waitForTimeout(300);
  await repeatInput.pressSequentially(password, { delay: 10 });
  await repeatInput.press('Tab');
  next = page.locator('[anchor-id="nextButton"]');
  await page.waitForTimeout(1_000);
  if (await next.isDisabled()) {
    const shape = await page
      .locator('[anchor-id="rootUserPassword"], [anchor-id="rootUserPasswordRepeat"]')
      .evaluateAll((nodes) => ({
        lengths: nodes.map((node) => node.value.length),
        equal: nodes.length === 2 && nodes[0].value === nodes[1].value,
      }));
    const messages = await page
      .locator('[role="alert"], [class*="error"], [class*="warning"]')
      .allTextContents();
    throw new Error(
      `Access validation failed (equal=${shape.equal}, lengths=${shape.lengths.join('/')}, messages=${messages
        .map((message) => message.trim())
        .filter(Boolean)
        .join('; ')})`,
    );
  }
  await clickWhenEnabled(next, 30_000, 'access Next');

  await page.waitForFunction(
    () => {
      const element = document.querySelector('[anchor-id="finishButton"]');
      const controller = element && window.angular?.element(element).scope()?.ctrl;
      return Boolean(
        controller?.license?.$resolved &&
          controller?.licenseDefault?.$resolved &&
          controller?.licenseDefaultForUpgrade?.$resolved &&
          controller?.revertDefault?.model &&
          controller.license.status === 'OK' &&
          controller.dataShouldChecked === false &&
          !controller.isSaving,
      );
    },
    undefined,
    { timeout: 60_000 },
  );
  const finish = page.locator('[anchor-id="finishButton"]');
  const dumpRequest = page.waitForRequest(
    (request) =>
      request.method() === 'POST' && new URL(request.url()).pathname.endsWith('/wizard/wait/dump'),
    { timeout: 60_000 },
  );
  const waitPage = page.waitForURL((url) => url.pathname.endsWith('/wait'), {
    timeout: 120_000,
  });
  await clickWhenEnabled(finish, 60_000, 'license Finish');
  await dumpRequest;
  await waitPage;
  console.log('YouTrack configuration wizard submitted.');
}

async function waitForApiReady(timeout = 120_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await apiIsReady()) return;
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  throw new Error('YouTrack API did not become ready after configuration');
}

async function ensureConfigured(browser, page, password) {
  let activeBrowser = browser;
  let activePage = page;
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    await configureWizard(activePage, password);
    try {
      await waitForApiReady();
      return { browser: activeBrowser, page: activePage };
    } catch (error) {
      if (attempt === 2) {
        await activeBrowser.close();
        throw error;
      }
      console.log('YouTrack wizard did not start the API; retrying the committed setup once.');
      await activeBrowser.close();
      activeBrowser = await chromium.launch({ headless: true });
      activePage = await activeBrowser.newPage();
    }
  }
  throw new Error('YouTrack configuration attempts were exhausted');
}

async function gotoWithRetry(page, url, timeout = 300_000) {
  const deadline = Date.now() + timeout;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const response = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 30_000 });
      if (response && response.ok()) return;
    } catch (error) {
      lastError = error;
    }
    await page.waitForTimeout(2_000);
  }
  throw lastError || new Error('YouTrack did not become ready');
}

async function createPermanentToken(page, password) {
  await gotoWithRetry(page, `${baseUrl.origin}/hub/users/me?tab=authentication`);

  const username = page.locator('[data-test="username-field"], #username').first();
  const loginButton = page.locator('[data-test="login-button"], #login-button');
  const newToken = page.locator('[data-test="new-token"]');
  const state = await Promise.race([
    username.waitFor({ state: 'visible', timeout: 120_000 }).then(() => 'login'),
    newToken.waitFor({ state: 'visible', timeout: 120_000 }).then(() => 'authenticated'),
  ]);
  if (state === 'login') {
    await username.fill('admin');
    await page.locator('[data-test="password-field"], #password').first().fill(password);
    await loginButton.first().click();
  }

  await newToken.waitFor({ state: 'visible', timeout: 120_000 });
  await newToken.click();

  const dialog = page.locator('[data-test="ring-dialog"]');
  await dialog.locator('#create-dialog__name, input[name="name"]').first().fill(`ironrag-e2e-${Date.now()}`);
  await dialog.locator('[data-test="ring-select__focus"]').click();
  const create = dialog
    .locator('[data-test="dialog-footer-button"]')
    .filter({ hasText: 'Create' })
    .first();
  await clickWhenEnabled(create, 30_000, 'token Create');

  const value = page.locator(
    '[data-test="ring-dialog"] .user-page__authentication__show-token__value',
  );
  const token = (await value.textContent({ timeout: 30_000 }))?.trim();
  if (!token || token.length < 16) throw new Error('YouTrack did not return a permanent token');

  const response = await fetch(`${baseUrl.origin}/api/users/me?fields=id,login,guest`, {
    headers: { Authorization: `Bearer ${token}`, Accept: 'application/json' },
  });
  const identity = response.ok ? await response.json() : null;
  if (!identity?.id || identity.guest === true) throw new Error('Created token failed authenticated validation');
  return token;
}

function sanitize(message, password) {
  const sanitized = password ? String(message).replaceAll(password, '<redacted>') : String(message);
  return sanitized
    .replace(/wizard_token=[^\s&]+/g, 'wizard_token=<redacted>')
    .replace(/perm:[A-Za-z0-9._~-]+/g, 'perm:<redacted>');
}

(async () => {
  const previous = readLocalEnv();
  const password = previous.YOUTRACK_E2E_ADMIN_PASSWORD || `YT_Aa1_${randomBytes(8).toString('hex')}`;
  saveLocalEnv({ password });

  let browser = await chromium.launch({ headless: true });
  let page = await browser.newPage();
  try {
    ({ browser, page } = await ensureConfigured(browser, page, password));
    const token = await createPermanentToken(page, password);
    saveLocalEnv({ password, token });
    console.log(`YouTrack E2E credentials written to ${path.relative(process.cwd(), envPath)} (mode 0600).`);
  } finally {
    await browser.close();
  }
})().catch((error) => {
  const password = readLocalEnv().YOUTRACK_E2E_ADMIN_PASSWORD || '';
  console.error(sanitize(error?.message || error, password));
  process.exitCode = 1;
});
